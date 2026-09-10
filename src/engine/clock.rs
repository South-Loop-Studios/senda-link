//! Monotonic host-time / zero-timestamp bookkeeping for `GetZeroTimeStamp`.
//!
//! The HAL calls `GetZeroTimeStamp` from an IO thread and expects back a
//! `(sampleTime, hostTime, seed)` triple describing the start of the current
//! zero-timestamp period. Two invariants matter more than anything else
//! here:
//!
//! 1. `sampleTime` must never go backwards, no matter what the underlying
//!    hardware clock (`mach_absolute_time`, read in `src/ffi/`) does. A
//!    clock that jumps, stalls, or repeats a reading must not desync the
//!    HAL's scheduling model.
//! 2. The advance period is always exactly `RING_FRAMES` frames — never a
//!    function of the client's negotiated IO buffer size. See the
//!    NON-NEGOTIABLE comment on `device::BLOCK`.
//!
//! Monotonicity has three independent sources — see the doc comment on
//! `zero_timestamp` for how they compose:
//!
//! - `count_and_epoch`, whose ratchet compare-exchange is tagged with a
//!   truncated `generation` value, so it can never successfully write a
//!   stale-epoch `want` into a newer epoch's slot — see the doc comment
//!   there. This is what actually makes `sample_time` immune to a
//!   concurrent `start()`, structurally rather than by narrowing a race
//!   window and hoping.
//! - `generation`, a seqlock protecting `anchor_ticks`/`ticks_per_period`/
//!   `seed` as one atomic snapshot, so a `start()` racing with a
//!   `zero_timestamp()` call can never hand back a torn mix of old and new
//!   fields. Its opening transition is a bounded compare-exchange (a real
//!   mutual-exclusion acquire), not a blind `fetch_add`, because `start`
//!   can race `start`: both `senda_StartIO` and `senda_SetPropertyData`
//!   (rate change) in `src/ffi/plugin.rs` call it, with no other
//!   driver-side mutual exclusion between them, so two concurrent writers
//!   are handled explicitly rather than assumed away.
//! - `last_output_*`, which `zero_timestamp` never reports less than for a
//!   given `seed` — the backstop that makes the guarantee unconditional
//!   rather than resting entirely on how tight the two mechanisms above
//!   are. This relies on a single-IO-thread-per-device invariant — see the
//!   doc comment on `last_output_sample_time_bits` and on `reader_thread_tag`.
//!
//! `mach_absolute_time`/`mach_timebase_info` are FFI calls, so they live in
//! `src/ffi/`. This module only ever receives already-read tick values as
//! plain `u64` parameters (`now_ticks` below) — it never touches the clock
//! itself, which is what keeps it free of `unsafe` and what makes the
//! adversarial-input property test below possible without mocking a syscall.

use super::device::RING_FRAMES;
use std::sync::atomic::{AtomicU64, Ordering};

/// Bounded retry count for the `generation` seqlock read and the
/// `count_and_epoch` ratchet loop in `zero_timestamp`, both on the
/// real-time IO thread. Same idiom, same value, as `engine::ring::Ring`'s
/// `RETRY_LIMIT`: an unbounded retry on this thread is as fatal as a wrong
/// answer.
const RETRY_LIMIT: u32 = 4;

/// Bounded busy-spin count for `acquire_write`'s compare-exchange, before
/// yielding to the scheduler. This is a genuine mutual-exclusion acquire
/// (see the doc comment on `acquire_write`), not a give-up-and-fall-back
/// read, so unlike `RETRY_LIMIT` this does not bound *whether* the lock is
/// eventually acquired — only how long it busy-spins per round before
/// backing off. `start` is a control-path call, not RT, so yielding under
/// contention (rather than falling back to something unsafe) is the right
/// trade-off here.
const WRITE_SPIN_LIMIT: u32 = 4;

/// Bit width given to the period count in `count_and_epoch`; the remaining
/// (high) 16 bits hold a truncated `generation` "epoch tag" — see the doc
/// comment on `count_and_epoch`. 48 bits of periods is ample headroom: at
/// the fastest synthetic test rate below (4.5e10 Hz, giving a ~728ns
/// period), `2^48` periods is `2^48 * 728ns ≈ 6.5 years` of continuous
/// operation before wraparound. Real audio rates advance far slower still
/// (`RING_FRAMES` periods are tens to hundreds of milliseconds), so the
/// margin in practice is many orders of magnitude larger.
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

