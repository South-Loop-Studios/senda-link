//! Hand-written declarations mirroring <CoreAudio/AudioServerPlugIn.h>.
//! Verified against Apple's header by tests/abi_probe.c — see tests/abi.rs.

use std::ffi::{c_char, c_void};

pub type OSStatus = i32;
pub type AudioObjectID = u32;
pub type AudioObjectPropertySelector = u32;
pub type AudioObjectPropertyScope = u32;
pub type AudioObjectPropertyElement = u32;
pub type CFStringRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
/// A CFBundle reference. Only ever obtained via
/// `CFBundleGetBundleWithIdentifier` in this driver — a Get-rule API, so
/// every `CFBundleRef` this driver touches is borrowed, never owned/released.
pub type CFBundleRef = *const c_void;
/// A CFURL reference, e.g. the `kAudioDevicePropertyIcon` payload. Same
/// pointer-sized representation as `CFStringRef`; kept as a distinct alias
/// so call sites read as "this is a URL", not "this happens to also be a
/// void pointer".
pub type CFURLRef = *const c_void;
pub type Pid = i32;

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

/// `kCFStringEncodingUTF8`, the only encoding this driver ever hands to
/// `CFStringCreateWithCString`.
pub const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

// CoreFoundation entry points needed to turn a Rust `&str` into the
// `CFStringRef` the HAL expects for string-typed properties (Name,
// Manufacturer, DeviceUID, ModelUID, ResourceBundle) and to read the
// `CFStringRef` qualifier `TranslateUIDToDevice` receives back out again.
//
// Passing `NULL` for the allocator argument is documented by Apple as
// equivalent to `kCFAllocatorDefault`, so no external `kCFAllocatorDefault`
// static needs to be linked.
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;

    /// Best-effort fast path for reading a `CFStringRef`'s bytes without a
    /// copy; returns null if the string isn't stored in a compatible
    /// internal representation, in which case the caller must fall back to
    /// `CFStringGetCString`.

    /// Copies up to `buffer_size` bytes (including the trailing NUL) of
    /// `the_string` into `buffer` using `encoding`. Returns `0` (false) on
    /// failure, nonzero on success.
    pub fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;

    /// Looks up an already-registered bundle by its `CFBundleIdentifier`.
    /// This is a Get-rule API: the returned `CFBundleRef` is borrowed from
    /// CoreFoundation's bundle registry, NOT owned by the caller — releasing
    /// it corrupts that registry entry for every other caller. Returns NULL
    /// if no bundle with that identifier is registered (e.g. this driver's
    /// `.driver` bundle has not been loaded by `coreaudiod`).
    pub fn CFBundleGetBundleWithIdentifier(bundle_id: CFStringRef) -> CFBundleRef;

    /// Locates a resource file (by name and extension, optionally within a
    /// subdirectory of `Contents/Resources`) inside `bundle`. This is a
    /// Copy-rule API: the caller owns the returned `CFURLRef` and must
    /// either release it or transfer that ownership onward — this driver
    /// does the latter, handing it straight to the HAL as a property value.
    /// Returns NULL if `bundle` is NULL or the resource does not exist.
    pub fn CFBundleCopyResourceURL(
        bundle: CFBundleRef,
        resource_name: CFStringRef,
        resource_type: CFStringRef,
        sub_dir_name: CFStringRef,
    ) -> CFURLRef;

    /// Releases a Create- or Copy-rule CoreFoundation reference this driver
    /// owns (never a Get-rule reference like the `CFBundleRef` above).
    /// `CFRelease(NULL)` is documented by Apple to crash, so every call site
    /// must null-check first — see `ffi::plugin`'s `icon_resource_url`.
    pub fn CFRelease(cf: *const c_void);
}

/// Mirrors `<mach/mach_time.h>`'s `mach_timebase_info_data_t`: the
/// numerator/denominator pair that converts `mach_absolute_time()` ticks to
/// nanoseconds (`ns = ticks * numer / denom`). On Apple Silicon this is
/// *not* 1:1 (ticks run at the 24MHz timebase, numer/denom ~= 125/3); on
/// Intel it typically is 1:1. Must always be queried at runtime, never
/// assumed.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct MachTimebaseInfo {
    pub numer: u32,
    pub denom: u32,
}

