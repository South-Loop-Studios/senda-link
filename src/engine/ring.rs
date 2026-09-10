//! The sample-time-indexed loopback ring buffer.
//!
//! `Ring` stores loopback audio indexed by absolute sample time:
//! `BLOCK`-frame blocks arranged around a `RING_BLOCKS`-deep ring
//! (`RING_FRAMES = BLOCK * RING_BLOCKS`, both from `engine::device`). A
//! single shared slot per device cannot serve several clients — two
//! readers draining the same slot at slightly different times each see a
//! partial, already-overwritten view of it — so a writer and any number
//! of concurrent readers here occupy different, independently-addressable
//! offsets by construction, rather than relying on the HAL to serialise
//! their IO operations.
//!
//! The ring guarantees the following:
//!
//! - A read of a block observes either a complete write or none of it,
//!   never a mixture (the per-block seqlock).
//! - A read is delivered as current only if every requested frame was
//!   written for the lap the caller asked about (the coverage watermark).
//! - Anything that cannot be delivered is faded towards silence rather
//!   than left as stale audio or an abrupt click, and is counted exactly
//!   once, as either `torn` or `stale`.
//! - `write` and `read` take no lock, allocate nothing, and never wait
//!   without a bound.
//!
//! Undersized-buffer calls are rejected up front: `write` with `src`
//! shorter than `frames * channels` samples, or `read` with `dst` shorter
//! than that, returns without touching the ring or the caller's buffer
//! and without incrementing any counter.
//!
//! ## Concurrency model
//!
//! Each block carries its own `BlockMeta` seqlock (`seq` generation +
//! `stamp`, the absolute sample time the block currently holds, plus a
//! coverage watermark — see "Coverage, not just a stamp" below). This
//! assumes a single writer and any number of concurrent readers.
//! Multiple writer threads calling `write` on the same `Ring` concurrently
//! is not supported — there is no writer/writer mutual exclusion here,
//! unlike `engine::clock::Clock::acquire_write`, because nothing in this
//! driver's design ever needs it (a loopback ring has exactly one
//! producer: whichever client is feeding the device's input side).
//! `zero` is, from a concurrency standpoint, a second writer — see its own
//! doc comment for the precondition that follows from that.
//!
//! The seqlock write/read sides use the fence idiom, not `Release`/
//! `Acquire` on the individual accesses: a `Release`-ordered store or RMW
//! only prevents *preceding* operations in this thread from being
//! reordered after it; it does not stop *following* operations (the
//! per-sample stores) from being hoisted before it becomes visible to
//! another core — on a weak memory model (this driver's primary target is
//! arm64 macOS) that is a real possibility, not a theoretical one. See
//! `Ring::write` and `Ring::read_block` for exactly where the fences sit;
//! same idiom as `engine::clock::Clock::start`/`zero_timestamp`.
//!
//! ## Tear vs. staleness
//!
//! These are different failure modes and are counted separately:
//!
//! - A **tear** is transient: the writer is (or was, within this read's
//!   validation window) mid-transaction on the exact block a reader
//!   wants. The writer's critical section is a few hundred to a few
//!   thousand atomic stores — microseconds — so a bounded retry
//!   (`RETRY_LIMIT`) usually recovers silently. Only once the retry budget
//!   is exhausted does the read give up and fall back.
//! - **Staleness** is not transient: the block's `stamp` does not match
//!   the sample time being asked for (or, per the coverage watermark
//!   below, matches but the requested span was never actually written for
//!   this lap), and — critically — this was observed on a snapshot the
//!   seqlock already confirmed was not torn. The data for that lap is
//!   simply not there; retrying cannot produce it, so a stale block is
//!   never retried.
//!
//! The distinction rests on the order of checks in `read_block`: `stamp`
//! is read once per attempt and only compared against the requested lap
//! after `seq` has been re-validated stable across that same attempt.
//! Checking `stamp` before the `seq` re-check would let a reader that
//! sampled `stamp` mid-transaction — after `seq` had gone odd, before the
//! re-check could notice — record an old, about-to-be-replaced value as a
//! permanent "stale" verdict, with no retry, even though the correct data
//! was microseconds away; and a second, later read of `stamp` to decide
//! whether the same failure was also a tear could then count one event
//! under both headings. The counters are only meaningful as a diagnostic
//! if each bad read is attributed to exactly one of them.
//! `stale_before_seq_recheck_would_double_count_a_genuine_in_flight_write`
//! demonstrates both halves with a real concurrent write.
//!
//! ## Coverage, not just a stamp
//!
//! `write` operates at frame granularity — a single call can cover as
//! little as one frame of a `BLOCK`-frame block (real CoreAudio clients
//! negotiate IO buffer sizes from 64 to 4096 frames, see
//! `device::MIN_BUFFER_FRAMES`/`MAX_BUFFER_FRAMES`, and `sample_time` is
//! not generally block-aligned) — but `stamp` only records the block's
//! nominal lap. If `stamp` alone decided currency, `write(256, src, 256)`
//! covering only the back half of block 0 would mark the whole block
//! current, and a later `read(0, dst, 512)` would deliver 256 real frames
//! and 256 frames of whatever sat at that offset one lap (683 ms at
//! 48 kHz) ago — as `Delivered`, incrementing no counter.
//!
//! Each `BlockMeta` therefore also tracks `covered_from`/`covered_to`: the
//! sub-range (frame offset within the block) that has actually been
//! written for the current `stamp`. `read_block` intersects the requested
//! span against it — frames outside the covered span are treated as
//! stale (faded to silence, `stale` incremented) even when `stamp` itself
//! matches. The watermark only grows when a same-lap write touches or
//! overlaps it; a same-lap write that leaves a gap resets the watermark
//! to exactly itself, so two writes such as `[0,128)` then `[256,384)`
//! can never make the untouched `[128,256)` look current — see `write`'s
//! doc comment for the check, and
//! `partial_block_coverage_does_not_mark_the_whole_block_current`/
//! `non_contiguous_same_lap_writes_do_not_bridge_the_gap_between_them`
//! for the tests.
//!
//! ## Fading, not clicking
//!
//! An abrupt jump to silence is itself an audible discontinuity — the
//! same class of defect, moved from "wrong sample" to "wrong shape".
//! `fade_span` ramps each channel from the last real sample already
//! present in the caller's own buffer (the previous block segment `read`
//! itself just delivered, within this same call) down towards silence,
//! rather than either zeroing the caller's output buffer outright or
//! decaying whatever unrelated bytes happened to be in it beforehand.
//!
//! `fade_span` is stateless. `read` takes `&self` and any number of
//! readers may be in flight at once, so a `Ring`-level "last delivered
//! sample" field would be shared mutable state between readers: reader
//! A's last sample would seed reader B's fade, and B's read would then
//! reset it, breaking A's own next fade. Instead the ramp starts from
//! `dst[(done - 1) * channels + c]` when `done > 0` and the immediately
//! preceding block segment in this `read` call was cleanly `Delivered`,
//! and from silence otherwise. Fade continuity across separate calls is
//! not attempted; with concurrent readers it would be meaningless anyway.

