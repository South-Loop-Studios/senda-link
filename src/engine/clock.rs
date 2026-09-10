//! Per-device monotonic zero-timestamp source for `GetZeroTimeStamp`.
//! `start` publishes `anchor_ticks`/`ticks_per_period`/`seed` under a
//! seqlock (`generation`); `zero_timestamp` ratchets an epoch-tagged period
//! count with a CAS and never reports less than its last output for a seed.
//! The period is always `RING_FRAMES` frames, never the client's IO buffer.
//! The HAL contract: `start` runs before the first read, and each device's
//! `zero_timestamp` is called from one IO thread only. Tick values arrive as
//! plain `u64`s; the clock itself is read in `src/ffi/`.

use super::device::RING_FRAMES;
use std::sync::atomic::{AtomicU64, Ordering};

/// Retry budget for the seqlock read and the ratchet loop in `zero_timestamp`;
/// an unbounded spin on the IO thread is as bad as a wrong answer.
const RETRY_LIMIT: u32 = 4;

/// Busy-spins per round in `acquire_write` before yielding; bounds the wait, not the acquire.
const WRITE_SPIN_LIMIT: u32 = 4;

/// Low bits of `count_and_epoch` holding the period count; the high 16 bits
/// hold the epoch tag. 48 bits is years of periods at any rate used here.
const COUNT_BITS: u32 = 48;
const COUNT_MASK: u64 = (1u64 << COUNT_BITS) - 1;

/// Packs a 16-bit epoch tag and a `COUNT_BITS`-wide count into one `u64`.
const fn pack(epoch_tag: u16, count: u64) -> u64 {
    ((epoch_tag as u64) << COUNT_BITS) | (count & COUNT_MASK)
}

/// Inverse of `pack`.
const fn unpack(packed: u64) -> (u16, u64) {
    ((packed >> COUNT_BITS) as u16, packed & COUNT_MASK)
}

/// Per-device monotonic zero-timestamp clock; one per device in `CLOCKS`.
pub struct Clock {
    /// Tick value of the last `start()`; the timeline origin. Under `generation`.
    anchor_ticks: AtomicU64,
    /// Ticks per `RING_FRAMES`-frame period at the current rate. Under `generation`.
    ticks_per_period: AtomicU64,
    /// Period count (low `COUNT_BITS`) plus a truncated `generation` tag (high
    /// 16 bits), in one word so a reader whose snapshot went stale after
    /// validation fails its ratchet CAS rather than overwriting a fresh reset.
    count_and_epoch: AtomicU64,
    /// Bumped by every `start()` so the HAL discards cached timeline maths.
    seed: AtomicU64,
    /// Seqlock: even is stable, odd is a writer mid-transaction. Opened by CAS,
    /// not `fetch_add`, because `StartIO` and a rate change can both call `start`.
    generation: AtomicU64,
    /// The triple last returned (`sample_time` as bits): the floor for the same
    /// seed, and the fallback on `RETRY_LIMIT` exhaustion. A repeat cannot decrease.
    last_output_sample_time_bits: AtomicU64,
    last_output_host_time: AtomicU64,
    last_output_seed: AtomicU64,
    /// Hash of the first reader's `ThreadId` (0 = unseen); `last_output_*` is sound
    /// only with one IO thread per device. Debug-only so release builds carry no trace.
    #[cfg(debug_assertions)]
    reader_thread_tag: AtomicU64,
}

impl Clock {
    pub const fn new() -> Self {
        Self {
            anchor_ticks: AtomicU64::new(0),
            // A pre-`start()` read breaks the contract; the next `start()` resets the count.
            ticks_per_period: AtomicU64::new(1),
            count_and_epoch: AtomicU64::new(pack(0, 0)),
            seed: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            // `host_time == 0` is the never-returned sentinel `zero_timestamp`'s fallback checks.
            last_output_sample_time_bits: AtomicU64::new(0),
            last_output_host_time: AtomicU64::new(0),
            last_output_seed: AtomicU64::new(1),
            #[cfg(debug_assertions)]
            reader_thread_tag: AtomicU64::new(0),
        }
    }

