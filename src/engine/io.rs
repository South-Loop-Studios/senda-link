//! Binds each device's `Ring` to the two IO operations the HAL actually
//! drives this driver through (`WriteMix`/`ReadInput` — see
//! `ffi::plugin::senda_WillDoIOOperation`).
//!
//! This module is deliberately thin. `ffi::plugin::senda_DoIOOperation` owns
//! every raw pointer the HAL hands over (`io_main_buffer`, the
//! `AudioServerPlugInIOCycleInfo*`) and is the only place that dereferences
//! them — by the time anything here runs, a device index has already been
//! resolved from an `AudioObjectID` (`device::find_by_object_id`), the
//! sample time has already been read out of the cycle info as a plain `u64`,
//! and the audio buffer has already become a safe `&[f32]`/`&mut [f32]`. That
//! split is what lets this module (like the rest of `engine::`) stay
//! `#![forbid(unsafe_code)]`.
//!
//! ## Why a `Ring` per device isn't a `const`-initialised `static`
//!
//! `device::STATE`/`clock::CLOCKS` can be plain `static` arrays of `const
//! fn new()` values because every field they hold is independent of a
//! device's channel count. `Ring::new` is not: it needs `channels`
//! (`device::DEVICES[dev].channels`, known only at runtime, and different
//! per device — 2/8/16/32/128), so a `Ring` can't be built in a `const`
//! context the way its siblings are. `std::sync::OnceLock` (`std`, so this
//! introduces no new dependency) covers exactly this shape: a fixed-size
//! array of empty slots that each initialise at most once, on whichever
//! thread gets there first, and hand out a `&'static Ring` good for the rest
//! of the process's life after that — which is what lets
//! `senda_DoIOOperation`'s real-time callers read a reference to one without
//! synchronising on anything themselves.
//!
//! ## When a `Ring` is created, and why there is no "destroy"
//!
//! `ensure_ring` is called from `ffi::plugin::senda_StartIO`, and only on
//! the transition from zero attached clients to one — allocation is fine
//! there (`StartIO` is a control-thread call, not the real-time IO path),
//! but it would defeat the purpose of `OnceLock` to reallocate on every
//! client that merely attaches to an already-running device. There is no
//! matching "free" function: `senda_DestroyDevice` always returns
//! `ERR_UNSUPPORTED` in this driver (the five devices are published
//! statically and the HAL never actually destroys one — see
//! `ffi::plugin`'s own module doc comment), so in practice a `Ring`, once
//! created, simply lives for the rest of the process's lifetime, exactly
//! like `device::STATE`/`clock::CLOCKS` already do.
//!
//! Because a `Ring` outlives any one client session, its previous
//! occupant's audio must not leak into a new one — `ensure_ring` only
//! *creates* a `Ring`, it never wipes it, so `senda_StartIO`'s own
//! transition handling is responsible for calling `Ring::zero` itself once
//! it has one (whether freshly created or reused from an earlier session).
//! `Ring::zero`'s own doc comment spells out why that call must come from a
//! control-thread context with no writer concurrently active — never from
//! this module, and never from the real-time `on_write_mix`/`on_read_input`
//! path below.
//!
//! When no `Ring` exists for a device, `on_write_mix`/`on_read_input`
//! return silently: nothing is written, read or counted.

use super::device::DEVICES;
use super::ring::Ring;
use std::sync::OnceLock;

/// One slot per device in `device::DEVICES`/`device::STATE`/`clock::CLOCKS`,
/// indexed identically — a device index, never an `AudioObjectID` directly.
/// Empty until `ensure_ring` first succeeds for that index.
static RINGS: [OnceLock<Ring>; 5] = [
    OnceLock::new(),
    OnceLock::new(),
    OnceLock::new(),
    OnceLock::new(),
    OnceLock::new(),
];

/// The device index's `Ring`, if one has ever been created for it
/// (`ensure_ring` has succeeded for it at least once). `None` for a device
/// that has never started IO, or for `dev` outside `0..DEVICES.len()`.
pub fn rings_for(dev: usize) -> Option<&'static Ring> {
    RINGS.get(dev).and_then(OnceLock::get)
}

/// Returns the device index's `Ring`, creating one sized to that device's
/// channel count if this is the first call for it. `None` only if `dev` is
/// out of range — never a transient failure a caller should retry.
///
/// Does NOT zero the ring, on creation or on any later call: a fresh
/// `Ring::new` already reads as "nothing written" (every block's `stamp`
/// starts at `NO_STAMP`), so there is nothing to zero the first time, and a
/// reused `Ring` from an earlier session needs its callers to decide *when*
/// stale audio must be wiped (see `Ring::zero`'s own RT-safety and
/// writer-concurrency preconditions) — that decision belongs to whoever
/// knows the session boundary (`ffi::plugin::senda_StartIO`'s client-count
/// transition, or a sample-rate change), not to this lookup.
///
/// Not real-time safe on first use for a given device (allocates). Callers
/// must only reach this from a control-path entry point — never from
/// `on_write_mix`/`on_read_input` or anything else on the `DoIOOperation`
/// hot path.
pub fn ensure_ring(dev: usize) -> Option<&'static Ring> {
    let cfg = DEVICES.get(dev)?;
    let slot = RINGS.get(dev)?;
    Some(slot.get_or_init(|| Ring::new(cfg.channels as usize)))
}

