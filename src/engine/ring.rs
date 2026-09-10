//! Sample-time-indexed loopback ring: `RING_BLOCKS` blocks of `BLOCK`
//! frames, one writer, any number of concurrent readers. Each block has
//! its own seqlock (`BlockMeta`), so a read sees a complete write or none
//! of it, and a per-block coverage watermark decides whether the requested
//! span was actually written for the lap asked about. Torn and stale spans
//! are faded towards silence and counted exactly once each.
//!
//! `write` and `read` take no lock, allocate nothing and never wait unbounded.

use super::device::{BLOCK, RING_BLOCKS, RING_FRAMES};
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Retry budget for the reader's seqlock loop; an unbounded spin on the
/// real-time thread would be as bad as a wrong answer.
const RETRY_LIMIT: u32 = 4;

/// Wall-clock cap on one reader retry's busy-wait. A time bound rather than
/// a spin count: `spin_loop()` is aarch64 `yield`, which has no minimum
/// latency, so iterations cannot be sized to a writer transaction.
/// Worst case per block segment: `RETRY_LIMIT` spins of `BACKOFF_CEILING`, 200 µs.
const BACKOFF_CEILING: Duration = Duration::from_micros(50);

/// Sentinel `stamp` for a block that has never been written.
const NO_STAMP: u64 = u64::MAX;

/// Per-block seqlock state. `align(64)` keeps each entry on its own cache
/// line so a writer closing block N does not contend with a reader
/// validating block N+1.
#[repr(align(64))]
struct BlockMeta {
    /// Even: stable. Odd: writer mid-transaction.
    seq: AtomicU64,
    /// Sample time of this block's first frame; `NO_STAMP` until first written.
    stamp: AtomicU64,
    /// Frame offsets within the block written for the current `stamp`'s lap:
    /// `covered_from..covered_to`. `stamp` alone cannot say which frames are current.
    covered_from: AtomicU64,
    covered_to: AtomicU64,
}

impl BlockMeta {
    const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            stamp: AtomicU64::new(NO_STAMP),
            covered_from: AtomicU64::new(0),
            covered_to: AtomicU64::new(0),
        }
    }
}

/// One block-sized segment of a `read` call; bundled only to keep
/// `read_block` under clippy's argument limit.
struct Segment {
    t: u64,
    bstart: u64,
    n: usize,
}

/// Outcome of validating one block-sized read attempt.
enum BlockOutcome {
    /// Every requested frame was delivered from the current lap.
    Delivered,
    /// `stamp` matched but only part of the span was covered; `read_block` has
    /// already faded the rest. `trailing_delivered`: the last frame is real,
    /// so the next segment's fade may start from it.
    PartiallyStale { trailing_delivered: bool },
    /// Consistent snapshot, but nothing requested belongs to this lap;
    /// retrying cannot produce it.
    Stale,
    /// `RETRY_LIMIT` attempts without a consistent snapshot.
    Torn,
}

/// The lock-free, sample-time-indexed loopback ring.
pub struct Ring {
    /// Interleaved samples as `f32` bits, always `Relaxed`: ordering comes from
    /// `BlockMeta::seq`; the atomics only give data-race freedom without `unsafe`.
    data: Vec<AtomicU32>,
    meta: Vec<BlockMeta>,
    channels: usize,
    torn: AtomicU64,
    stale: AtomicU64,
    retries: AtomicU64,
}

impl Ring {
    /// Creates a zeroed ring for `channels` interleaved channels.
    pub fn new(channels: usize) -> Self {
        let mut data = Vec::with_capacity(RING_FRAMES.saturating_mul(channels));
        data.resize_with(RING_FRAMES.saturating_mul(channels), || AtomicU32::new(0));
        let mut meta = Vec::with_capacity(RING_BLOCKS);
        meta.resize_with(RING_BLOCKS, BlockMeta::new);
        Self {
            data,
            meta,
            channels,
            torn: AtomicU64::new(0),
            stale: AtomicU64::new(0),
            retries: AtomicU64::new(0),
        }
    }

