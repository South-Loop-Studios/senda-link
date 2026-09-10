//! Static device table and per-device mutable state.
//!
//! Five devices ship in one bundle: 2/8/16/32/128 channels. Everything here
//! is a compile-time constant table plus a fixed-size array of atomics — no
//! `Vec`, no `Mutex`, no allocation, matching the "safe Rust only" contract
//! in `engine::mod`.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Object ID of the plugin (`kAudioObjectPlugInObject`), the root object
/// every device is owned by. Fixed by the HAL's object model, not chosen by
/// us.
pub const PLUGIN_OBJECT_ID: u32 = 1;

pub const SAMPLE_RATES: [f64; 6] = [44100.0, 48000.0, 88200.0, 96000.0, 176400.0, 192000.0];
pub const DEFAULT_SAMPLE_RATE: f64 = 48000.0;

/// IO block size in frames. Not the same thing as `ZeroTimeStampPeriod` —
/// see `RING_FRAMES` below and `engine::properties::zero_timestamp_period`.
///
/// Non-negotiable: a settable buffer size (see `PerDevice::buffer_frames`,
/// which backs the `'fsiz'` / `BufferFrameSize` property) must never be
/// implemented by making this constant variable. `RING_FRAMES = BLOCK *
/// RING_BLOCKS`, so a variable `BLOCK` would make `ZeroTimeStampPeriod` a
/// function of the client's requested buffer size — reintroducing the exact
/// defect this project exists to fix. The negotiated buffer size lives in
/// `PerDevice` and touches nothing in this ring geometry.
pub const BLOCK: usize = 512;
pub const RING_BLOCKS: usize = 64;

/// Range of buffer sizes (in frames) this driver accepts via `SetPropertyData`
/// on `'fsiz'`. Purely a per-`PerDevice` negotiated value reported back to
/// the host — see the non-negotiable note above.
pub const MIN_BUFFER_FRAMES: u32 = 64;
pub const MAX_BUFFER_FRAMES: u32 = 4096;

/// The clock-advance / zero-timestamp period, in frames. This is a fixed
/// geometric constant of the ring buffer, computed once at compile time from
/// `BLOCK * RING_BLOCKS`. It is intentionally NOT a function of any runtime
/// buffer-size negotiation: `ZeroTimeStampPeriod` must return this exact
/// value, always, or the HAL's scheduling model desyncs from the driver the
/// moment a client changes its IO buffer size. See
/// `engine::properties::zero_timestamp_period`.
pub const RING_FRAMES: usize = BLOCK * RING_BLOCKS;

#[derive(Clone, Copy)]
pub struct DeviceConfig {
    pub channels: u32,
    pub device_id: u32,
    pub out_stream_id: u32,
    pub in_stream_id: u32,
    pub name: &'static str,
    pub uid: &'static str,
}

pub static DEVICES: [DeviceConfig; 5] = [
    DeviceConfig {
        channels: 2,
        device_id: 10,
        out_stream_id: 11,
        in_stream_id: 12,
        name: "Senda Link 2ch",
        uid: "SendaLink_2ch",
    },
    DeviceConfig {
        channels: 8,
        device_id: 20,
        out_stream_id: 21,
        in_stream_id: 22,
        name: "Senda Link 8ch",
        uid: "SendaLink_8ch",
    },
    DeviceConfig {
        channels: 16,
        device_id: 30,
        out_stream_id: 31,
        in_stream_id: 32,
        name: "Senda Link 16ch",
        uid: "SendaLink_16ch",
    },
    DeviceConfig {
        channels: 32,
        device_id: 40,
        out_stream_id: 41,
        in_stream_id: 42,
        name: "Senda Link 32ch",
        uid: "SendaLink_32ch",
    },
    DeviceConfig {
        channels: 128,
        device_id: 50,
        out_stream_id: 51,
        in_stream_id: 52,
        name: "Senda Link 128ch",
        uid: "SendaLink_128ch",
    },
];

/// Looks up a device's index in `DEVICES`/`STATE` by its `AudioObjectID`.
pub fn find_by_object_id(id: u32) -> Option<usize> {
    DEVICES.iter().position(|d| d.device_id == id)
}

/// Looks up a stream's owning device index and direction (`true` = input)
/// by its `AudioObjectID`.
pub fn find_by_stream_id(id: u32) -> Option<(usize, bool)> {
    DEVICES.iter().enumerate().find_map(|(i, d)| {
        if d.out_stream_id == id {
            Some((i, false))
        } else if d.in_stream_id == id {
            Some((i, true))
        } else {
            None
        }
    })
}

pub struct PerDevice {
    pub sample_rate: AtomicU64, // f64 bits; 0 means "use DEFAULT_SAMPLE_RATE"
    pub io_running: AtomicBool,
    pub client_count: AtomicU32,
    /// Negotiated IO buffer size in frames, clamped to
    /// `[MIN_BUFFER_FRAMES, MAX_BUFFER_FRAMES]`. Defaults to `BLOCK`.
    /// Reported by `'fsiz'` and settable via `SetPropertyData` — it never
    /// feeds back into `RING_FRAMES`/`ZeroTimeStampPeriod`. See the
    /// non-negotiable note on `BLOCK` above.
    pub buffer_frames: AtomicU32,
    /// Counts every `DoIOOperation` cycle in which the HAL-supplied frame
    /// count exceeded `MAX_BUFFER_FRAMES` and was clamped down before
    /// touching any audio (see `ffi::plugin::senda_DoIOOperation`). The
    /// clamp is against that hard ceiling, not this device's negotiated
    /// `buffer_frames`, so it only fires for a genuine anomaly. Clamping
    /// itself is correct; this turns an otherwise-silent truncation into a
    /// number a diagnostic can see, the same motivation as `Ring`'s own
    /// `torn`/`stale`/`retries` counters.
    frame_clamps: AtomicU64,
}