/// Per-device monotonic zero-timestamp clock. One instance per device in
/// `device::DEVICES` — see `CLOCKS` below.
pub struct Clock {
    /// Tick value `start()` was called with — the origin of this clock's
    /// timeline. Protected by `generation` — see below.
    anchor_ticks: AtomicU64,
    /// Ticks per `RING_FRAMES`-frame period at the current sample rate.
    /// Protected by `generation` — see below.
    ticks_per_period: AtomicU64,
    /// Packs the period count (low `COUNT_BITS` bits, ratcheted upward by
    /// `zero_timestamp`) together with a truncated `generation` "epoch tag"
    /// (high 16 bits, set by every writer to `generation`'s own new value
    /// truncated to `u16`) into one atomic word.
    ///
    /// The tag exists because, even with `generation` closing the
    /// torn-snapshot and decrease failure modes (see the doc comment
    /// there), a snapshot a reader validated a moment ago can still go
    /// stale by the time its ratchet compare-exchange on the count
    /// executes, if a `start()` completes in that gap. With a bare,
    /// untagged count, that stale compare-exchange could succeed, writing
    /// an old-epoch `want` into a count `start()` had just reset to 0 for
    /// a new epoch; the count would jump ahead to `uptime / old_period`
    /// and stay there (nothing decreases it) until real elapsed time under
    /// the new epoch caught up — at real audio rates, a very long freeze.
    /// Detecting and retrying after the fact cannot undo a write that has
    /// already landed.
    ///
    /// Packing the tag into the same word the ratchet compare-exchanges
    /// closes this structurally: the reader supplies the entire observed
    /// packed word (tag and count together) as its expected value, and
    /// every writer re-tags the word as part of its own seqlock
    /// transaction, so a stale reader's compare-exchange fails cleanly,
    /// like any other losing CAS, instead of overwriting a fresh reset.
    ///
    /// A 16-bit truncated tag is enough. The only collision that would
    /// fool this check needs `generation` to advance by an exact multiple
    /// of 65536 between this reader validating `g1 == g2` and its very
    /// next load of `count_and_epoch` — one gap between two adjacent
    /// reads, not the whole call or its retry budget. Because the reader
    /// always re-loads the current word, a wrapped tag match alone would
    /// suffice to fool it, but 65536 writer transactions completing inside
    /// that single-load gap is not reachable even under this module's own
    /// adversarial stress tests.
    count_and_epoch: AtomicU64,
    /// Bumped whenever the timeline is reset (sample-rate change) so the
    /// HAL knows to discard any cached zero-timestamp math. Protected by
    /// `generation` — see below.
    seed: AtomicU64,
    /// Seqlock generation counter: even means "stable, safe to read";
    /// odd means "a writer is mid-transaction, retry". Same idiom as
    /// `engine::ring::Ring`'s per-block `seq`.
    ///
    /// Without this, two failure modes are reachable when `start()` (the
    /// control thread, on a sample-rate change) overlaps `zero_timestamp()`
    /// (the IO thread), because `anchor_ticks`/`ticks_per_period`/`seed`
    /// would be three independent atomics with no relationship enforced
    /// between them:
    ///
    /// 1. A reader could read the new `ticks_per_period` paired with the
    ///    stale `anchor_ticks` (each individually a valid Acquire/Release
    ///    read, but combined into a `want` that belongs to neither the old
    ///    nor the new epoch), computed under the old `seed`.
    /// 2. A reader could land after `start`'s reset of `count_and_epoch`
    ///    but before `seed` was bumped, observing `sample_time == 0` under
    ///    the seed that still describes the previous, non-zero-based epoch
    ///    — a visible decrease within what the caller still believes is
    ///    one epoch.
    ///
    /// Gating the read of `anchor_ticks`/`ticks_per_period`/`seed` on
    /// `generation` not having changed across the read closes both.
    ///
    /// Multiple writers: `start` can race `start`, because both
    /// `senda_StartIO` and `senda_SetPropertyData` (rate change) in
    /// `src/ffi/plugin.rs` call it, with no other driver-side mutual
    /// exclusion between them. A blind `fetch_add` to open the transaction
    /// would let two concurrent writers take `generation` from even to odd
    /// to even again (0→1→2) while both are still mid-transaction, so a
    /// reader validates an even generation across a genuinely torn read —
    /// anchor from one writer, period from the other. `acquire_write`
    /// (below) closes this with a real compare-exchange acquire instead.
    /// A validated snapshot going stale between validation and the count
    /// ratchet is a separate issue, closed on `count_and_epoch`; see the
    /// doc comment there.
    generation: AtomicU64,
    /// The exact `(sample_time, host_time, seed)` this function last
    /// returned, with `sample_time` stored as its bit pattern (`f64` has no
    /// native atomic). `zero_timestamp` never hands back less than this for
    /// the same `seed` — see the doc comment there. Repeating a value this
    /// function already returned is trivially non-decreasing, which is what
    /// makes this the correct place to enforce the guarantee unconditionally,
    /// rather than leaving it resting on `generation`/`count_and_epoch`
    /// timing alone. Also doubles as the fallback when `RETRY_LIMIT` is
    /// exhausted — "return the last committed values rather than spinning".
    ///
    /// This three-field read-modify-write is itself only sound under a
    /// single-reader-thread assumption — see `reader_thread_tag`, which
    /// promotes that assumption from an implicit comment to a checked
    /// invariant (debug builds only).
    last_output_sample_time_bits: AtomicU64,
    last_output_host_time: AtomicU64,
    last_output_seed: AtomicU64,
    /// Debug-only tripwire for the single-IO-thread-per-device invariant
    /// `last_output_*` rests on: the HAL drives each device's IO cycle from
    /// one IO thread, so `GetZeroTimeStamp` for a device is never called
    /// concurrently with itself. Were that false, the three `last_output_*`
    /// fields (individually `Relaxed`, not atomic as a triple) could tear
    /// across two callers and clamp a new epoch to a stale, bogus value.
    ///
    /// Holds a hash of the first caller's `std::thread::ThreadId`; 0 means
    /// "not yet observed" (`ThreadId::as_u64` is unstable as of Rust 1.98,
    /// `thread_id_value`, so a `DefaultHasher` digest stands in). Gated at
    /// the field level so a release build carries no trace of it. The tag
    /// is never reset and `CLOCKS` is a shared `static`, so tests must not
    /// call `zero_timestamp` on one device index from two threads — see
    /// the per-test device-index discipline in `engine::properties`.
    #[cfg(debug_assertions)]
    reader_thread_tag: AtomicU64,
}