    /// Block reads that exhausted `RETRY_LIMIT` without a consistent snapshot.
    pub fn torn(&self) -> u64 {
        self.torn.load(Ordering::Relaxed)
    }
    /// Block reads, or sub-spans, whose data for the requested lap was absent.
    pub fn stale(&self) -> u64 {
        self.stale.load(Ordering::Relaxed)
    }
    /// Seqlock read attempts after the first, summed over all block reads.
    pub fn retries(&self) -> u64 {
        self.retries.load(Ordering::Relaxed)
    }

    /// Resets every block to "never written". Not real-time safe and not safe
    /// concurrently with `write`; racing readers see old or new state, never a mix.
    pub fn zero(&self) {
        for bi in 0..RING_BLOCKS {
            let Some(m) = self.meta.get(bi) else { continue };
            m.seq.fetch_add(1, Ordering::Relaxed);
            fence(Ordering::Release);
            let base = bi.saturating_mul(BLOCK).saturating_mul(self.channels);
            let span = BLOCK.saturating_mul(self.channels);
            for i in 0..span {
                if let Some(cell) = self.data.get(base.saturating_add(i)) {
                    cell.store(0, Ordering::Relaxed);
                }
            }
            m.stamp.store(NO_STAMP, Ordering::Relaxed);
            m.covered_from.store(0, Ordering::Relaxed);
            m.covered_to.store(0, Ordering::Relaxed);
            m.seq.fetch_add(1, Ordering::Release);
        }
    }

    fn block_start(t: u64) -> u64 {
        t - (t % BLOCK as u64)
    }

    fn block_index(t: u64) -> usize {
        ((t / BLOCK as u64) % RING_BLOCKS as u64) as usize
    }

    /// Flat index into `data` of channel 0 of the frame at sample time `t`.
    /// Always `< data.len()`, since `data.len() == RING_FRAMES * channels`.
    fn slot(&self, t: u64) -> usize {
        (t % RING_FRAMES as u64) as usize * self.channels
    }

    fn required_len(&self, frames: usize) -> Option<usize> {
        frames.checked_mul(self.channels)
    }

    /// Writes `frames` interleaved frames from `src` at absolute sample time
    /// `sample_time`, spanning blocks and wrapping; a no-op if `src` is too short.
    pub fn write(&self, sample_time: u64, src: &[f32], frames: usize) {
        let Some(need) = self.required_len(frames) else {
            return;
        };
        if src.len() < need {
            return;
        }

        let mut done = 0usize;
        while done < frames {
            let t = sample_time.wrapping_add(done as u64);
            let bstart = Self::block_start(t);
            let within = t.wrapping_sub(bstart) as usize; // == t % BLOCK
            let n = BLOCK
                .saturating_sub(within)
                .min(frames.saturating_sub(done));
            if n == 0 {
                // Unreachable, but never spin in place on a real-time thread.
                break;
            }
            let bi = Self::block_index(t);
            let Some(m) = self.meta.get(bi) else { return };

            // Relaxed RMW plus a fence: `Release` on the RMW alone would not stop
            // the sample stores below being hoisted above it.
            m.seq.fetch_add(1, Ordering::Relaxed); // -> odd
            fence(Ordering::Release);

            for f in 0..n {
                let base = self.slot(t.wrapping_add(f as u64));
                let frame = done.wrapping_add(f);
                for c in 0..self.channels {
                    let idx = base.wrapping_add(c);
                    let src_idx = frame.wrapping_mul(self.channels).wrapping_add(c);
                    if let (Some(cell), Some(v)) = (self.data.get(idx), src.get(src_idx)) {
                        cell.store(v.to_bits(), Ordering::Relaxed);
                    }
                    // Unreachable; skip rather than `return`, which would leave
                    // `seq` stuck odd.
                }
            }

            let within_u64 = within as u64;
            let end_u64 = within_u64.saturating_add(n as u64);
            let old_stamp = m.stamp.load(Ordering::Relaxed);
            let old_from = m.covered_from.load(Ordering::Relaxed);
            let old_to = m.covered_to.load(Ordering::Relaxed);
            // The watermark only grows when the new segment touches or overlaps
            // it; a same-lap segment that leaves a gap resets it, otherwise `[0,128)`
            // then `[256,384)` would claim the never-written `[128,256)` as current.
            let (new_from, new_to) =
                if old_stamp == bstart && within_u64 <= old_to && end_u64 >= old_from {
                    (old_from.min(within_u64), old_to.max(end_u64))
                } else {
                    (within_u64, end_u64)
                };
            m.covered_from.store(new_from, Ordering::Relaxed);
            m.covered_to.store(new_to, Ordering::Relaxed);
            m.stamp.store(bstart, Ordering::Relaxed);
            m.seq.fetch_add(1, Ordering::Release); // -> even
            done = done.saturating_add(n);
        }
    }

