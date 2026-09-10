//! Binds each device's `Ring` to the `WriteMix`/`ReadInput` IO operations;
//! `ffi::plugin` owns every raw pointer and hands over only safe values.
//! A `Ring` needs its channel count, so it cannot be a `const` static like
//! `device::STATE`: a `OnceLock` per device creates it once, from `StartIO`.

use super::device::DEVICES;
use super::ring::Ring;
use std::sync::OnceLock;

/// One slot per device index (never an `AudioObjectID`); empty until `ensure_ring` runs.
static RINGS: [OnceLock<Ring>; 5] = [
    OnceLock::new(),
    OnceLock::new(),
    OnceLock::new(),
    OnceLock::new(),
    OnceLock::new(),
];

/// The device's `Ring`, if `ensure_ring` has ever succeeded for it.
pub fn rings_for(dev: usize) -> Option<&'static Ring> {
    RINGS.get(dev).and_then(OnceLock::get)
}

/// The device's `Ring`, created on first call (allocates: control path only,
/// never `DoIOOperation`). Never zeroes it; `senda_StartIO` owns that decision.
pub fn ensure_ring(dev: usize) -> Option<&'static Ring> {
    let cfg = DEVICES.get(dev)?;
    let slot = RINGS.get(dev)?;
    Some(slot.get_or_init(|| Ring::new(cfg.channels as usize)))
}

/// Writes one `WriteMix` cycle at the HAL's own `sample_time`, the one timeline
/// every writer and reader share. No ring (IO never started): a silent no-op.
pub fn on_write_mix(dev: usize, sample_time: u64, src: &[f32], frames: usize) {
    if let Some(ring) = rings_for(dev) {
        ring.write(sample_time, src, frames);
    }
}

/// Reads one `ReadInput` cycle out of `dev`'s ring; with no ring, `dst` is left untouched.
pub fn on_read_input(dev: usize, sample_time: u64, dst: &mut [f32], frames: usize) {
    if let Some(ring) = rings_for(dev) {
        ring.read(sample_time, dst, frames);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `RINGS` is shared; index 1 is used only by this module, `ffi::plugin` uses 0, 2, 3, 4.
    const EXCLUSIVE_DEV: usize = 1;

    #[test]
    fn rings_for_is_none_until_ensure_ring_has_run_for_that_index() {
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
        if let (Some(a), Some(b)) = (first, second) {
            assert!(
                std::ptr::eq(a, b),
                "ensure_ring must not reallocate on a later call"
            );
        }

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
        // Every in-range index gets a ring from some test; 5 is structurally always `None`.
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