impl Clock {
    pub const fn new() -> Self {
        Self {
            anchor_ticks: AtomicU64::new(0),
            // NOTE: a `zero_timestamp` call before the first `start()` (i.e.
            // before `StartIO`) is not something the CoreAudio contract
            // permits, but if it happened anyway, this default of `1` tick
            // per period would make `elapsed / 1` ratchet the count up to
            // whatever `now_ticks` is, immediately and enormously. That's
            // self-correcting — the very next `start()` unconditionally
            // resets `count_and_epoch` to 0 (tagged with the new epoch) as
            // part of its seqlock transaction — so it is a one-call
            // cosmetic wrong answer, never a lasting inconsistency.
            // Documented here rather than guarded against, since a guard
            // would just be re-stating "start() must be called first",
            // which is already the documented contract.
            ticks_per_period: AtomicU64::new(1),
            count_and_epoch: AtomicU64::new(pack(0, 0)),
            seed: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            // (0.0f64.to_bits(), 0, 1): the "nothing has ever been
            // returned yet" starting point. A `host_time` of `0` is not a
            // value `mach_absolute_time` could ever legitimately produce
            // (it reads as "at boot").
            //
            // This sentinel is observable: it persists until the first call
            // to `zero_timestamp` returns, and that first call can itself
            // be the one racing `start()`. The CoreAudio contract does not
            // require `StartIO` to have completed before the HAL calls
            // `GetZeroTimeStamp`, and a `SetPropertyData` rate change can
            // land at any time. If that first-ever call loses all
            // `RETRY_LIMIT` outer attempts, it falls through to this exact
            // untouched `(0.0, 0, 1)` triple: `hostTime = 0` (boot) under a
            // `seed` that never described a real epoch. `zero_timestamp`
            // substitutes the current tick reading for `host_time` in
            // exactly that case (see the comment there) rather than
            // handing the HAL a boot-time reading; every subsequent call
            // overwrites this sentinel with a real observation regardless.
            last_output_sample_time_bits: AtomicU64::new(0),
            last_output_host_time: AtomicU64::new(0),
            last_output_seed: AtomicU64::new(1),
            #[cfg(debug_assertions)]
            reader_thread_tag: AtomicU64::new(0),
        }
    }