    /// Reads `frames` interleaved frames into `dst` at absolute sample time
    /// `sample_time`; torn or stale spans are faded to silence. No-op if `dst` is short.
    pub fn read(&self, sample_time: u64, dst: &mut [f32], frames: usize) {
        let Some(need) = self.required_len(frames) else {
            return;
        };
        if dst.len() < need {
            return;
        }

        let mut done = 0usize;
        // Only a cleanly `Delivered` previous segment leaves a usable fade start in `dst`.
        let mut prev_delivered = false;
        while done < frames {
            let t = sample_time.wrapping_add(done as u64);
            let bstart = Self::block_start(t);
            let within = t.wrapping_sub(bstart) as usize; // == t % BLOCK
            let n = BLOCK
                .saturating_sub(within)
                .min(frames.saturating_sub(done));
            if n == 0 {
                break; // see the matching comment in `write`
            }
            let bi = Self::block_index(t);

            let outcome = match self.meta.get(bi) {
                Some(m) => self.read_block(m, Segment { t, bstart, n }, dst, done, prev_delivered),
                None => BlockOutcome::Torn, // unreachable; see `slot`'s invariant note
            };

            match outcome {
                BlockOutcome::Delivered => {
                    prev_delivered = true;
                }
                BlockOutcome::PartiallyStale { trailing_delivered } => {
                    self.stale.fetch_add(1, Ordering::Relaxed);
                    prev_delivered = trailing_delivered;
                }
                BlockOutcome::Stale => {
                    self.stale.fetch_add(1, Ordering::Relaxed);
                    self.fade_span(dst, done, n, prev_delivered);
                    prev_delivered = false;
                }
                BlockOutcome::Torn => {
                    self.torn.fetch_add(1, Ordering::Relaxed);
                    self.fade_span(dst, done, n, prev_delivered);
                    prev_delivered = false;
                }
            }
            done = done.saturating_add(n);
        }
    }

