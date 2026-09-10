//! Static device table and per-device mutable state: five devices
//! (2/8/16/32/128 channels), a compile-time table plus fixed-size arrays of
//! atomics. No `Vec`, no `Mutex`, no allocation.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// `kAudioObjectPlugInObject`, the root object every device is owned by.
pub const PLUGIN_OBJECT_ID: u32 = 1;

pub const SAMPLE_RATES: [f64; 6] = [44100.0, 48000.0, 88200.0, 96000.0, 176400.0, 192000.0];
pub const DEFAULT_SAMPLE_RATE: f64 = 48000.0;

/// IO block size in frames. Never make this variable: `RING_FRAMES = BLOCK *
/// RING_BLOCKS` is the `ZeroTimeStampPeriod`, and deriving that from a client's
/// buffer size is the defect this project exists to fix.
pub const BLOCK: usize = 512;
pub const RING_BLOCKS: usize = 64;

/// Buffer sizes accepted via `'fsiz'`; a per-`PerDevice` value, never ring geometry.
pub const MIN_BUFFER_FRAMES: u32 = 64;
pub const MAX_BUFFER_FRAMES: u32 = 4096;

/// Clock-advance / `ZeroTimeStampPeriod` in frames. A fixed geometric constant:
/// the HAL's scheduling desyncs if it moves with a client's buffer size.
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

/// A stream's owning device index and direction (`true` = input).
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
    /// Negotiated IO buffer size, clamped to `[MIN, MAX]_BUFFER_FRAMES`; never feeds `RING_FRAMES`.
    pub buffer_frames: AtomicU32,
    /// `DoIOOperation` cycles whose frame count exceeded `MAX_BUFFER_FRAMES` and was clamped.
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

    /// Clamps rather than rejects: `'fsz#'` is a hard limit, not a discrete list.
    pub fn set_buffer_frames(&self, frames: u32) {
        let clamped = frames.clamp(MIN_BUFFER_FRAMES, MAX_BUFFER_FRAMES);
        self.buffer_frames.store(clamped, Ordering::Release);
    }

    /// `Relaxed`: a diagnostic counter, not a synchronisation point.
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
        let st = PerDevice::new();
        st.set_buffer_frames(4096);
        assert_eq!(RING_FRAMES, 32768);
    }
}