    /// Acquires the seqlock's write side: compare-exchanges `generation`
    /// from its current even value to the next odd value, retrying if a
    /// concurrent writer's compare-exchange wins the race first, and
    /// yielding to the scheduler after a bounded number of busy-spins per
    /// round (`WRITE_SPIN_LIMIT`) rather than busy-waiting indefinitely.
    ///
    /// This is genuine mutual exclusion, not a best-effort read: unlike
    /// `zero_timestamp`'s bounded retry (which may safely give up and fall
    /// back to a cached value), giving up here and proceeding without the
    /// lock would let two writers interleave their field writes — an
    /// outright data race, not merely a torn read. So this loop has no
    /// "give up" exit; it only bounds how it waits, not whether it
    /// eventually succeeds. `start`'s own critical section is a handful of
    /// atomic stores, so under any realistic contention (including this
    /// module's own multi-writer stress test) this resolves in a handful
    /// of iterations.
    ///
    /// Returns the even value `generation` held at the moment this call
    /// won the race; the caller advances it by 1 again (to the next even
    /// value) to close the transaction.
    fn acquire_write(&self) -> u64 {
        let mut spins: u32 = 0;
        loop {
            let g = self.generation.load(Ordering::Relaxed);
            // Both orderings on this compare-exchange are `Relaxed`
            // deliberately. Winning it establishes only ownership of the
            // odd generation; the writer's own field stores are ordered
            // after it by the `fence(Release)` every caller issues next,
            // and readers validate `generation` around their loads. No
            // happens-before with the previous writer's field stores is
            // claimed or needed: that writer's closing
            // `generation.store(Release)` publishes them, and a new writer
            // overwrites every field anyway.
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

    /// (Re)starts the timeline and bumps the seed, as a single seqlock
    /// transaction: `now_ticks` becomes the new zero point, `count_and_epoch`
    /// resets to 0 tagged with the new epoch, the advance period is
    /// recomputed for `rate` (always `RING_FRAMES` frames' worth of ticks,
    /// never a function of any negotiated IO buffer size), and `seed`
    /// advances — all bracketed between `acquire_write`'s odd transition
    /// and a closing even store, so a concurrent `zero_timestamp` reader
    /// observes either the complete old state or the complete new state,
    /// never a mix, and a concurrent `start` call from another thread
    /// cannot interleave its own writes with this one (see the doc comment
    /// on `generation`).
    ///
    /// Uses the canonical seqlock fence idiom rather than plain
    /// Acquire/Release on the `generation` operations themselves: a
    /// `Release`-ordered store/RMW prevents preceding operations from
    /// being reordered after it, but does not prevent following
    /// operations (the field writes below) from being hoisted before it —
    /// that requires an explicit `fence(Release)` after the odd transition
    /// (and, symmetrically in `zero_timestamp`, a `fence(Acquire)` before
    /// the closing generation re-check rather than `Acquire` on that load
    /// itself). A plain-Release version would happen to work on
    /// x86-64/aarch64 because the real hardware barriers there are stronger
    /// than the abstract model requires, but that is correctness-by-target,
    /// not correctness-by-model.
    ///
    /// The seed bump is part of this transaction, not a follow-up step, so
    /// callers must not bump the seed separately afterwards: a separate
    /// bump would leave a window where the reset count is observable under
    /// the not-yet-bumped old `seed`.
    ///
    /// `rate` and `ns_per_tick` are accepted as any `f64`; a zero `rate` or
    /// `ns_per_tick` yields a saturated period (`inf as u64`), which is
    /// harmless because callers validate the rate first (against
    /// `device::SAMPLE_RATES`, in `engine::properties`). `ns_per_tick`
    /// converts mach ticks to nanoseconds (timebase numer/denom) — how many
    /// nanoseconds one tick represents.
    pub fn start(&self, now_ticks: u64, rate: f64, ns_per_tick: f64) {
        let period_ns = RING_FRAMES as f64 / rate * 1.0e9;
        let ticks = (period_ns / ns_per_tick).max(1.0) as u64;

        let g = self.acquire_write(); // generation: g -> g+1 (odd), exclusively ours
        std::sync::atomic::fence(Ordering::Release);
        self.ticks_per_period.store(ticks, Ordering::Relaxed);
        self.anchor_ticks.store(now_ticks, Ordering::Relaxed);
        let new_gen = g.wrapping_add(2);
        self.count_and_epoch
            .store(pack(new_gen as u16, 0), Ordering::Relaxed);
        self.seed.fetch_add(1, Ordering::Relaxed);
        // Release: publishes every write above as visible-before this
        // store to any thread that later Acquire-observes it (directly or
        // via `zero_timestamp`'s `fence(Acquire)`).
        self.generation.store(new_gen, Ordering::Release);
    }

    /// Returns `(sample_time, host_time, seed)` for the current
    /// zero-timestamp period.
    ///
    /// Monotonic by construction, from four cooperating sources — the last
    /// is what makes it unconditional rather than "true unless something
    /// else races it":
    ///
    /// 1. The period count only ever increases, via `count_and_epoch`'s
    ///    bounded, epoch-tagged compare-exchange loop below, so
    ///    `sample_time` cannot move backwards regardless of what
    ///    `now_ticks` does (repeats, jumps forward, or goes "backwards"
    ///    relative to a previous call).
    /// 2. `anchor_ticks`/`ticks_per_period`/`seed` are read as one seqlock
    ///    snapshot gated on `generation` (itself multi-writer-safe — see
    ///    the doc comment there), so a concurrent `start()` can never
    ///    produce a torn (old anchor, new period) mix, nor let the
    ///    freshly-reset count surface under the not-yet-bumped old `seed`.
    /// 3. A validated snapshot can still go stale between validation and
    ///    this function's own ratchet, if a `start()` completes in that
    ///    gap. The ratchet's compare-exchange supplies the entire packed
    ///    `count_and_epoch` word it observed, and every `start()` re-tags
    ///    that word, so the stale compare-exchange simply fails rather
    ///    than overwriting a fresh reset with old-epoch arithmetic (see
    ///    the doc comment on `count_and_epoch`).
    /// 4. Whatever (1)-(3) produce on any single call, the value about to
    ///    be returned is compared against `last_output_*` for the same
    ///    `seed` and clamped up to it if it would otherwise be lower.
    ///    Repeating a value already returned is trivially non-decreasing.
    ///    This backstop rests on the single-reader-thread assumption
    ///    checked by `reader_thread_tag`.
    ///
    /// The retry loop is bounded (`RETRY_LIMIT`) because this runs on the
    /// real-time IO thread: spinning without bound on a stalled writer
    /// would be as fatal as a wrong answer. On exhaustion (pathological
    /// contention only — `start`'s critical section is a handful of atomic
    /// stores), this reuses `last_output_*` verbatim as the return value —
    /// "return the last committed values rather than spinning" — rather
    /// than attempt a fresh ratchet from a snapshot this call could not
    /// confirm is current.
    ///
    /// A `count_and_epoch` tag mismatch is also an outer-retry trigger.
    /// Under a burst of `start()` calls landing back-to-back, each one
    /// advancing `generation` before this call's inner ratchet finishes, a
    /// call can exhaust all `RETRY_LIMIT` outer attempts on tag mismatches
    /// alone and fall through to the `last_output_*` fallback, which still
    /// carries the pre-burst `seed`: the HAL is then told the epoch has
    /// not changed when it has. `sample_time` still never decreases (point
    /// 4 holds unconditionally), and this driver's real `start()` call
    /// sites — `StartIO` and a user-initiated sample-rate change — occur at
    /// human timescales, nowhere near that burst rate, so this is an
    /// accuracy concern under synthetic contention, not a safety one under
    /// real HAL usage.
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
            // Fence (not `Ordering::Acquire` on the load below) so the
            // three reads above cannot be reordered past this point — see
            // the fence-idiom note on `start`.
            std::sync::atomic::fence(Ordering::Acquire);
            let g2 = self.generation.load(Ordering::Relaxed);
            if g1 != g2 {
                std::hint::spin_loop();
                continue;
            }

            let elapsed = now_ticks.saturating_sub(anchor);
            // `pack` masks its `count` argument to `COUNT_MASK` (48 bits)
            // regardless, but leaving `want` itself unmasked here would let
            // the stored count and the returned `n`/`sample_time` silently
            // diverge for `want >= 2^48`. Only reachable via the documented
            // pre-`start()` default `ticks_per_period == 1` (see
            // `Clock::new()`), where `want` is raw mach ticks — crossing
            // 2^48 needs roughly 135 days of uptime on a 24MHz timebase, so
            // reachable in principle on a long-running Mac. Monotonicity is
            // unaffected either way (a clamped `want` only ratchets
            // `count_and_epoch` less far, never backwards), but clamping
            // here keeps what is stored and what is returned from ever
            // disagreeing.
            let want = (elapsed / per).min(COUNT_MASK);
            let epoch_tag = g1 as u16;

            // Ratchet `count_and_epoch` towards `want`, tagged to this
            // exact epoch — see the doc comment on `count_and_epoch` for
            // why this can never write stale-epoch arithmetic into a newer
            // epoch's slot. Bounded (RETRY_LIMIT), not an unbounded spin: a
            // concurrent `start()` re-tagging the word, or a spurious
            // `compare_exchange_weak` failure, can only make this loop
            // retry a bounded number of times before giving up on this
            // attempt (falling through to `n = None`, which sends the
            // outer loop around again with a freshly validated snapshot).
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
                // `AcqRel` on success and `Acquire` on failure are
                // conservative: stronger than the reasoning here needs.
                // The stale-epoch protection comes from the compare-
                // exchange's atomicity on the whole packed word, not from
                // its ordering, and the snapshot this ratchet depends on
                // was already made visible by the `fence(Acquire)` above.
                // Every writer-side store to this word is `Relaxed` inside
                // the seqlock transaction, so there is no release on it to
                // synchronise with. The cost is one acquire-release RMW on
                // the IO thread, negligible against a relaxed one.
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

        // Retries exhausted under pathological contention: reuse the last
        // output verbatim rather than spin longer or attempt a fresh
        // ratchet from a snapshot we can't confirm is current.
        //
        // `last_output_host_time` starts at `0` (see `Clock::new()`) and
        // only changes once a call reaches this point and stores a real
        // value below — so on the very first call this `Clock` ever
        // receives, this fallback can observe the untouched `0` sentinel
        // if that call itself loses all `RETRY_LIMIT` attempts (a
        // first-ever `GetZeroTimeStamp` racing a `SetPropertyData` rate
        // change, which the CoreAudio contract permits: `start()` need
        // only have been attempted, not completed, for the first read to
        // land here). `0` is not a `mach_absolute_time` value the HAL
        // could sensibly receive (it reads as "at boot"), so substitute
        // the current reading in that one case — still not a validated
        // snapshot, but a plausible host time, and it becomes the new
        // cached baseline via the store at the end of this function.
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

        // Point 4: never report a decrease relative to the last value this
        // function actually returned for the same seed, regardless of why
        // the raw computation above might otherwise have produced one.
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

    /// Debug-only check for the single-IO-thread-per-device invariant —
    /// see the doc comment on `reader_thread_tag`. Records the first
    /// caller's thread tag (compare-exchange from the `0` sentinel) and
    /// `debug_assert!`s that every later caller matches it. The call site
    /// in `zero_timestamp` is `#[cfg(debug_assertions)]`-gated too, so a
    /// release build pays nothing, not even the `compare_exchange`.
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

/// One clock per device in `device::DEVICES`/`device::STATE`, indexed
/// identically (device index, not `AudioObjectID` — callers must resolve
/// through `device::find_by_object_id` first, exactly as `STATE` requires).
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
            // xorshift — deterministic, no dependencies
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
        // `start` bumps the seed as part of its own transaction; see the
        // doc comment on `Clock::start`.
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

    /// Regression for the first-call fallback. Real concurrency cannot
    /// reliably force `zero_timestamp`'s `RETRY_LIMIT`-exhaustion fallback
    /// (it needs a writer stuck mid-transaction for the whole retry
    /// budget), so this uses `mod tests`'s private-field access (it is a
    /// child module of `clock`) to leave `generation` stuck odd on a brand
    /// new `Clock`, forcing every outer attempt to see "writer
    /// mid-transaction" and fall through to the untouched `(0.0, 0, 1)`
    /// sentinel from `Clock::new()`. Without the substitution in
    /// `zero_timestamp`, this returns `host_time == 0` — a
    /// `mach_absolute_time` reading no real boot could produce, handed to
    /// the HAL as if it were real.
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

    /// Also asserts no forward error (`count` jumping ahead of what real
    /// elapsed time under the current epoch could justify) — see
    /// `forward_error` below for the exact (not heuristic) predicate —
    /// and cross-checks `implied_period_is_legitimate`: within one seed
    /// epoch, the ratio between two successive advances of the period
    /// count and `host_time` must equal exactly one of the handful of
    /// periods any writer here could legitimately be using. See that
    /// helper's doc comment for why it specifically targets a torn
    /// (old-anchor, new-period) mix in a way `forward_error` alone does
    /// not.
    #[test]
    fn concurrent_rate_changes_never_decrease_or_jump_ahead_within_a_seed_epoch() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        // Real wall-clock nanoseconds (ns_per_tick == 1.0 below, so "ticks"
        // and nanoseconds coincide) rather than a shared racing counter: the
        // reader must see genuinely elapsed time within an epoch for the
        // count to ratchet up to something worth losing when a reset lands.
        //
        // The rates below are deliberately not real audio sample rates —
        // at 48kHz, one `RING_FRAMES`-period is ~683ms, far longer than
        // this whole test. These synthetic (and huge) rates shrink the
        // period to low-microsecond scale purely so the count has room to
        // ratchet up meaningfully inside one writer epoch (tens of
        // microseconds long, below) before the next reset — `Clock` itself
        // never validates `rate` against `device::SAMPLE_RATES` (that
        // happens one layer up, in `engine::properties`), so this is a
        // legitimate way to exercise the seqlock's timing-independent logic
        // fast.
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
                // `.get(..).copied().unwrap_or(...)` rather than `RATES[..]`
                // — `engine::mod` denies `clippy::indexing_slicing`, and
                // that lint propagates into `#[cfg(test)]` code too.
                let rate = RATES.get(i % RATES.len()).copied().unwrap_or(RATES[0]);
                // Single call: `start` bumps the seed as part of its own
                // seqlock transaction — see the doc comment on `Clock::start`.
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
            // Bound against a reading taken after the call returns, not
            // the one passed in — see the doc comment on `forward_error`
            // for why the "before" reading is not a safe bound under an
            // adversarial writer.
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

    /// Exact (not heuristic) forward-error predicate, with no wall-clock
    /// slack that could drift loose over time.
    ///
    /// `Clock::zero_timestamp` computes `host_time = anchor + n * per`.
    /// Inside a validated snapshot, `n` is bounded by
    /// `want = (now_ticks - anchor) / per` (integer division) on every
    /// path: the healthy ratchet caps `n` at `want` directly, and the clamp
    /// path (point 4 in `zero_timestamp`'s doc comment) and the
    /// `RETRY_LIMIT`-exhaustion fallback both repeat a `last_output_*`
    /// value already proven `<= now_ticks` at an earlier `now_ticks`, which
    /// is monotonic in this test (`Instant::elapsed()`). Since `per >= 1`,
    /// `n <= want` implies `anchor + n*per <= now_ticks`. The only way to
    /// violate `host_time <= now_ticks` is `n > want` — exactly the
    /// poisoned-count condition a stale ratchet landing after a reset
    /// produces (see the doc comment on `count_and_epoch`).
    ///
    /// One caveat, load-bearing for how call sites use it: the `now_ticks`
    /// passed to `Clock::zero_timestamp` is captured before the call, but
    /// the function validates against whatever `anchor`/`generation` are
    /// live during the call. Under this test's zero-delay adversarial
    /// writer a fresh `start()` can land in between, making the validated
    /// `anchor` legitimately newer than the passed-in `now_ticks`;
    /// `elapsed` saturates to `0`, `n` is `0`, and `host_time == anchor`
    /// exceeds the stale pre-call reading despite being correct. So callers
    /// check against a reading taken after `zero_timestamp` returns: real
    /// time is monotonic, so a post-call reading is `>=` anything the call
    /// could legitimately have observed internally.
    ///
    /// This predicate does not reliably detect a torn (old-anchor,
    /// new-period) multi-writer mix on its own: a torn pairing still
    /// satisfies `n <= (now_ticks - anchor_torn) / per_torn` for its own
    /// `anchor`/`per` — the inequality is a property of the formula, not
    /// of whether the pairing is legitimate. `implied_period_is_legitimate`
    /// targets the torn case specifically.
    fn forward_error(host_time: u64, now_ticks: u64) -> bool {
        host_time > now_ticks
    }

    /// The exact tick period `Clock::start` computes for `rate`, mirroring
    /// its internal formula (`ns_per_tick == 1.0` throughout these tests,
    /// so ticks and nanoseconds coincide).
    fn expected_period_ticks(rate: f64) -> u64 {
        (RING_FRAMES as f64 / rate * 1.0e9).max(1.0) as u64
    }

    /// `true` unless `host_time` advanced by an amount that is not an exact
    /// multiple of one of `legit_periods`, given the period count advanced
    /// by `n - last_n` — the check that catches a torn (old-anchor,
    /// new-period) multi-writer mix, which `forward_error` does not.
    ///
    /// Within one seed epoch (`last_seed == seed`, checked by the caller),
    /// `host_time = anchor + n*per` for a fixed `anchor`/`per` — that is
    /// the entire point of a seed identifying one epoch. So two readings
    /// under the same seed must satisfy exactly
    /// `host_time - last_host_time == (n - last_n) * per` for whichever
    /// `per` that epoch's `start()` call used. Every legitimate `per` any
    /// writer in a given test can produce is a known, finite, exactly
    /// representable integer (`expected_period_ticks` mirrors `Clock`'s own
    /// formula) — so this checks whether some legitimate `per` satisfies
    /// that equation exactly, not merely whether the numbers are
    /// "reasonable".
    ///
    /// This is what actually targets a torn snapshot, unlike
    /// `forward_error`: mixing an anchor from one writer's transaction with
    /// a period from another's does not, in general, advance `host_time`
    /// in exact lockstep with `n` at any one of the small number of
    /// legitimate rates in play — an exact-equality check across a
    /// enumerated few candidates has essentially no chance of a false
    /// match on a genuinely torn value, unlike a magnitude-based bound.
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

    /// `start` can race `start`: both `senda_StartIO` and
    /// `senda_SetPropertyData` (rate change) in `src/ffi/plugin.rs` call
    /// it, with no other driver-side mutual exclusion, so two writers can
    /// run concurrently against each other, not just against a reader.
    /// Two writer threads here, both hammering `start()`, exercise exactly
    /// that. With neither `acquire_write` nor the packed epoch tag in
    /// place, both `forward_error` and `implied_period_is_legitimate` trip
    /// tens to thousands of times per run.
    ///
    /// What this blind-concurrency test cannot do is isolate the need for
    /// `acquire_write` from the packed-tag fix: with the tag in place but
    /// `acquire_write` replaced by a blind `fetch_add`, neither check
    /// trips, even though the underlying data race (two writers' field
    /// writes interleaving with no exclusion) is real. An accepted torn
    /// read needs writer B's stray field write to land in the
    /// single-digit-nanosecond gap between writer A finishing its own
    /// `count_and_epoch`/`generation` writes and those becoming externally
    /// visible — any wider miss is rejected as a stale attempt and retried.
    /// `acquire_write_prevents_a_torn_snapshot` below reproduces that exact
    /// interleaving deterministically instead of relying on timing luck.
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
        // Two independent writer threads, deliberately different rate
        // cycles so a torn mix is distinguishable from either writer's own
        // legitimate sequence.
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

    /// Deterministic (non-probabilistic) proof that `acquire_write`'s
    /// mutual exclusion is necessary — a companion to
    /// `concurrent_multiple_writers_never_tear_the_snapshot`, whose doc
    /// comment explains why blind concurrent stress testing could not
    /// reliably reproduce this in isolation from the packed-tag fix.
    ///
    /// `mod tests` is a child module of `clock`, so — like any submodule
    /// in Rust — it can reach `Clock`'s private fields directly. This test
    /// uses that access to manually perform the exact field-write sequence
    /// `start()` uses, but stops writer A one field short of closing its
    /// own transaction, lets a stray writer-B-shaped field write land in
    /// that gap, and only then lets A close — exactly the interleaving a
    /// blind `fetch_add` (no mutual exclusion) permits, since writer B's
    /// own `generation` update would not need to wait for A's transaction
    /// to finish. With `acquire_write`'s compare-exchange in place, this
    /// interleaving cannot happen for real: B's own attempt to open a
    /// transaction spins until `generation` is even again, which it is not
    /// until A's `generation.store` (the very last step) has run — so the
    /// gap this test manually threads a write through cannot exist between
    /// two calls that actually go through `acquire_write`. This test does
    /// not call `acquire_write` at all; it reproduces the raw field state
    /// a bypass of it would produce, and shows `zero_timestamp` reports it
    /// exactly as torn.
    #[test]
    fn acquire_write_prevents_a_torn_snapshot() {
        let c = Clock::new();

        // Writer A's transaction, in `start`'s own field order, stopping
        // one field short of closing it (no `generation` store yet).
        let anchor_a = 1_000_000u64;
        let per_a = expected_period_ticks(1.0e10);
        let tag_a: u16 = 2; // as if `acquire_write` returned 0 and A computed g+2
        c.ticks_per_period.store(per_a, Ordering::Relaxed);
        c.anchor_ticks.store(anchor_a, Ordering::Relaxed);
        c.count_and_epoch.store(pack(tag_a, 0), Ordering::Relaxed);
        c.seed.fetch_add(1, Ordering::Relaxed);

        // Writer B's stray field write lands here — exactly what a blind
        // `fetch_add` (no mutual exclusion) permits, since B's own
        // `generation` update does not wait for A to close. B never
        // finishes its own transaction in this reproduction; one torn
        // field write from it is enough to demonstrate the mechanism.
        let per_b = expected_period_ticks(4.5e10);
        assert_ne!(
            per_a, per_b,
            "test setup: the two periods must differ or this proves nothing"
        );
        c.ticks_per_period.store(per_b, Ordering::Relaxed);

        // A resumes and closes its transaction. `generation` now matches
        // `count_and_epoch`'s tag (both say "A") — a reader validates this
        // as a stable, trustworthy snapshot — but `ticks_per_period` is
        // B's, not A's: the torn snapshot `acquire_write` exists to
        // prevent.
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