// `mach_absolute_time`/`mach_timebase_info` are part of libSystem, linked
// into every macOS binary by default — no `#[link(name = ...)]` needed,
// unlike the CoreFoundation entry points above.
extern "C" {
    /// Monotonic tick counter. The only clock source this driver reads —
    /// see `engine::clock`, which turns raw ticks into a monotonic
    /// zero-timestamp timeline without itself touching this function.
    pub fn mach_absolute_time() -> u64;

    /// Fills in the tick-to-nanosecond conversion ratio for this machine.
    /// Returns a `kern_return_t` (`0` == `KERN_SUCCESS`).
    pub fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AudioObjectPropertyAddress {
    pub selector: AudioObjectPropertySelector,
    pub scope: AudioObjectPropertyScope,
    pub element: AudioObjectPropertyElement,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct SmpteTime {
    pub subframes: i16,
    pub subframe_divisor: i16,
    pub counter: u32,
    pub ty: u32,
    pub flags: u32,
    pub hours: i16,
    pub minutes: i16,
    pub seconds: i16,
    pub frames: i16,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct AudioTimeStamp {
    pub sample_time: f64,
    pub host_time: u64,
    pub rate_scalar: f64,
    pub word_clock_time: u64,
    pub smpte: SmpteTime,
    pub flags: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct AudioStreamBasicDescription {
    pub sample_rate: f64,
    pub format_id: u32,
    pub format_flags: u32,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
    pub bytes_per_frame: u32,
    pub channels_per_frame: u32,
    pub bits_per_channel: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct AudioValueRange {
    pub minimum: f64,
    pub maximum: f64,
}

/// Mirrors CoreAudio's `AudioStreamRangedDescription` — NOT the same type as
/// `AudioStreamBasicDescription`. `kAudioStreamPropertyAvailableVirtualFormats`
/// ('sfma') and `kAudioStreamPropertyAvailablePhysicalFormats` ('pfta') are
/// arrays of *this* 56-byte struct (a format plus the sample-rate range it's
/// valid over), not arrays of the 40-byte bare `AudioStreamBasicDescription`
/// that `'sfmt'`/`'pft '` return. Confusing the two silently corrupts every
/// element's offset for a host walking the array. See tests/abi.rs.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct AudioStreamRangedDescription {
    pub format: AudioStreamBasicDescription,
    pub sample_rate_range: AudioValueRange,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct AudioServerPlugInClientInfo {
    pub client_id: u32,
    pub process_id: Pid,
    pub is_native_endian: u8,
    pub bundle_id: CFStringRef,
}

/// Mirrors CoreAudio's `AudioServerPlugInIOCycleInfo`.
///
/// Apple's header names the buffer-size field `mNominalIOBufferFrameSize`
/// (not `mIOBufferFrameSize`), declares `mInputTime` before `mOutputTime`, and
/// has two trailing `Float64` fields — `mMainHostTicksPerFrame` (a union with
/// the deprecated `mMasterHostTicksPerFrame` alias, so a single `f64` field
/// reproduces the layout) and `mDeviceHostTicksPerFrame`. Every field offset
/// is checked against the header in tests/abi.rs.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct AudioServerPlugInIOCycleInfo {
    pub io_cycle_counter: u64,
    pub nominal_io_buffer_frame_size: u32,
    pub current_time: AudioTimeStamp,
    pub input_time: AudioTimeStamp,
    pub output_time: AudioTimeStamp,
    pub main_host_ticks_per_frame: f64,
    pub device_host_ticks_per_frame: f64,
}

pub type AudioServerPlugInDriverRef = *mut *mut AudioServerPlugInDriverInterface;

#[repr(C)]
pub struct AudioServerPlugInDriverInterface {
    pub _reserved: *mut c_void,
    pub QueryInterface: unsafe extern "C" fn(*mut c_void, [u8; 16], *mut *mut c_void) -> i32,
    pub AddRef: unsafe extern "C" fn(*mut c_void) -> u32,
    pub Release: unsafe extern "C" fn(*mut c_void) -> u32,
    pub Initialize: unsafe extern "C" fn(AudioServerPlugInDriverRef, *const c_void) -> OSStatus,
    pub CreateDevice: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        CFDictionaryRef,
        *const AudioServerPlugInClientInfo,
        *mut AudioObjectID,
    ) -> OSStatus,
    pub DestroyDevice: unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID) -> OSStatus,
    pub AddDeviceClient: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        *const AudioServerPlugInClientInfo,
    ) -> OSStatus,
    pub RemoveDeviceClient: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        *const AudioServerPlugInClientInfo,
    ) -> OSStatus,
    pub PerformDeviceConfigurationChange: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        u64,
        *mut c_void,
    ) -> OSStatus,
    pub AbortDeviceConfigurationChange: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        u64,
        *mut c_void,
    ) -> OSStatus,
    pub HasProperty: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        Pid,
        *const AudioObjectPropertyAddress,
    ) -> u8,
    pub IsPropertySettable: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        Pid,
        *const AudioObjectPropertyAddress,
        *mut u8,
    ) -> OSStatus,
    pub GetPropertyDataSize: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        Pid,
        *const AudioObjectPropertyAddress,
        u32,
        *const c_void,
        *mut u32,
    ) -> OSStatus,
    pub GetPropertyData: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        Pid,
        *const AudioObjectPropertyAddress,
        u32,
        *const c_void,
        u32,
        *mut u32,
        *mut c_void,
    ) -> OSStatus,
    pub SetPropertyData: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        Pid,
        *const AudioObjectPropertyAddress,
        u32,
        *const c_void,
        u32,
        *const c_void,
    ) -> OSStatus,
    pub StartIO: unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, u32) -> OSStatus,
    pub StopIO: unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, u32) -> OSStatus,
    pub GetZeroTimeStamp: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        u32,
        *mut f64,
        *mut u64,
        *mut u64,
    ) -> OSStatus,
    pub WillDoIOOperation: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        u32,
        u32,
        *mut u8,
        *mut u8,
    ) -> OSStatus,
    pub BeginIOOperation: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        u32,
        u32,
        u32,
        *const AudioServerPlugInIOCycleInfo,
    ) -> OSStatus,
    pub DoIOOperation: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        AudioObjectID,
        u32,
        u32,
        u32,
        *const AudioServerPlugInIOCycleInfo,
        *mut c_void,
        *mut c_void,
    ) -> OSStatus,
    pub EndIOOperation: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        u32,
        u32,
        u32,
        *const AudioServerPlugInIOCycleInfo,
    ) -> OSStatus,
}