use super::device::{BLOCK, RING_BLOCKS, RING_FRAMES};
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Bounded retry budget for the reader's seqlock loop, on the real-time IO
/// thread. Same idiom as `engine::clock::Clock`'s `RETRY_LIMIT`: an
/// unbounded spin waiting on a writer that might be scheduled out would be
/// as fatal, on this thread, as a wrong answer. See `BACKOFF_CEILING` for
/// the worst-case wait this budget implies.
///
/// This bounds a *retry-on-condition* loop specifically. The outer
/// frame-traversal loops in `write`/`read` (`while done < frames`) are a
/// different kind of loop — a fixed, always-terminating walk over a
/// caller-supplied, bounded frame count (at most `MAX_BUFFER_FRAMES`
/// frames per call in practice) that never waits on anything. That is the
/// same distinction `engine::clock` draws between `zero_timestamp`'s
/// bounded retry and its own plain `for` loops.
const RETRY_LIMIT: u32 = 4;

/// Wall-clock ceiling on how long one reader retry attempt busy-waits
/// before trying again, giving the writer's critical section room to
/// close. A hard time bound rather than a raw `spin_loop()` iteration
/// count, because a fixed iteration count is not a reliable proxy for
/// wall-clock delay: `spin_loop()` lowers to aarch64's `yield` hint,
/// which unlike x86's `pause` carries no guaranteed minimum latency, so
/// counting iterations of it cannot size a wait to a real writer
/// transaction on this architecture. `Ring::write`'s critical section
/// normally closes within a few microseconds, but the writer thread is
/// subject to ordinary OS scheduling preemption and can occasionally
/// stall for milliseconds even when uncontended. Sizing this to absorb
/// such a stall is not attempted: it would need a per-attempt budget in
/// the low milliseconds, and `RETRY_LIMIT` attempts of that would itself
/// risk missing this call's own real-time deadline — worse than the rare,
/// correctly-counted `torn` fallback it would be trying to avoid.
/// `Instant` is monotonic, so this remains statically bounded in the
/// sense that matters for a real-time thread: a fixed, deterministic
/// maximum wait rather than an iteration count with no fixed relationship
/// to elapsed time.
///
/// Worst case on the IO thread: `RETRY_LIMIT` spins of up to
/// `BACKOFF_CEILING` each, 4 × 50 µs = 200 µs, per block segment of a
/// `read`; a full-size 4096-frame read spans up to nine `BLOCK`-frame
/// segments (eight, plus one when `sample_time` is not block-aligned).
/// For comparison, the smallest IO period this driver supports is 64
/// frames at 192 kHz, about 333 µs.
const BACKOFF_CEILING: Duration = Duration::from_micros(50);

/// Sentinel `stamp` meaning "this block has never been written." Not a
/// value `block_start` can ever produce for any sample time this driver
/// reaches (that would need `u64::MAX` frames of continuous uptime), so it
/// can never collide with a real block start.
const NO_STAMP: u64 = u64::MAX;