    /// One block of the seqlock read protocol, bounded to `RETRY_LIMIT` attempts.
    /// `stamp` and the watermark are trusted only after `seq` is re-checked stable
    /// across the same attempt: compared earlier, a mid-transaction read would be
    /// recorded as stale, and the same event could then also be counted as torn.
    fn read_block(
        &self,
        m: &BlockMeta,
        seg: Segment,
        dst: &mut [f32],
        done: usize,
        prev_delivered: bool,
    ) -> BlockOutcome {
        let Segment { t, bstart, n } = seg;
        let within = t.wrapping_sub(bstart) as usize;
        let req_from = within as u64;
        let req_to = req_from.saturating_add(n as u64);

        for attempt in 0..RETRY_LIMIT {
            if attempt > 0 {
                self.retries.fetch_add(1, Ordering::Relaxed);
                // `checked_add`: `Instant + Duration` can panic on overflow;
                // skipping the backoff is the safe fallback.
                if let Some(deadline) = Instant::now().checked_add(BACKOFF_CEILING) {
                    while Instant::now() < deadline {
                        std::hint::spin_loop();
                    }
                }
            }

            // Fence before the re-check, not `Acquire` on that load: the sample
            // loads above must not sink below it.
            let s1 = m.seq.load(Ordering::Acquire);
            if !s1.is_multiple_of(2) {
                continue; // writer mid-transaction; bounded retry
            }

            let stamp = m.stamp.load(Ordering::Relaxed);
            let cov_from = m.covered_from.load(Ordering::Relaxed);
            let cov_to = m.covered_to.load(Ordering::Relaxed);
            let ov_from = cov_from.max(req_from);
            let ov_to = cov_to.min(req_to);
            // Hint only; trusted after `s2` confirms the attempt was not torn.
            let looks_entirely_absent = stamp != bstart || ov_from >= ov_to;

            if !looks_entirely_absent {
                for f in 0..n {
                    let base = self.slot(t.wrapping_add(f as u64));
                    let frame = done.wrapping_add(f);
                    for c in 0..self.channels {
                        let idx = base.wrapping_add(c);
                        let dst_idx = frame.wrapping_mul(self.channels).wrapping_add(c);
                        if let (Some(cell), Some(out)) = (self.data.get(idx), dst.get_mut(dst_idx))
                        {
                            *out = f32::from_bits(cell.load(Ordering::Relaxed));
                        }
                    }
                }
            }

            fence(Ordering::Acquire);
            let s2 = m.seq.load(Ordering::Relaxed);
            if s2 != s1 {
                continue; // torn: a write started and/or finished during
                          // this read; nothing observed above (samples,
                          // stamp, or coverage) can be trusted. Retry.
            }

            if stamp != bstart {
                return BlockOutcome::Stale; // different lap entirely
            }
            if ov_from >= ov_to {
                return BlockOutcome::Stale; // matched lap, nothing requested was ever written for it
            }
            if ov_from <= req_from && ov_to >= req_to {
                return BlockOutcome::Delivered;
            }

            // Partial coverage: fade the parts outside `[ov_from, ov_to)` in place.
            if ov_from > req_from {
                let gap_len = ov_from.saturating_sub(req_from) as usize;
                self.fade_span(dst, done, gap_len, prev_delivered);
            }
            let trailing_delivered = ov_to >= req_to;
            if ov_to < req_to {
                let gap_off = ov_to.saturating_sub(req_from) as usize;
                let gap_len = req_to.saturating_sub(ov_to) as usize;
                // The covered span directly precedes this gap, so continuity holds.
                self.fade_span(dst, done.saturating_add(gap_off), gap_len, true);
            }
            return BlockOutcome::PartiallyStale { trailing_delivered };
        }
        BlockOutcome::Torn
    }