    /// Takes the seqlock write side (CAS even to odd, yielding every `WRITE_SPIN_LIMIT`
    /// spins); returns the even value replaced. No give-up path: interleaved writers race.
    fn acquire_write(&self) -> u64 {
        let mut spins: u32 = 0;
        loop {
            let g = self.generation.load(Ordering::Relaxed);
            // Relaxed: this only claims the odd generation; the caller's fence orders its stores.
            if g.is_multiple_of(2)
                && self
                    .generation
                    .compare_exchange_weak(
                        g,
                        g.wrapping_add(1),
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
            {
                return g;
            }
            spins += 1;
            if spins >= WRITE_SPIN_LIMIT {
                std::thread::yield_now();
                spins = 0;
            } else {
                std::hint::spin_loop();
            }
        }
    }

    /// Restarts the timeline at `now_ticks` for `rate` and bumps the seed in one
    /// seqlock transaction, so the reset count is never seen under the old seed.
    /// A zero `rate` or `ns_per_tick` saturates the period; callers validate the rate first.
    pub fn start(&self, now_ticks: u64, rate: f64, ns_per_tick: f64) {
        let period_ns = RING_FRAMES as f64 / rate * 1.0e9;
        let ticks = (period_ns / ns_per_tick).max(1.0) as u64;

        // The fence, not Release on the CAS, keeps the field stores after the odd transition.
        let g = self.acquire_write(); // generation: g -> g+1 (odd), exclusively ours
        std::sync::atomic::fence(Ordering::Release);
        self.ticks_per_period.store(ticks, Ordering::Relaxed);
        self.anchor_ticks.store(now_ticks, Ordering::Relaxed);
        let new_gen = g.wrapping_add(2);
        self.count_and_epoch
            .store(pack(new_gen as u16, 0), Ordering::Relaxed);
        self.seed.fetch_add(1, Ordering::Relaxed);
        // Release publishes every store above to a reader's `fence(Acquire)`.
        self.generation.store(new_gen, Ordering::Release);
    }

    /// Returns `(sample_time, host_time, seed)` for the current period. Never
    /// decreases within a seed, whatever `now_ticks` does or `start()` races it.
    pub fn zero_timestamp(&self, now_ticks: u64) -> (f64, u64, u64) {
        #[cfg(debug_assertions)]
        self.check_single_reader_thread();

        let mut accepted = None;

        for _ in 0..RETRY_LIMIT {
            let g1 = self.generation.load(Ordering::Acquire);
            if !g1.is_multiple_of(2) {
                std::hint::spin_loop();
                continue; // writer mid-transaction
            }
            let anchor = self.anchor_ticks.load(Ordering::Relaxed);
            let per = self.ticks_per_period.load(Ordering::Relaxed).max(1);
            let seed_val = self.seed.load(Ordering::Relaxed);
            // A fence, not Acquire on the load below, keeps the three reads above the re-check.
            std::sync::atomic::fence(Ordering::Acquire);
            let g2 = self.generation.load(Ordering::Relaxed);
            if g1 != g2 {
                std::hint::spin_loop();
                continue;
            }

            let elapsed = now_ticks.saturating_sub(anchor);
            // Clamped so the stored count and the returned `n` cannot diverge past 2^48.
            let want = (elapsed / per).min(COUNT_MASK);
            let epoch_tag = g1 as u16;

            // A re-tag by `start()` fails the CAS and sends the outer loop round again.
            let mut packed = self.count_and_epoch.load(Ordering::Acquire);
            let mut n = None;
            for _ in 0..RETRY_LIMIT {
                let (tag, cur) = unpack(packed);
                if tag != epoch_tag {
                    break; // our snapshot is already stale; bail, don't guess
                }
                if want <= cur {
                    n = Some(cur);
                    break;
                }
                // AcqRel/Acquire are stronger than needed: the protection is
                // the CAS's atomicity on the whole word, not its ordering.
                match self.count_and_epoch.compare_exchange_weak(
                    packed,
                    pack(tag, want),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        n = Some(want);
                        break;
                    }
                    Err(actual) => packed = actual,
                }
            }

            let Some(n) = n else {
                std::hint::spin_loop();
                continue;
            };

            let sample_time = (n as f64) * (RING_FRAMES as f64);
            let host_time = anchor.saturating_add(n.saturating_mul(per));
            accepted = Some((sample_time, host_time, seed_val));
            break;
        }

        // Retries exhausted: reuse the last output. A `0` host time is the
        // never-returned sentinel (boot time, to the HAL), so substitute `now_ticks`.
        let (sample_time, host_time, seed_val) = accepted.unwrap_or_else(|| {
            let cached_host_time = self.last_output_host_time.load(Ordering::Relaxed);
            (
                f64::from_bits(self.last_output_sample_time_bits.load(Ordering::Relaxed)),
                if cached_host_time == 0 {
                    now_ticks
                } else {
                    cached_host_time
                },
                self.last_output_seed.load(Ordering::Relaxed),
            )
        });

        // Never report less than the last value returned for the same seed.
        let last_seed = self.last_output_seed.load(Ordering::Relaxed);
        let last_sample_time =
            f64::from_bits(self.last_output_sample_time_bits.load(Ordering::Relaxed));
        let (sample_time, host_time, seed_val) =
            if seed_val == last_seed && sample_time < last_sample_time {
                (
                    last_sample_time,
                    self.last_output_host_time.load(Ordering::Relaxed),
                    last_seed,
                )
            } else {
                (sample_time, host_time, seed_val)
            };

        self.last_output_sample_time_bits
            .store(sample_time.to_bits(), Ordering::Relaxed);
        self.last_output_host_time
            .store(host_time, Ordering::Relaxed);
        self.last_output_seed.store(seed_val, Ordering::Relaxed);

        (sample_time, host_time, seed_val)
    }