/// Per-block seqlock state.
///
/// `#[repr(align(64))]` is false-sharing padding: it places each entry on
/// its own cache line. `RING_BLOCKS` (64) of these live in one contiguous
/// `Vec`; without this, two adjacent blocks' metadata could share a cache
/// line, and a writer closing block N's transaction would then contend on
/// that line with a reader validating block N+1 — needlessly slowing down
/// exactly the thing (a fast writer transaction closing) the reader's
/// bounded retry budget is counting on. 64 bytes is the x86_64 line size;
/// Apple silicon uses 128-byte lines, so two adjacent entries can still
/// share a line there.
#[repr(align(64))]
struct BlockMeta {
    /// Seqlock generation: even means stable, odd means a writer is
    /// mid-transaction. Same convention as `engine::clock::Clock::generation`.
    seq: AtomicU64,
    /// Absolute sample time of this block's first frame, as of the last
    /// completed write. `NO_STAMP` until the block is written for the
    /// first time. Only trustworthy once a read has confirmed (by
    /// re-checking `seq`) that it was not read mid-transaction — see
    /// `Ring::read_block`. Matching `bstart` is necessary but not
    /// sufficient for the requested span to be current — see
    /// `covered_from`/`covered_to`.
    stamp: AtomicU64,
    /// Start of the sub-range (frame offset within the block, `0..=BLOCK`)
    /// that `write` has actually populated for the current `stamp`'s lap.
    /// See the module docs' "Coverage, not just a stamp" section for why
    /// `stamp` alone is not enough: `write` operates at frame granularity
    /// while `stamp` is block-granular, so a block written 512 frames at
    /// once by one call but 64 at a time by another could otherwise have
    /// `stamp == bstart` while most of its frames still held a previous
    /// lap's data.
    covered_from: AtomicU64,
    /// End (exclusive) of the covered sub-range. See `covered_from`.
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

/// One block-sized segment of a `read` call: the absolute sample time,
/// its block's start, and the frame count — bundled into one type purely
/// to keep `read_block`'s parameter count under clippy's
/// `too_many_arguments` threshold; it carries no behaviour of its own.
struct Segment {
    t: u64,
    bstart: u64,
    n: usize,
}

/// Outcome of validating one block-sized read attempt.
enum BlockOutcome {
    /// A consistent, current snapshot was read into the caller's buffer,
    /// and the entire requested span was actually covered by the current
    /// lap's writes.
    Delivered,
    /// A consistent snapshot was confirmed and `stamp` matched, but only
    /// part of the requested span was covered by the current lap's
    /// writes. `read_block` has already faded the uncovered part(s) of
    /// `dst` in place — this is a "please count it" signal, not a "please
    /// also fade it" one.
    ///
    /// Carries whether the segment's trailing frame (`dst[done + n - 1]`)
    /// is real, just-copied data (the covered span reached all the way to
    /// the end of the request) rather than a faded one. Without this, the
    /// next segment's fade — if it turns out `Stale`/`Torn` — would start
    /// from silence even when this segment's tail was genuinely real,
    /// producing exactly the abrupt click `fade_span` exists to avoid.
    PartiallyStale { trailing_delivered: bool },
    /// A consistent snapshot was confirmed, but none of the requested
    /// span belongs to the current lap — either `stamp` itself mismatches
    /// `bstart` entirely, or it matches but the coverage watermark shows
    /// nothing of the requested range was ever written for it. The data
    /// is genuinely absent; retrying cannot produce it.
    Stale,
    /// `RETRY_LIMIT` attempts were exhausted without ever observing a
    /// consistent snapshot. Transient by nature (the writer is unusually
    /// slow or unusually unlucky with scheduling) but nothing salvageable
    /// was obtained this call.
    Torn,
}

/// The lock-free, sample-time-indexed loopback ring. See the module docs
/// for the concurrency model and the tear/staleness/coverage/fade design.
pub struct Ring {
    /// Interleaved sample storage, `RING_FRAMES * channels` slots, each
    /// holding an `f32`'s raw bits (`AtomicU32`, always accessed
    /// `Relaxed`). This is what lets `engine/` stay
    /// `#![forbid(unsafe_code)]`: a plain `Vec<f32>` shared across a writer
    /// and multiple reader threads would be a data race, and there is no
    /// `unsafe` escape hatch available in this module to paper over it.
    /// `f32::to_bits`/`from_bits` round-trip every bit pattern exactly —
    /// NaN payloads, denormals, signed zero, infinities included — so
    /// storing bits instead of the float costs nothing in fidelity, which
    /// matters here: this device is used for measurement work, where a
    /// silently "cleaned up" NaN would be its own kind of lie. Ordering
    /// comes entirely from `BlockMeta::seq`; these accesses supply only
    /// data-race freedom, hence `Relaxed`.
    data: Vec<AtomicU32>,
    /// One seqlock per `BLOCK`-frame block, `RING_BLOCKS` entries.
    meta: Vec<BlockMeta>,
    channels: usize,
    torn: AtomicU64,
    stale: AtomicU64,
    retries: AtomicU64,
}

impl Ring {
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

    /// Block reads that exhausted `RETRY_LIMIT` without observing a
    /// consistent snapshot. Not yet exposed as a HAL property.
    pub fn torn(&self) -> u64 {
        self.torn.load(Ordering::Relaxed)
    }
    /// Block reads (or sub-spans) whose data for the requested lap was
    /// absent. Not yet exposed as a HAL property.
    pub fn stale(&self) -> u64 {
        self.stale.load(Ordering::Relaxed)
    }
    /// Seqlock read attempts after the first, summed over all block reads.
    /// Not yet exposed as a HAL property.
    pub fn retries(&self) -> u64 {
        self.retries.load(Ordering::Relaxed)
    }

    /// Resets every block to "never written".
    ///
    /// Not real-time safe: clears up to `RING_FRAMES * channels` samples,
    /// which for the largest (128-channel) device is over four million
    /// atomic stores. Callers must only invoke this from a control thread
    /// while IO is stopped for this device — e.g. a sample-rate change,
    /// where old ring contents describe a timeline that no longer exists —
    /// never from the real-time audio path.
    ///
    /// Not safe to call concurrently with `write`: like `write` itself,
    /// this provides no writer/writer mutual exclusion (see the module
    /// docs), and from a concurrency standpoint `zero` is a second writer.
    /// Calling it while the real writer is active is the unsupported
    /// multi-writer configuration `write`'s own doc comment already rules
    /// out; it bears repeating here because a caller could otherwise reach
    /// for `zero` from a context that races the writer without realising
    /// it is one.
    ///
    /// Safe with respect to concurrent readers: each block's reset is
    /// bracketed by the same seqlock transition `write` uses, so a reader
    /// racing this call observes either the complete pre-reset state or
    /// the complete post-reset ("never written") state, never a mixture —
    /// e.g. never zeroed samples paired with a `stamp` that still claims
    /// them current, which an unprotected reset could otherwise produce
    /// (a reader would then deliver silence as `Delivered`, with tear
    /// detection never armed for it at all).
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

    /// The `BLOCK`-aligned start of the block containing `t`. Always
    /// `<= t` (it is `t` minus `t % BLOCK`), so the subtraction below can
    /// never underflow.
    fn block_start(t: u64) -> u64 {
        t - (t % BLOCK as u64)
    }