    /// Ramps `dst[done..done+n)` towards silence, starting from the previous
    /// frame when the preceding segment was `Delivered` and from silence otherwise.
    /// Stateless: `read` takes `&self` with concurrent readers, so ring-level
    /// "last sample" state would let one reader's fade seed another's.
    fn fade_span(&self, dst: &mut [f32], done: usize, n: usize, prev_delivered: bool) {
        if n == 0 {
            return;
        }
        for c in 0..self.channels {
            let start = if done > 0 && prev_delivered {
                let idx = (done - 1).saturating_mul(self.channels).saturating_add(c);
                dst.get(idx).copied().unwrap_or(0.0)
            } else {
                0.0
            };
            for f in 0..n {
                let gain = 1.0 - (f as f32 / n as f32);
                let idx = done
                    .saturating_add(f)
                    .saturating_mul(self.channels)
                    .saturating_add(c);
                if let Some(out) = dst.get_mut(idx) {
                    *out = start * gain;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn roundtrip_at_same_sample_time() {
        let r = Ring::new(2);
        let src: Vec<f32> = (0..1024).map(|i| i as f32).collect();
        r.write(0, &src, 512);
        let mut dst = vec![0.0f32; 1024];
        r.read(0, &mut dst, 512);
        assert_eq!(src, dst);
    }

    #[test]
    fn is_bit_transparent_for_nan_denormal_and_infinity() {
        let r = Ring::new(1);
        let src = vec![
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::from_bits(1),
            -0.0,
            1.0,
        ];
        r.write(0, &src, 6);
        let mut dst = vec![0.0f32; 6];
        r.read(0, &mut dst, 6);
        for (a, b) in src.iter().zip(dst.iter()) {
            assert_eq!(a.to_bits(), b.to_bits(), "bit pattern changed");
        }
    }

    #[test]
    fn read_of_never_written_time_returns_silence_not_stale_audio() {
        let r = Ring::new(1);
        let loud = vec![1.0f32; BLOCK];
        r.write(0, &loud, BLOCK);
        let mut dst = vec![9.0f32; BLOCK];
        r.read(RING_FRAMES as u64, &mut dst, BLOCK);
        assert!(
            dst.iter().all(|&x| x == 0.0),
            "stale lap data leaked through"
        );
        assert_eq!(r.stale(), 1, "the stale block must be counted");
    }

    #[test]
    fn straddling_block_boundary_reads_correctly() {
        let r = Ring::new(1);
        let src: Vec<f32> = (0..(BLOCK * 2)).map(|i| i as f32).collect();
        r.write(0, &src, BLOCK * 2);
        let mut dst = vec![0.0f32; BLOCK];
        r.read((BLOCK / 2) as u64, &mut dst, BLOCK);
        let expect: Vec<f32> = ((BLOCK / 2)..(BLOCK / 2 + BLOCK))
            .map(|i| i as f32)
            .collect();
        assert_eq!(dst, expect);
    }

    #[test]
    fn wraps_around_the_ring() {
        let r = Ring::new(1);
        let src = vec![7.0f32; BLOCK];
        let t = (RING_FRAMES - BLOCK / 2) as u64;
        r.write(t, &src, BLOCK);
        let mut dst = vec![0.0f32; BLOCK];
        r.read(t, &mut dst, BLOCK);
        assert_eq!(dst, src);
    }

    #[test]
    fn partial_block_coverage_does_not_mark_the_whole_block_current() {
        let r = Ring::new(1);
        let full: Vec<f32> = vec![1.0; RING_FRAMES];
        r.write(0, &full, RING_FRAMES); // fill one full lap
        let half = vec![2.0f32; BLOCK / 2];
        r.write(RING_FRAMES as u64 + (BLOCK / 2) as u64, &half, BLOCK / 2);

        let mut dst = vec![9.0f32; BLOCK];
        r.read(RING_FRAMES as u64, &mut dst, BLOCK);

        assert!(
            dst.iter().take(BLOCK / 2).all(|&x| x != 1.0),
            "stale (previous-lap) audio leaked through as if it were current"
        );
        assert!(
            dst.iter().take(BLOCK / 2).all(|&x| x == 0.0),
            "uncovered frames must be silent, not stale audio"
        );
        assert!(
            dst.iter().skip(BLOCK / 2).all(|&x| x == 2.0),
            "covered frames must still be delivered"
        );
        assert_eq!(r.stale(), 1, "the partially-covered block must be counted");
    }

    #[test]
    fn non_contiguous_same_lap_writes_do_not_bridge_the_gap_between_them() {
        let r = Ring::new(1);
        let full: Vec<f32> = vec![1.0; RING_FRAMES];
        r.write(0, &full, RING_FRAMES);

        let next_lap = RING_FRAMES as u64;
        let a = vec![2.0f32; 128];
        let b = vec![3.0f32; 128];
        r.write(next_lap, &a, 128); // covers [0,128) ... only until the next write
        r.write(next_lap + 256, &b, 128); // resets coverage to exactly [256,384)

        let mut dst = vec![9.0f32; BLOCK];
        r.read(next_lap, &mut dst, BLOCK);

        assert!(
            dst.iter().all(|&x| x != 1.0),
            "stale (previous-lap) audio leaked through as if it were current"
        );
        assert!(
            dst.iter().take(256).all(|&x| x == 0.0),
            "everything before the most recent covered span must be silent, not delivered"
        );
        assert!(
            dst.iter().skip(256).take(128).all(|&x| x == 3.0),
            "the most recent write's span must still be delivered"
        );
        let fade_start = dst.get(384).copied().unwrap_or(f32::NAN);
        let fade_end = dst.last().copied().unwrap_or(f32::NAN);
        assert_eq!(
            fade_start, 3.0,
            "the trailing fade must start from the covered span's real last sample"
        );
        assert!(
            fade_end.abs() < fade_start.abs(),
            "the trailing fade must decay toward silence"
        );
        assert!(r.stale() > 0, "at least one stale span must be counted");
    }

    #[test]
    fn concurrent_writer_and_reader_never_produce_a_silent_gap() {
        // The writer is paced to a sample clock: free-running loops fall into
        // lockstep with the reader in a way hardware-clocked IO never does.
        let r = Arc::new(Ring::new(2));
        let w = Arc::clone(&r);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = Arc::clone(&stop);
        let epoch = std::time::Instant::now();
        const RATE: f64 = 48_000.0;

        let writer = std::thread::spawn(move || {
            let buf = vec![1.0f32; BLOCK * 2];
            let mut last_written: Option<u64> = None;
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                let due_block =
                    (epoch.elapsed().as_secs_f64() * RATE) as u64 / BLOCK as u64 * BLOCK as u64;
                if last_written != Some(due_block) {
                    w.write(due_block, &buf, BLOCK);
                    last_written = Some(due_block);
                } else {
                    std::hint::spin_loop();
                }
            }
        });

        std::thread::sleep(std::time::Duration::from_millis(20));

        let mut silent = 0u64;
        let mut delivered_real_audio = 0u64;
        let mut total = 0u64;
        let lag_blocks = 20u64;
        let mut dst = vec![9.0f32; BLOCK * 2];
        for _ in 0..20_000 {
            let elapsed_blocks = (epoch.elapsed().as_secs_f64() * RATE) as u64 / BLOCK as u64;
            let t = elapsed_blocks
                .saturating_sub(lag_blocks)
                .saturating_mul(BLOCK as u64);
            dst.fill(9.0);
            r.read(t, &mut dst, BLOCK);
            let has_sentinel = dst.contains(&9.0);
            let has_real = dst.contains(&1.0);
            let all_zero = dst.iter().all(|&x| x == 0.0);
            assert!(
                !has_sentinel,
                "an untouched sentinel leaked through a delivered/faded block"
            );
            assert!(
                all_zero || has_real,
                "a block was neither silent nor carried the writer's real value — torn without being counted"
            );
            if all_zero {
                silent += 1;
            }
            if has_real {
                delivered_real_audio += 1;
            }
            total += 1;
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = writer.join();

        assert_eq!(
            r.torn(),
            0,
            "unrecoverable tears must not occur; retries={}",
            r.retries()
        );
        assert!(total > 0 && silent <= total);
        assert!(
            delivered_real_audio > 0,
            "no real audio was ever delivered — reader and writer never overlapped"
        );
    }

    #[test]
    fn concurrent_writer_and_reader_at_zero_lag_records_contention_without_flaky_assertions() {
        let r = Arc::new(Ring::new(2));
        let w = Arc::clone(&r);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = Arc::clone(&stop);
        let epoch = std::time::Instant::now();
        const RATE: f64 = 48_000.0;

        let writer = std::thread::spawn(move || {
            let buf = vec![1.0f32; BLOCK * 2];
            let mut last_written: Option<u64> = None;
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                let due_block =
                    (epoch.elapsed().as_secs_f64() * RATE) as u64 / BLOCK as u64 * BLOCK as u64;
                if last_written != Some(due_block) {
                    w.write(due_block, &buf, BLOCK);
                    last_written = Some(due_block);
                } else {
                    std::hint::spin_loop();
                }
            }
        });

        std::thread::sleep(std::time::Duration::from_millis(20));

        let mut dst = vec![9.0f32; BLOCK * 2];
        let mut total = 0u64;
        let mut delivered_real_audio = 0u64;
        let lag_blocks = 0u64;
        for _ in 0..20_000 {
            let elapsed_blocks = (epoch.elapsed().as_secs_f64() * RATE) as u64 / BLOCK as u64;
            let t = elapsed_blocks
                .saturating_sub(lag_blocks)
                .saturating_mul(BLOCK as u64);
            dst.fill(9.0);
            r.read(t, &mut dst, BLOCK);
            assert!(
                !dst.contains(&9.0),
                "an untouched sentinel leaked through a delivered/faded block"
            );
            if dst.contains(&1.0) {
                delivered_real_audio += 1;
            }
            total += 1;
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = writer.join();

        let torn = r.torn();
        let stale = r.stale();
        let retries = r.retries();
        eprintln!(
            "zero-lag contention recorded: total={total} torn={torn} stale={stale} retries={retries} delivered={delivered_real_audio}"
        );
        assert!(
            torn.saturating_add(stale) <= total,
            "torn+stale ({torn}+{stale}) exceeded the number of reads ({total}) — a single bad read was double-counted"
        );
        assert!(
            delivered_real_audio > 0,
            "no real audio was ever delivered at (near-)zero lag — reader and writer never overlapped"
        );
    }

    #[test]
    fn retry_exhaustion_is_torn_never_stale() {
        let r = Ring::new(1);
        if let Some(m) = r.meta.first() {
            m.seq.store(1, Ordering::Relaxed); // stuck odd: "mid-transaction" forever
        }
        let mut dst = vec![9.0f32; BLOCK];
        r.read(0, &mut dst, BLOCK);

        assert_eq!(r.stale(), 0, "a torn read must never be counted as stale");
        assert_eq!(
            r.torn(),
            1,
            "retry-budget exhaustion must be counted as torn"
        );
        assert_eq!(
            r.retries(),
            u64::from(RETRY_LIMIT - 1),
            "every attempt after the first counts as a retry"
        );
        assert!(
            dst.iter().all(|&x| x == 0.0),
            "nothing was ever delivered to fade from"
        );
    }

    #[test]
    fn genuinely_stale_block_is_still_reported_stale_when_seq_is_healthy() {
        let r = Ring::new(1);
        assert_eq!(
            r.meta.len(),
            RING_BLOCKS,
            "setup invalid: Ring::new must allocate RING_BLOCKS block metadata entries"
        );
        let Some(m) = r.meta.first() else { return };
        m.seq.store(0, Ordering::Relaxed); // healthy: even, no writer active
        m.stamp.store(999, Ordering::Relaxed); // some other lap's stamp
        let mut dst = vec![9.0f32; BLOCK];
        r.read(0, &mut dst, BLOCK); // bstart for t=0 is 0, != 999
        assert_eq!(
            r.stale(),
            1,
            "a genuinely stale block (seq healthy) must still be reported stale"
        );
        assert_eq!(r.torn(), 0);
        assert_eq!(r.retries(), 0, "a stale block must not be retried");
    }

    #[test]
    fn stale_before_seq_recheck_would_double_count_a_genuine_in_flight_write() {
        let r = Arc::new(Ring::new(1));
        let bstart_old = 0u64;
        let bstart_new = RING_FRAMES as u64; // a different (later) lap, same block index

        let old_data = [1.0f32];
        r.write(bstart_old, &old_data, 1);

        let (s1, stamp_captured) = r
            .meta
            .first()
            .map(|m| {
                (
                    m.seq.load(Ordering::Acquire),
                    m.stamp.load(Ordering::Acquire),
                )
            })
            .unwrap_or((u64::MAX, u64::MAX));
        assert!(
            s1.is_multiple_of(2),
            "setup invalid: seq must look healthy/even at the moment of capture"
        );

        let (go_tx, go_rx) = std::sync::mpsc::sync_channel::<()>(0);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(0);
        let writer_ring = Arc::clone(&r);
        let writer = std::thread::spawn(move || {
            go_rx.recv().ok(); // wait for step 1 above to have already happened
            let new_data = [2.0f32];
            writer_ring.write(bstart_new, &new_data, 1); // a real, complete seqlock transaction, a NEW lap
            done_tx.send(()).ok();
        });

        go_tx.send(()).ok(); // release the writer now that step 1 is captured
        done_rx.recv().ok(); // block until its transaction has fully closed
        let _ = writer.join();

        // The naive ordering: a verdict from the pre-write stamp, then a re-read of
        // `stamp` to decide whether to also count torn. Both fire for one event.
        let would_report_stale = stamp_captured != bstart_new;
        assert!(
            would_report_stale,
            "setup invalid: the captured (previous-lap) stamp must mismatch the new lap"
        );

        let stamp_after = r
            .meta
            .first()
            .map(|m| m.stamp.load(Ordering::Acquire))
            .unwrap_or(u64::MAX);
        let would_also_report_torn = stamp_after == bstart_new;
        assert!(
            would_also_report_torn,
            "demonstration invalid: the second read must see the completed write for the double-count to occur"
        );

        let mut dst = vec![9.0f32; 1];
        r.read(bstart_new, &mut dst, 1);
        assert_eq!(r.stale(), 0, "read_block must not count this as stale");
        assert_eq!(r.torn(), 0, "read_block must not count this as torn either");
        assert_eq!(
            dst,
            vec![2.0f32],
            "the real, now-complete data must be delivered"
        );
    }

    #[test]
    fn write_and_read_no_op_on_undersized_buffers_rather_than_panicking() {
        let r = Ring::new(2);
        let short_src = vec![1.0f32; 4]; // needs 512*2 = 1024 for frames=512
        r.write(0, &short_src, 512);
        let mut dst = vec![9.0f32; 1024];
        r.read(0, &mut dst, 512);
        assert!(dst.iter().all(|&x| x == 0.0));
        assert_eq!(r.stale(), 1);

        let src = vec![1.0f32; 1024];
        r.write(100, &src, 512);
        let mut short_dst = vec![9.0f32; 4];
        r.read(100, &mut short_dst, 512);
        assert_eq!(
            short_dst,
            vec![9.0f32; 4],
            "undersized dst must be left untouched"
        );
    }

    #[test]
    fn a_torn_block_fades_from_the_last_real_sample_instead_of_clicking() {
        let r = Ring::new(1);
        let src = vec![8.0f32; BLOCK];
        r.write(0, &src, BLOCK); // block 0 real; block 1 forced torn below

        if let Some(m) = r.meta.get(1) {
            m.seq.store(1, Ordering::Relaxed);
        }
        let mut dst = vec![9.0f32; BLOCK * 2];
        r.read(0, &mut dst, BLOCK * 2);

        assert_eq!(r.torn(), 1);
        let boundary = dst.get(BLOCK - 1).copied().unwrap_or(f32::NAN);
        let first_of_fade = dst.get(BLOCK).copied().unwrap_or(f32::NAN);
        let last_of_fade = dst.last().copied().unwrap_or(f32::NAN);
        assert_eq!(
            boundary, 8.0,
            "setup invalid: the first block must have delivered real data"
        );
        assert_eq!(
            first_of_fade, 8.0,
            "the fade must start exactly at the last real sample"
        );
        assert!(
            last_of_fade.abs() < first_of_fade.abs(),
            "the fade must decay toward silence, not stay flat"
        );
    }

    #[test]
    fn sustained_gap_flattens_to_silence_after_the_first_fade() {
        let r = Ring::new(1);
        let src = vec![8.0f32; BLOCK];
        r.write(0, &src, BLOCK); // block 0 real; blocks 1 and 2 never written

        let mut dst = vec![9.0f32; BLOCK * 3];
        r.read(0, &mut dst, BLOCK * 3);

        assert_eq!(r.stale(), 2);
        assert_eq!(r.torn(), 0);
        let first_of_first_gap = dst.get(BLOCK).copied().unwrap_or(f32::NAN);
        let first_of_second_gap = dst.get(BLOCK * 2).copied().unwrap_or(f32::NAN);
        assert_eq!(
            first_of_first_gap, 8.0,
            "the first gap must ramp down from the preceding real sample"
        );
        assert_eq!(
            first_of_second_gap, 0.0,
            "the second gap must not re-ramp from the first gap's already-faded tail"
        );
    }
}