    /// Records the first caller's thread and asserts later callers match; call site gated too.
    #[cfg(debug_assertions)]
    fn check_single_reader_thread(&self) {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::thread::current().id().hash(&mut hasher);
        let tag = match hasher.finish() {
            0 => 1, // 0 is the "unset" sentinel; nudge a real hash off it
            h => h,
        };
        match self
            .reader_thread_tag
            .compare_exchange(0, tag, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => {} // first call for this Clock: recorded
            Err(prev) => debug_assert_eq!(
                prev, tag,
                "Clock::zero_timestamp called from more than one thread for the same device \
                 — violates the single-IO-thread invariant last_output_* relies on"
            ),
        }
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

/// One clock per device in `device::DEVICES`, indexed by device index, not
/// `AudioObjectID`: resolve through `device::find_by_object_id` first.
pub static CLOCKS: [Clock; 5] = [
    Clock::new(),
    Clock::new(),
    Clock::new(),
    Clock::new(),
    Clock::new(),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn clk() -> Clock {
        let c = Clock::new();
        c.start(0, 48_000.0, 1.0); // 1 tick = 1 ns for arithmetic clarity
        c
    }

    #[test]
    fn sample_time_never_decreases_under_adversarial_input() {
        let c = clk();
        let mut last = f64::NEG_INFINITY;
        let mut x: u64 = 12345;
        for _ in 0..1_000_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let now = x % 60_000_000_000; // up to 60s of ns, deliberately unordered
            let (st, _ht, _seed) = c.zero_timestamp(now);
            assert!(st >= last, "sample time went backwards: {st} < {last}");
            last = st;
        }
    }

    #[test]
    fn advances_in_ring_frames_increments() {
        let c = clk();
        let period_ns = (RING_FRAMES as f64 / 48_000.0 * 1e9) as u64;
        let (a, _, _) = c.zero_timestamp(0);
        let (b, _, _) = c.zero_timestamp(period_ns * 3);
        assert_eq!(a, 0.0);
        assert!(
            b >= RING_FRAMES as f64,
            "expected at least one period, got {b}"
        );
        assert_eq!(
            b % RING_FRAMES as f64,
            0.0,
            "must land on a period boundary"
        );
    }

    #[test]
    fn seed_is_stable_in_steady_state() {
        let c = clk();
        let (_, _, s1) = c.zero_timestamp(1_000);
        let (_, _, s2) = c.zero_timestamp(2_000_000_000);
        assert_eq!(s1, s2, "seed must not change during steady state");
    }

    #[test]
    fn restart_resets_the_timeline_and_bumps_the_seed() {
        let c = clk();
        let (_, _, s1) = c.zero_timestamp(5_000_000_000);
        c.start(0, 96_000.0, 1.0);
        let (st, _, s2) = c.zero_timestamp(0);
        assert_eq!(st, 0.0, "timeline must restart from zero");
        assert_ne!(
            s1, s2,
            "seed must change so the HAL discards its cached timeline"
        );
    }

    #[test]
    fn clocks_array_has_one_entry_per_device() {
        assert_eq!(CLOCKS.len(), super::super::device::DEVICES.len());
    }

    /// Leaves `generation` stuck odd so every attempt falls through to the sentinel.
    #[test]
    fn retry_exhaustion_on_the_first_ever_call_substitutes_now_for_a_zero_host_time() {
        let c = Clock::new();
        c.generation.store(1, Ordering::Relaxed); // stuck "mid-transaction"
        let now = 123_456_789u64;
        let (_, host_time, _seed) = c.zero_timestamp(now);
        assert_eq!(
            host_time, now,
            "a zero host_time sentinel must not leak to the HAL on the first-ever call"
        );
    }

    /// Also checks that every in-epoch advance implies a period some writer here uses.
    #[test]
    fn concurrent_rate_changes_never_decrease_or_jump_ahead_within_a_seed_epoch() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        // Synthetic rates: microsecond periods let the count ratchet inside one epoch.
        const RATES: [f64; 4] = [1.0e10, 2.0e10, 5.0e9, 4.0e10];
        let legit_periods: Vec<u64> = RATES.iter().map(|&r| expected_period_ticks(r)).collect();

        let epoch = Instant::now();
        let now_ticks = || epoch.elapsed().as_nanos() as u64;

        let c = Arc::new(Clock::new());
        c.start(now_ticks(), RATES[0], 1.0);

        let stop = Arc::new(AtomicBool::new(false));

        let writer_c = Arc::clone(&c);
        let writer_stop = Arc::clone(&stop);
        let writer = std::thread::spawn(move || {
            let mut i = 0usize;
            while !writer_stop.load(Ordering::Relaxed) {
                // Not `RATES[..]`: `engine::mod` denies `clippy::indexing_slicing`.
                let rate = RATES.get(i % RATES.len()).copied().unwrap_or(RATES[0]);
                writer_c.start(epoch.elapsed().as_nanos() as u64, rate, 1.0);
                i = i.wrapping_add(1);
            }
        });

        let mut violations = 0u32;
        let mut forward_violations = 0u32;
        let mut torn_violations = 0u32;
        let mut baseline: Option<(u64, f64)> = None; // (seed, last sample_time)
        let mut epoch_track: Option<(u64, u64, u64)> = None; // (seed, n, host_time)
        while epoch.elapsed() < Duration::from_millis(300) {
            let now_before = now_ticks();
            let (st, ht, seed) = c.zero_timestamp(now_before);
            let now_after = now_ticks();

            if forward_error(ht, now_after) {
                forward_violations += 1;
            }

            let n = (st / RING_FRAMES as f64).round() as u64;
            if let Some((last_seed, last_n, last_ht)) = epoch_track {
                if last_seed == seed
                    && n > last_n
                    && !implied_period_is_legitimate(ht, last_ht, n, last_n, &legit_periods)
                {
                    torn_violations += 1;
                }
            }
            epoch_track = Some((seed, n, ht));

            if let Some((last_seed, last_st)) = baseline {
                if last_seed == seed && st < last_st {
                    violations += 1;
                }
            }
            baseline = Some((seed, st));
        }

        stop.store(true, Ordering::Relaxed);
        let _ = writer.join();

        assert_eq!(
            violations, 0,
            "sample time decreased within a seed epoch {violations} time(s)"
        );
        assert_eq!(
            forward_violations, 0,
            "host_time exceeded now_ticks {forward_violations} time(s) — a forward error"
        );
        assert_eq!(
            torn_violations, 0,
            "host_time advanced by a period no writer here uses {torn_violations} time(s) — a torn snapshot"
        );
    }