    /// Which of the `RING_BLOCKS` metadata slots covers `t`.
    fn block_index(t: u64) -> usize {
        ((t / BLOCK as u64) % RING_BLOCKS as u64) as usize
    }

    /// Flat index of channel 0 of the frame at absolute sample time `t`,
    /// into `data`. Always `< data.len()` given this `Ring`'s own geometry
    /// (`data.len() == RING_FRAMES * channels`): `t % RING_FRAMES` ranges
    /// over `0..RING_FRAMES`, so the largest possible result plus the
    /// largest channel offset (`channels - 1`) lands on `data.len() - 1`.
    /// `write`/`read` still go through `data.get(...)` rather than
    /// indexing directly — `engine::mod` denies `clippy::indexing_slicing`
    /// unconditionally, and treating that as load-bearing rather than
    /// provably-dead is cheap insurance against this invariant being
    /// broken by a future change to either this function or `Ring::new`.
    fn slot(&self, t: u64) -> usize {
        (t % RING_FRAMES as u64) as usize * self.channels
    }

    /// Number of `f32` samples `frames` interleaved frames occupy at this
    /// `Ring`'s channel count, or `None` on overflow. `write` and `read`
    /// use it to reject a malformed call before touching any state.
    fn required_len(&self, frames: usize) -> Option<usize> {
        frames.checked_mul(self.channels)
    }

    /// Writes `frames` interleaved frames from `src` starting at absolute
    /// sample time `sample_time`. Spans multiple blocks and wraps around
    /// the ring transparently.
    ///
    /// No-ops (does not touch any state) if `src` is shorter than
    /// `frames * channels` samples, rather than reading whatever is
    /// available and silently writing a partial frame.
    ///
    /// Each block segment updates a coverage watermark
    /// (`BlockMeta::covered_from`/`covered_to`) alongside `stamp`: a
    /// segment that continues the same lap already recorded for this block
    /// (the block's previous `stamp` equals this segment's `bstart`) and
    /// touches or overlaps the existing watermark extends it to their
    /// union; anything else (a new lap, the block's first-ever write, or a
    /// same-lap segment that leaves a genuine gap before or after the
    /// existing watermark) resets it to exactly this segment. The
    /// contiguity/overlap check is what makes this safe for a caller that
    /// does not write a lap in strictly increasing order: two same-lap
    /// segments with a real gap between them — `[0,128)` then `[256,384)`
    /// — reset rather than bridge the gap, so `read`'s later intersection
    /// against this watermark can never claim the skipped `[128,256)` is
    /// current when it never was. This driver's own write pattern (a
    /// single writer whose `sample_time` only ever advances) never
    /// exercises the reset-on-gap branch in practice, but the watermark's
    /// correctness does not depend on that.
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
                // Unreachable given `within < BLOCK` and `done < frames`
                // above, but this is a real-time loop — stop rather than
                // spin in place if that invariant is ever violated.
                break;
            }
            let bi = Self::block_index(t);
            let Some(m) = self.meta.get(bi) else { return };