impl PerDevice {
    pub const fn new() -> Self {
        Self {
            sample_rate: AtomicU64::new(0),
            io_running: AtomicBool::new(false),
            client_count: AtomicU32::new(0),
            buffer_frames: AtomicU32::new(BLOCK as u32),
            frame_clamps: AtomicU64::new(0),
        }
    }

    pub fn rate(&self) -> f64 {
        let bits = self.sample_rate.load(Ordering::Acquire);
        if bits == 0 {
            DEFAULT_SAMPLE_RATE
        } else {
            f64::from_bits(bits)
        }
    }

    pub fn set_rate(&self, r: f64) {
        self.sample_rate.store(r.to_bits(), Ordering::Release);
    }

    pub fn buffer_frames(&self) -> u32 {
        self.buffer_frames.load(Ordering::Acquire)
    }

    /// Clamps to `[MIN_BUFFER_FRAMES, MAX_BUFFER_FRAMES]` and stores.
    /// Never rejects a request outright — an out-of-range value is clamped
    /// to the nearest bound rather than erroring, matching how the
    /// advertised `'fsz#'` range is meant to be read (a hard limit, not a
    /// discrete list like `SAMPLE_RATES`).
    pub fn set_buffer_frames(&self, frames: u32) {
        let clamped = frames.clamp(MIN_BUFFER_FRAMES, MAX_BUFFER_FRAMES);
        self.buffer_frames.store(clamped, Ordering::Release);
    }

    /// Records one `DoIOOperation` cycle whose HAL-supplied frame count
    /// exceeded `MAX_BUFFER_FRAMES` and had to be clamped. `Relaxed` — this
    /// is a diagnostic counter, not a synchronisation point (same
    /// reasoning as `Ring`'s `torn`/`stale`/`retries`).
    pub fn record_frame_clamp(&self) {
        self.frame_clamps.fetch_add(1, Ordering::Relaxed);
    }

    /// The clamp count so far. Not yet exposed as a HAL property.
    pub fn frame_clamps(&self) -> u64 {
        self.frame_clamps.load(Ordering::Relaxed)
    }
}

impl Default for PerDevice {
    fn default() -> Self {
        Self::new()
    }
}

pub static STATE: [PerDevice; 5] = [
    PerDevice::new(),
    PerDevice::new(),
    PerDevice::new(),
    PerDevice::new(),
    PerDevice::new(),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_devices_with_expected_channel_counts() {
        assert_eq!(DEVICES.len(), 5);
        let chans: Vec<u32> = DEVICES.iter().map(|d| d.channels).collect();
        assert_eq!(chans, vec![2, 8, 16, 32, 128]);
    }

    #[test]
    fn object_ids_are_unique_and_resolvable() {
        for (i, d) in DEVICES.iter().enumerate() {
            assert_eq!(find_by_object_id(d.device_id), Some(i));
            assert_ne!(d.device_id, d.in_stream_id);
            assert_ne!(d.device_id, d.out_stream_id);
        }
        assert_eq!(find_by_object_id(9999), None);
    }

    #[test]
    fn stream_ids_resolve_to_owning_device_and_direction() {
        for (i, d) in DEVICES.iter().enumerate() {
            assert_eq!(find_by_stream_id(d.out_stream_id), Some((i, false)));
            assert_eq!(find_by_stream_id(d.in_stream_id), Some((i, true)));
        }
        assert_eq!(find_by_stream_id(9999), None);
    }

    #[test]
    fn ring_frames_is_32768_and_matches_block_times_ring_blocks() {
        assert_eq!(RING_FRAMES, 32768);
        assert_eq!(RING_FRAMES, BLOCK * RING_BLOCKS);
    }

    #[test]
    fn per_device_rate_defaults_and_round_trips() {
        let st = PerDevice::new();
        assert_eq!(st.rate(), DEFAULT_SAMPLE_RATE);
        st.set_rate(96000.0);
        assert_eq!(st.rate(), 96000.0);
    }

    #[test]
    fn per_device_buffer_frames_defaults_to_block_and_clamps() {
        let st = PerDevice::new();
        assert_eq!(st.buffer_frames(), BLOCK as u32);
        st.set_buffer_frames(1024);
        assert_eq!(st.buffer_frames(), 1024);
        st.set_buffer_frames(1); // below MIN_BUFFER_FRAMES
        assert_eq!(st.buffer_frames(), MIN_BUFFER_FRAMES);
        st.set_buffer_frames(u32::MAX); // above MAX_BUFFER_FRAMES
        assert_eq!(st.buffer_frames(), MAX_BUFFER_FRAMES);
    }

    #[test]
    fn setting_buffer_frames_never_touches_ring_frames() {
        // The non-negotiable invariant, exercised end to end: mutating a
        // PerDevice's buffer size must not be able to move RING_FRAMES
        // (a `const`, so this is really just documentation-as-a-test —
        // there is structurally no path from one to the other).
        let st = PerDevice::new();
        st.set_buffer_frames(4096);
        assert_eq!(RING_FRAMES, 32768);
    }
}