    /// `host_time > now` is exactly the `n > want` a stale ratchet produces. Callers pass
    /// a post-call reading: a mid-call `start()` can make `anchor` newer than the value passed.
    fn forward_error(host_time: u64, now_ticks: u64) -> bool {
        host_time > now_ticks
    }

    /// Mirrors `Clock::start`'s period formula (ticks are nanoseconds here).
    fn expected_period_ticks(rate: f64) -> u64 {
        (RING_FRAMES as f64 / rate * 1.0e9).max(1.0) as u64
    }

    /// Within one seed `host_time` must advance by exactly `(n - last_n) * per` for some
    /// legitimate `per`; a torn (old-anchor, new-period) mix fails this; `forward_error` cannot.
    fn implied_period_is_legitimate(
        host_time: u64,
        last_host_time: u64,
        n: u64,
        last_n: u64,
        legit_periods: &[u64],
    ) -> bool {
        let diff_n = n.saturating_sub(last_n);
        if diff_n == 0 {
            return true; // nothing advanced; no ratio to check
        }
        let diff_ht = host_time.saturating_sub(last_host_time);
        legit_periods
            .iter()
            .any(|&p| diff_n.checked_mul(p) == Some(diff_ht))
    }

    /// Two writers racing each other, as `StartIO` and a rate change can.
    #[test]
    fn concurrent_multiple_writers_never_tear_the_snapshot() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        const W1_RATES: [f64; 4] = [1.0e10, 2.0e10, 5.0e9, 4.0e10];
        const W2_RATES: [f64; 4] = [3.0e10, 6.0e9, 1.5e10, 4.5e10];
        let legit_periods: Vec<u64> = W1_RATES
            .iter()
            .chain(W2_RATES.iter())
            .map(|&r| expected_period_ticks(r))
            .collect();