            // Opening transition: a Relaxed store, not a Release RMW,
            // followed by an explicit fence(Release) — see the module docs
            // for why the fence (not ordering on the access itself) is
            // what actually stops the per-sample stores below from being
            // hoisted ahead of this becoming visible.
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
                    // A `None` here is unreachable given the `required_len`
                    // check and `slot`'s documented invariant, both above —
                    // skipping rather than aborting the transaction is the
                    // defensive choice: a `return` here would leave `seq`
                    // stuck odd, poisoning this block for every future
                    // reader.
                }
            }

            let within_u64 = within as u64;
            let end_u64 = within_u64.saturating_add(n as u64);
            let old_stamp = m.stamp.load(Ordering::Relaxed);
            let old_from = m.covered_from.load(Ordering::Relaxed);
            let old_to = m.covered_to.load(Ordering::Relaxed);
            // Union-by-min/max alone would over-claim coverage when two
            // same-lap writes are not contiguous or overlapping: `[0,128)`
            // then `[256,384)` would produce a watermark of `[0,384)`, and
            // a later read of `[128,256)` would report `Delivered` while
            // actually handing back the previous lap's audio for that gap,
            // with no counter incremented. So the watermark is only
            // extended when the new segment touches or overlaps the
            // existing one (`within_u64 <= old_to && end_u64 >= old_from`);
            // a non-contiguous segment resets the span to exactly itself
            // instead of bridging the gap. Correctness therefore does not
            // rest on the "single writer whose `sample_time` only
            // advances" assumption as a caller contract.
            let (new_from, new_to) =
                if old_stamp == bstart && within_u64 <= old_to && end_u64 >= old_from {
                    (old_from.min(within_u64), old_to.max(end_u64))
                } else {
                    (within_u64, end_u64)
                };
            m.covered_from.store(new_from, Ordering::Relaxed);
            m.covered_to.store(new_to, Ordering::Relaxed);
            m.stamp.store(bstart, Ordering::Relaxed);
            // Closing transition: a genuine Release store. Everything
            // written above (data, coverage, and stamp) happens-before
            // any reader thread that Acquire-loads this exact value of
            // `seq`.
            m.seq.fetch_add(1, Ordering::Release); // -> even
            done = done.saturating_add(n);
        }
    }

    /// Reads `frames` interleaved frames into `dst` starting at absolute
    /// sample time `sample_time`. Spans multiple blocks and wraps around
    /// the ring transparently. Blocks (or sub-spans of a block — see the
    /// module docs' "Coverage, not just a stamp" section) that are torn
    /// or stale are faded to silence rather than left as whatever `dst`
    /// already contained — see `fade_span`.
    ///
    /// No-ops (does not touch `dst`) if it is shorter than
    /// `frames * channels` samples.
    pub fn read(&self, sample_time: u64, dst: &mut [f32], frames: usize) {
        let Some(need) = self.required_len(frames) else {
            return;
        };
        if dst.len() < need {
            return;
        }

        let mut done = 0usize;
        // Whether the immediately preceding block segment in this call
        // was cleanly `Delivered` — the only case in which `dst`'s
        // trailing sample is a real, trustworthy fade-continuation point.
        // See `fade_span` and the module docs' "Fading, not clicking"
        // section for why this is per-call state rather than a field on
        // `Ring`.
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
                    // `read_block` has already faded the uncovered part(s)
                    // of this segment in place. If the segment's own
                    // trailing frame is real (the covered span reached the
                    // end of the request), the next segment's fade (if
                    // any) must still be able to start from it — a flat
                    // `false` here would click to silence even when real
                    // audio was right there.
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

    /// One block's worth of the seqlock read protocol, bounded to
    /// `RETRY_LIMIT` attempts.
    ///
    /// The critical property, which keeps tear and staleness apart as
    /// described in the module docs: `stamp` (and the coverage watermark)
    /// are read exactly once per attempt, and are only trusted (compared
    /// against `bstart`/the requested span) *after* `seq` has been
    /// re-validated stable across that same attempt — i.e. after this
    /// attempt has already proven it was not looking at a torn snapshot.
    /// A mismatch discovered any other way would not distinguish
    /// "genuinely a different/incomplete lap" from "read while the writer
    /// was replacing it", which is exactly what staleness must not be
    /// confused with.
    ///
    /// As an optimisation (not a correctness requirement — the property
    /// above holds regardless): if `stamp`/coverage already look like the
    /// requested span cannot possibly be current, the per-sample copy
    /// below (up to `BLOCK * channels` samples, up to 256 KB for this
    /// driver's largest configured block) is skipped, since it would only
    /// be discarded. This is a hint, not a verdict — `s2` is still
    /// checked below before trusting it, exactly like every other
    /// observation here; a mismatch discovered before the copy still goes
    /// through the same "confirm not torn, then decide" path as one
    /// discovered after it.
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
                // A hard time bound via `checked_add`, not a bare
                // `Instant + Duration` (whose `Add` impl can itself panic
                // on overflow — a panic path `clippy::expect_used`/
                // `unwrap_used`/`panic` cannot see, since it is not a
                // literal `.unwrap()`/`.expect()`/`panic!()` in this
                // source). Overflow here would need `Instant::now()` to
                // already be within `BACKOFF_CEILING` of the platform's
                // representable maximum — not reachable in practice, but
                // skipping the backoff and retrying immediately is a safe,
                // cheap fallback rather than a reason to trust that.
                if let Some(deadline) = Instant::now().checked_add(BACKOFF_CEILING) {
                    while Instant::now() < deadline {
                        std::hint::spin_loop();
                    }
                }
            }

            // Read side, mirroring `Clock::zero_timestamp`: Acquire on the
            // first generation load, Relaxed on the fields, fence(Acquire)
            // before the re-check — not Acquire on the re-check load
            // itself. See the module docs for why.
            let s1 = m.seq.load(Ordering::Acquire);
            if !s1.is_multiple_of(2) {
                continue; // writer mid-transaction; bounded retry
            }

            let stamp = m.stamp.load(Ordering::Relaxed);
            let cov_from = m.covered_from.load(Ordering::Relaxed);
            let cov_to = m.covered_to.load(Ordering::Relaxed);
            let ov_from = cov_from.max(req_from);
            let ov_to = cov_to.min(req_to);
            // Hint only (see the doc comment above) — `stamp`/coverage are
            // re-consulted, and only trusted, after `s2` confirms below
            // that this attempt was not torn.
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

            // `seq` was stable (even, unchanged) across the entire read
            // above: this is a confirmed-consistent snapshot. Now `stamp`
            // and the coverage watermark mean what they say.
            if stamp != bstart {
                return BlockOutcome::Stale; // different lap entirely
            }
            if ov_from >= ov_to {
                return BlockOutcome::Stale; // matched lap, nothing requested was ever written for it
            }
            if ov_from <= req_from && ov_to >= req_to {
                // Fully covered: the copy above already populated every
                // requested frame with real data.
                return BlockOutcome::Delivered;
            }

            // Partial coverage: the copy above populated every requested
            // frame, but only [ov_from, ov_to) of that is trustworthy —
            // fade whichever part(s) fall outside it, in place.
            if ov_from > req_from {
                let gap_len = ov_from.saturating_sub(req_from) as usize;
                self.fade_span(dst, done, gap_len, prev_delivered);
            }
            let trailing_delivered = ov_to >= req_to;
            if ov_to < req_to {
                let gap_off = ov_to.saturating_sub(req_from) as usize;
                let gap_len = req_to.saturating_sub(ov_to) as usize;
                // The covered middle segment directly precedes this
                // trailing gap within this exact call and is real,
                // just-copied data — continuity holds here regardless of
                // what preceded the whole block.
                self.fade_span(dst, done.saturating_add(gap_off), gap_len, true);
            }
            return BlockOutcome::PartiallyStale { trailing_delivered };
        }
        BlockOutcome::Torn
    }

    /// Ramps `dst[done..done+n)` (interleaved, all channels) towards
    /// silence, overwriting whatever `dst` held — never decaying the
    /// caller's incoming contents, which carry no meaning (`read`'s
    /// contract is to fill this buffer, not accumulate into it).
    ///
    /// Stateless (see the module docs' "Fading, not clicking" section):
    /// each channel's ramp starts from `dst[(done - 1) * channels + c]`
    /// when `done > 0` and `prev_delivered` (the immediately preceding
    /// block segment in this same `read` call was cleanly `Delivered`),
    /// and from silence otherwise — including for the very first segment
    /// of a call (`done == 0`), and for any segment following one that
    /// was itself faded or only partially covered.
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
                // f in [0, n): gain 1.0 at the first missing frame
                // (continuous with the last real sample), decaying towards
                // (but not necessarily exactly reaching, for a nonzero
                // `start`) zero by the last. `n > 0` checked above.
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
        // One full lap later, same ring slot, different sample time.
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

    /// `write` operates at frame granularity while `stamp` is
    /// block-granular. Fill one full lap, then write only the second half
    /// of the following lap's block 0: a full-block read of that lap's
    /// block 0 must not return the first half (one-lap-stale audio from
    /// the previous lap) as though it were current, and must count the
    /// partially-covered block as stale.
    #[test]
    fn partial_block_coverage_does_not_mark_the_whole_block_current() {
        let r = Ring::new(1);
        let full: Vec<f32> = vec![1.0; RING_FRAMES];
        r.write(0, &full, RING_FRAMES); // fill one full lap
        let half = vec![2.0f32; BLOCK / 2];
        // Covers only the 2nd half of the next lap's block 0.
        r.write(RING_FRAMES as u64 + (BLOCK / 2) as u64, &half, BLOCK / 2);

        let mut dst = vec![9.0f32; BLOCK];
        r.read(RING_FRAMES as u64, &mut dst, BLOCK);

        // The first half belongs to the previous lap and must not be
        // delivered as though it were current.
        assert!(
            dst.iter().take(BLOCK / 2).all(|&x| x != 1.0),
            "stale (previous-lap) audio leaked through as if it were current"
        );
        assert!(
            dst.iter().take(BLOCK / 2).all(|&x| x == 0.0),
            "uncovered frames must be silent, not stale audio"
        );
        // The second half genuinely is current-lap data and must still be
        // delivered.
        assert!(
            dst.iter().skip(BLOCK / 2).all(|&x| x == 2.0),
            "covered frames must still be delivered"
        );
        assert_eq!(r.stale(), 1, "the partially-covered block must be counted");
    }

    /// Two non-adjacent writes to the same block in the same lap —
    /// `[0,128)` then `[256,384)`, leaving `[128,256)` untouched for this
    /// lap — must not produce a watermark of `[0,384)`: a read of the gap
    /// would then report `Delivered` while actually handing back whatever
    /// the previous lap left there, uncounted. `write` resets the
    /// watermark to exactly the most recent segment whenever it is not
    /// contiguous with or overlapping the existing one, rather than
    /// tracking multiple disjoint covered spans — see its doc comment.
    #[test]
    fn non_contiguous_same_lap_writes_do_not_bridge_the_gap_between_them() {
        let r = Ring::new(1);
        // A full previous lap, so an incorrectly-bridged gap would read
        // back as real (but stale) audio rather than coincidentally
        // matching the ring's zero-initialised default.
        let full: Vec<f32> = vec![1.0; RING_FRAMES];
        r.write(0, &full, RING_FRAMES);

        // Two non-adjacent writes to the same block, same (next) lap.
        // `[128,256)` is never written for this lap. The second write's
        // watermark update resets to exactly `[256,384)` rather than
        // unioning with the first write's `[0,128)`, since the two do not
        // touch or overlap.
        let next_lap = RING_FRAMES as u64;
        let a = vec![2.0f32; 128];
        let b = vec![3.0f32; 128];
        r.write(next_lap, &a, 128); // covers [0,128) ... only until the next write
        r.write(next_lap + 256, &b, 128); // resets coverage to exactly [256,384)

        let mut dst = vec![9.0f32; BLOCK];
        r.read(next_lap, &mut dst, BLOCK);

        // Nowhere in the block may the previous lap's audio (1.0) leak
        // through as if it were current.
        assert!(
            dst.iter().all(|&x| x != 1.0),
            "stale (previous-lap) audio leaked through as if it were current"
        );
        // Only the most recent, self-contained write is trusted.
        assert!(
            dst.iter().take(256).all(|&x| x == 0.0),
            "everything before the most recent covered span must be silent, not delivered"
        );
        assert!(
            dst.iter().skip(256).take(128).all(|&x| x == 3.0),
            "the most recent write's span must still be delivered"
        );
        // Everything after the most recent covered span is a trailing gap
        // within the same `read` call — it fades from that span's real
        // trailing sample (3.0) rather than clicking straight to zero
        // (see `BlockOutcome::PartiallyStale`), so this is not flat
        // silence.
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
        // A writer and a reader genuinely racing on the same ring: no
        // block may ever be delivered half-written (real samples mixed
        // with zeros, or with the caller's untouched sentinel), and no
        // read may exhaust its retry budget. This is the invariant that
        // makes a shared loopback device usable by several clients at
        // once.
        //
        // The writer is paced to a real sample clock rather than
        // free-running: two free-spinning loops of near-identical
        // per-iteration cost fall into near-lockstep, so the reader's
        // target block would coincide with the writer's current block far
        // more often than hardware-clocked IO ever does. Two further
        // choices follow from that pacing:
        //
        // - The writer always writes whichever block is due right now,
        //   not "the next block after the last one written". A real HAL
        //   IO thread is driven by a hardware callback on a fixed cadence
        //   — if the OS preempts it past a cycle, the next callback fires
        //   for the current cycle, not a catch-up burst replaying every
        //   cycle that was missed. Such a burst would produce tears that
        //   are an artefact of the harness, not of `Ring`.
        // - The reader keeps a `lag_blocks` margin behind the live edge
        //   (below), as any real client keeps a buffering margin.
        //
        // The 20-block lag here is deliberately generous. This driver's
        // shipped configuration runs at `SafetyOffset = 0, Latency = 0`,
        // i.e. roughly zero reader lag, which
        // `concurrent_writer_and_reader_at_zero_lag_records_contention_without_flaky_assertions`
        // exercises instead, without a zero-tear assertion.
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
                // Whatever block is due right now, not "the next
                // sequential block after the last one written" — see the
                // note at the top of this test.
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

        // Let the writer establish steady state before the reader starts
        // sampling: a freshly spawned thread's first few scheduling
        // quanta are noisy (thread creation, first-touch page faults,
        // cache warm-up) in a way steady-state operation is not — same
        // reason a real client's first few IO callbacks after `StartIO`
        // are not representative of steady playback either.
        std::thread::sleep(std::time::Duration::from_millis(20));

        let mut silent = 0u64;
        let mut delivered_real_audio = 0u64;
        let mut total = 0u64;
        // A realistic reader asks for audio a little behind the live
        // edge — the same buffering margin any real client keeps — and,
        // like the writer, asks for it one whole block at a time rather
        // than at an arbitrary (sub-block) wall-clock offset. A
        // non-block-aligned `t` would make every `read` straddle two
        // blocks (see `Ring::read`'s sub-block splitting), doubling the
        // per-call boundary checks and making the reader linger on the
        // exact boundary the writer is closest to for many consecutive
        // iterations while wall-clock time creeps across that one block's
        // span.
        //
        // 20 blocks (~213 ms at 48 kHz) is generous compared to a
        // production buffering margin, but it is sized to this harness —
        // two threads spinning as fast as the CPU allows, in an
        // unoptimised build, in parallel with every other test in this
        // crate — where an ordinary scheduler preemption of the writer
        // can stall it for several milliseconds at a time. This is a
        // property of the test's own scheduling exposure, not a claim
        // about what a real client needs to buffer; the zero-lag test
        // below exercises the real operating point.
        let lag_blocks = 20u64;
        // Reused rather than reallocated every iteration: 20,000 fresh
        // allocations on this thread added measurable allocator-lock
        // contention with the other tests running in parallel, which was
        // itself part of what made the writer harder to schedule promptly
        // — noise from the test, not from `Ring`.
        let mut dst = vec![9.0f32; BLOCK * 2];
        for _ in 0..20_000 {
            let elapsed_blocks = (epoch.elapsed().as_secs_f64() * RATE) as u64 / BLOCK as u64;
            let t = elapsed_blocks
                .saturating_sub(lag_blocks)
                .saturating_mul(BLOCK as u64);
            dst.fill(9.0);
            r.read(t, &mut dst, BLOCK);
            // A block is either fully written or reported stale/torn —
            // never half: no sample in a delivered block may show the
            // caller's un-overwritten sentinel (9.0), and a silent
            // (all-zero) block must be either genuinely ahead of the
            // writer or a completed fade, never a mix of real samples
            // (1.0, from the writer) and zeros.
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

        // Reads ahead of the writer are legitimately silent; what must
        // never happen is a torn block delivering half audio and half
        // zeros (checked above, every iteration) or an unrecoverable tear
        // (checked here, in aggregate).
        assert_eq!(
            r.torn(),
            0,
            "unrecoverable tears must not occur; retries={}",
            r.retries()
        );
        assert!(total > 0 && silent <= total);
        // `silent <= total` holds by construction of the loop above
        // (`silent` only ever increments a subset of `total`'s
        // increments), so on its own it says nothing about the ring. The
        // assertion with content is this one: the writer and reader must
        // actually have overlapped at least once.
        assert!(
            delivered_real_audio > 0,
            "no real audio was ever delivered — reader and writer never overlapped"
        );
    }

    /// Exercises this driver's shipped operating point — `SafetyOffset =
    /// 0, Latency = 0`, i.e. a reader that asks for audio at exactly the
    /// live edge (`lag_blocks = 0`) rather than the comfortably-lagged
    /// 20-block margin the test above uses. At this lag, the reader
    /// hitting the writer mid-transaction is expected, not anomalous, so
    /// this test deliberately does not assert `torn() == 0` /
    /// `stale() == 0`: a zero assertion here would only hold on a machine
    /// whose scheduling noise happened to permit it.
    ///
    /// What does hold unconditionally, contention or not, is checked
    /// instead: no untouched sentinel ever leaks through, and `torn() +
    /// stale()` never exceeds the number of reads that could possibly
    /// have needed to fall back — a single bad read counted as both (the
    /// double count described in the module docs' "Tear vs. staleness"
    /// section) would violate this over enough iterations, so it is not
    /// vacuous. `torn`/`stale`/`retries` are also recorded (`eprintln!`,
    /// visible with `--nocapture`) so a human can see what contention
    /// looked like at this operating point.
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
        // Zero lag matches the shipped `SafetyOffset = 0, Latency = 0`
        // configuration exactly. The writer writes block `E` at the very
        // start of period `E` and then spins for the rest of that ~10.7 ms
        // period, so a lag-0 reader finds block `E` present for the
        // overwhelming majority of it; the reads landing inside the
        // writer's brief transaction window are exactly the ones worth
        // observing.
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

    /// Deterministic (non-probabilistic) proof that a tear is counted as
    /// `torn`, never `stale` — the exact defect described in the module
    /// docs' "Tear vs. staleness" section. Manually forces block 0's
    /// seqlock into "writer mid-transaction, forever" (an adversarial
    /// stand-in for a writer stuck for the whole retry budget — the
    /// mechanism `RETRY_LIMIT` exists to bound), then confirms:
    ///
    /// - `stale()` stays at 0 — a torn read must never be attributed to
    ///   staleness, which by definition needs a *confirmed-consistent*
    ///   snapshot.
    /// - `torn()` becomes exactly 1.
    /// - `retries()` becomes exactly `RETRY_LIMIT - 1` (attempt 0 is not a
    ///   retry; each subsequent attempt is).
    /// - the output was faded (all zero here, since nothing was ever
    ///   successfully delivered on this fresh `Ring` to fade from).
    ///
    /// This is `mod tests`' private-field access to `BlockMeta`, the same
    /// technique `engine::clock`'s
    /// `acquire_write_prevents_a_torn_snapshot` uses to reproduce an
    /// interleaving deterministically rather than relying on timing luck.
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

    /// Companion negative control: a genuinely stale block (healthy,
    /// even `seq`, `stamp` legitimately from a different lap) must still
    /// be reported stale, not swallowed by whatever mechanism keeps a
    /// mid-transaction read from being misreported as stale. Together
    /// with `retry_exhaustion_is_torn_never_stale`, this pins down both
    /// directions of the tear/staleness distinction described in the
    /// module docs.
    #[test]
    fn genuinely_stale_block_is_still_reported_stale_when_seq_is_healthy() {
        let r = Ring::new(1);
        // `r.meta` has exactly `RING_BLOCKS` entries by construction
        // (`Ring::new`), so `.first()` can never actually be `None` here —
        // but a silent `else { return }` would let a future refactor that
        // broke that invariant go undetected (the rest of this test would
        // just never run, and still report as passing). Asserting the
        // precondition explicitly makes a break loud instead.
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

    /// Demonstrates, with a real concurrent write, the double count that
    /// a "check `stamp` before re-validating `seq`" ordering produces —
    /// the ordering the module docs' "Tear vs. staleness" section rules
    /// out.
    ///
    /// A real previous-lap stamp is established first (captured from a
    /// fresh ring it would be `NO_STAMP`, and the central assertion would
    /// reduce to `u64::MAX != 0`). The test then makes the two decisions
    /// such an ordering would make, in the order it would make them:
    /// (1) an immediate "stale" verdict from a `stamp` read before a real
    /// concurrent writer (rendezvous-synchronised via blocking channels,
    /// not sleep-based timing) has done anything, and (2) a second,
    /// independent re-read of `stamp` after that writer has completed,
    /// consulted to decide whether to also count `torn` — and shows both
    /// fire for the exact same real event. `read_block`, given the
    /// identical completed write, does neither: it delivers the real data
    /// with both counters untouched.
    #[test]
    fn stale_before_seq_recheck_would_double_count_a_genuine_in_flight_write() {
        let r = Arc::new(Ring::new(1));
        let bstart_old = 0u64;
        let bstart_new = RING_FRAMES as u64; // a different (later) lap, same block index

        // A real previous-lap stamp, established by a real write.
        let old_data = [1.0f32];
        r.write(bstart_old, &old_data, 1);

        // Step 1 (this thread): the naive ordering's first observation —
        // exactly what a reader would see an instant before a writer, for
        // a new lap on the same block, does anything at all.
        //
        // `.unwrap_or` with a sentinel that cannot accidentally satisfy
        // the assertions below, instead of a silent `None => return`:
        // `r.meta` always has `RING_BLOCKS` entries, so this is
        // unreachable in practice, but a broken precondition must fail the
        // assertions loudly rather than let the test quietly stop early
        // and still pass. `u64::MAX` guarantees `s1.is_multiple_of(2)`
        // fails immediately below (odd), matching the
        // `.unwrap_or(f32::NAN)` idiom used for the float cases elsewhere
        // in this file.
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

        // The naive ordering, check 1: decide "stale" immediately from
        // the captured pre-write snapshot, never re-consulting `seq`.
        let would_report_stale = stamp_captured != bstart_new;
        assert!(
            would_report_stale,
            "setup invalid: the captured (previous-lap) stamp must mismatch the new lap"
        );

        // The naive ordering, check 2: after its retry loop has already
        // broken out on the verdict above, it re-reads `stamp` a second
        // time to decide whether to also count `torn`. The real writer
        // above has, by now, completed — this second read sees the new
        // lap's stamp. Same `.unwrap_or` sentinel reasoning as `stamp_captured`
        // above: `u64::MAX` cannot equal `bstart_new`, so a broken
        // precondition fails the assertion below loudly instead of
        // returning early and passing.
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
        // Both checks fire for one real event: the double count.
        // `read_block`'s actual behaviour, given the identical completed
        // write, does not — verified below.

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
        // Nothing should have been written: reading it back must be a
        // never-written (stale) block, not a partially-populated one.
        let mut dst = vec![9.0f32; 1024];
        r.read(0, &mut dst, 512);
        assert!(dst.iter().all(|&x| x == 0.0));
        assert_eq!(r.stale(), 1);

        // A short destination buffer must also be a no-op, not a panic.
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

        // Force the next block's seqlock stuck odd, as in
        // `retry_exhaustion_is_torn_never_stale`.
        if let Some(m) = r.meta.get(1) {
            m.seq.store(1, Ordering::Relaxed);
        }
        // Read both blocks in one call so the fade can see the first
        // block's real trailing sample at `dst[done - 1]` — `fade_span`
        // only has visibility within a single `read` call (module docs'
        // "Fading, not clicking" section); a separate, later call could
        // never see it, `Ring`-level state or not.
        let mut dst = vec![9.0f32; BLOCK * 2];
        r.read(0, &mut dst, BLOCK * 2);

        assert_eq!(r.torn(), 1);
        // `.unwrap_or(f32::NAN)` rather than `let Some(x) = ... else {
        // return }`: the latter would go green on a broken precondition
        // instead of failing the assertions below, whereas `NAN` never
        // equals the expected values.
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

        // One call spanning the real block followed by two never-written
        // ones, so the first gap's fade can see the real trailing sample
        // — `fade_span` only has visibility within a single `read` call
        // (module docs' "Fading, not clicking" section). Continuity
        // across separate `read` calls is absent by design, not merely
        // untested here: any `Ring`-level carrier for it would be state
        // shared between concurrent readers.
        let mut dst = vec![9.0f32; BLOCK * 3];
        r.read(0, &mut dst, BLOCK * 3);

        // Both gaps are stale (never written), not torn.
        assert_eq!(r.stale(), 2);
        assert_eq!(r.torn(), 0);
        // The first gap ramps down from the preceding real sample; the
        // second, immediately following, starts already at silence rather
        // than ramping down from 8.0 a second time.
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