/// Routes one `WriteMix` IO cycle's audio into `dev`'s ring, indexed by the
/// HAL's own `sample_time` for this cycle (`AudioServerPlugInIOCycleInfo`'s
/// `output_time.sample_time`, read by the caller). The HAL's own sample
/// time is the index because it is the one timeline every writer and
/// reader of a device shares: each cycle's audio lands in the slot for the
/// time it was rendered for, so a reader asking for that time gets that
/// audio rather than whatever happened to be written most recently.
///
/// A no-op if `dev` has never started IO (`rings_for` returns `None`): the
/// HAL is not supposed to call `DoIOOperation` for a device before
/// `StartIO`, but silently dropping the audio is the safe, real-time-safe
/// fallback if it ever happened anyway — matching `Ring::write`'s own
/// reject-rather-than-panic stance on a call it cannot honour.
pub fn on_write_mix(dev: usize, sample_time: u64, src: &[f32], frames: usize) {
    if let Some(ring) = rings_for(dev) {
        ring.write(sample_time, src, frames);
    }
}

/// Routes one `ReadInput` IO cycle's audio out of `dev`'s ring, indexed by
/// the HAL's `input_time.sample_time` for this cycle. See `on_write_mix` for
/// the no-ring fallback; `dst` is simply left exactly as the caller passed
/// it in (whatever the host's own buffer already held) rather than this
/// module inventing what an unstarted device's input should look like.
pub fn on_read_input(dev: usize, sample_time: u64, dst: &mut [f32], frames: usize) {
    if let Some(ring) = rings_for(dev) {
        ring.read(sample_time, dst, frames);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `RINGS` is a shared `static` and tests run concurrently, so each test that
    // starts IO owns one index: 1 belongs here; `ffi::plugin` uses 0, 2, 3, 4.
    const EXCLUSIVE_DEV: usize = 1;

    #[test]
    fn rings_for_is_none_until_ensure_ring_has_run_for_that_index() {
        // Out-of-range indices are always `None`, with no shared state to
        // race — safe to check from any test.
        assert!(rings_for(5).is_none());
        assert!(rings_for(usize::MAX).is_none());
        assert!(ensure_ring(5).is_none());
    }

    #[test]
    fn ensure_ring_creates_a_ring_sized_to_the_devices_channel_count_and_is_idempotent() {
        assert!(
            rings_for(EXCLUSIVE_DEV).is_none(),
            "must be the first thing to touch this index"
        );
        assert!(
            DEVICES.len() > EXCLUSIVE_DEV,
            "setup invalid: EXCLUSIVE_DEV out of range"
        );
        let Some(cfg) = DEVICES.get(EXCLUSIVE_DEV) else {
            return;
        };
        let channels = cfg.channels as usize;

        let first = ensure_ring(EXCLUSIVE_DEV);
        assert!(first.is_some());
        let second = ensure_ring(EXCLUSIVE_DEV);
        assert!(second.is_some());
        // `OnceLock::get_or_init` only ever runs its initialiser once —
        // both calls must hand back the exact same instance, not two
        // independently allocated rings.
        if let (Some(a), Some(b)) = (first, second) {
            assert!(
                std::ptr::eq(a, b),
                "ensure_ring must not reallocate on a later call"
            );
        }

        // Sized correctly: a full-lap write/read round-trips exactly
        // `channels` samples per frame.
        assert!(
            rings_for(EXCLUSIVE_DEV).is_some(),
            "setup invalid: ensure_ring must have created a ring by now"
        );
        let src: Vec<f32> = (0..channels).map(|i| i as f32).collect();
        let mut dst = vec![9.0f32; channels];
        on_write_mix(EXCLUSIVE_DEV, 0, &src, 1);
        on_read_input(EXCLUSIVE_DEV, 0, &mut dst, 1);
        assert_eq!(dst, src);
    }

    #[test]
    fn on_write_mix_and_on_read_input_are_no_ops_when_rings_for_is_none() {
        // Every one of the 5 real device indices gets a `Ring` allocated
        // by SOME test somewhere in this crate (`ffi::plugin`'s own
        // `senda_StartIO` tests cover 0, 2, 3, 4; this module's own test
        // above covers 1) — there is no index in range that is
        // guaranteed to stay never-started for the whole test run. An
        // out-of-range index exercises the exact same code path
        // (`rings_for` returning `None`, see `on_write_mix`/
        // `on_read_input`'s single `if let Some(ring) = ...`) with no
        // shared state to race: `RINGS.get(5)` is structurally always
        // `None`, for every caller, forever.
        let mut dst = vec![9.0f32; 4];
        on_read_input(5, 0, &mut dst, 2);
        assert_eq!(
            dst,
            vec![9.0f32; 4],
            "a device with no ring must leave dst untouched"
        );
        on_write_mix(5, 0, &dst, 2); // must not panic either way
    }
}