        let epoch = Instant::now();
        let now_ticks = || epoch.elapsed().as_nanos() as u64;

        let c = Arc::new(Clock::new());
        c.start(now_ticks(), W1_RATES[0], 1.0);

        let stop = Arc::new(AtomicBool::new(false));

        let spawn_writer = |rates: [f64; 4]| {
            let c = Arc::clone(&c);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut i = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let rate = rates.get(i % rates.len()).copied().unwrap_or(rates[0]);
                    c.start(epoch.elapsed().as_nanos() as u64, rate, 1.0);
                    i = i.wrapping_add(1);
                }
            })
        };
        // Different rate cycles, so a torn mix is distinguishable from either writer's own.
        let w1 = spawn_writer(W1_RATES);
        let w2 = spawn_writer(W2_RATES);

        let mut violations = 0u32;
        let mut forward_violations = 0u32;
        let mut torn_violations = 0u32;
        let mut baseline: Option<(u64, f64)> = None;
        let mut epoch_track: Option<(u64, u64, u64)> = None; // (seed, n, host_time)
        while epoch.elapsed() < Duration::from_millis(300) {
            let now_before = now_ticks();
            let (st, ht, seed) = c.zero_timestamp(now_before);
            let now_after = now_ticks();

            if forward_error(ht, now_after) {
                forward_violations += 1;
            }

            let n = (st / RING_FRAMES as f64).round() as u64;
            if let Some((last_seed, last_n, last_ht)) = epoch_track {
                if last_seed == seed
                    && n > last_n
                    && !implied_period_is_legitimate(ht, last_ht, n, last_n, &legit_periods)
                {
                    torn_violations += 1;
                }
            }
            epoch_track = Some((seed, n, ht));

            if let Some((last_seed, last_st)) = baseline {
                if last_seed == seed && st < last_st {
                    violations += 1;
                }
            }
            baseline = Some((seed, st));
        }

        stop.store(true, Ordering::Relaxed);
        let _ = w1.join();
        let _ = w2.join();

        assert_eq!(
            violations, 0,
            "sample time decreased within a seed epoch {violations} time(s) under multi-writer contention"
        );
        assert_eq!(
            forward_violations, 0,
            "host_time exceeded now_ticks {forward_violations} time(s) under multi-writer contention — a forward error"
        );
        assert_eq!(
            torn_violations, 0,
            "host_time advanced by a period no writer here uses {torn_violations} time(s) under multi-writer contention — a torn snapshot"
        );
    }

    /// Writes the fields directly to reproduce the interleaving a blind `fetch_add` would
    /// permit (A stops short of closing, B's period lands, A closes); timing alone cannot.
    #[test]
    fn acquire_write_prevents_a_torn_snapshot() {
        let c = Clock::new();

        // Writer A, in `start`'s field order, stopping before the `generation` store.
        let anchor_a = 1_000_000u64;
        let per_a = expected_period_ticks(1.0e10);
        let tag_a: u16 = 2; // as if `acquire_write` returned 0 and A computed g+2
        c.ticks_per_period.store(per_a, Ordering::Relaxed);
        c.anchor_ticks.store(anchor_a, Ordering::Relaxed);
        c.count_and_epoch.store(pack(tag_a, 0), Ordering::Relaxed);
        c.seed.fetch_add(1, Ordering::Relaxed);

        // Writer B's stray period write lands in the gap.
        let per_b = expected_period_ticks(4.5e10);
        assert_ne!(
            per_a, per_b,
            "test setup: the two periods must differ or this proves nothing"
        );
        c.ticks_per_period.store(per_b, Ordering::Relaxed);

        // A closes; `generation` and the tag agree, but the period is B's.
        c.generation.store(tag_a as u64, Ordering::Release);

        let elapsed = 100_003u64; // arbitrary; chosen so the correct and
                                  // torn answers below land on different
                                  // host_time values, not a coincidental
                                  // common multiple of both periods
        let (_, host_time, _seed) = c.zero_timestamp(anchor_a + elapsed);

        let host_time_if_correct = anchor_a + (elapsed / per_a) * per_a;
        let host_time_if_torn = anchor_a + (elapsed / per_b) * per_b;
        assert_ne!(
            host_time_if_correct, host_time_if_torn,
            "test setup: the correct and torn answers must differ or this proves nothing"
        );
        assert_eq!(
            host_time, host_time_if_torn,
            "expected the torn ticks_per_period (per_b) to be the one actually used — \
             if this now fails, `zero_timestamp` may have started rejecting this manually \
             constructed state, which would need this test re-examined, not just re-asserted"
        );
    }
}
