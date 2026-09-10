//! Hand-written mirror of `<CoreAudio/AudioServerPlugIn.h>` plus the few
//! CoreFoundation and Mach entry points the driver needs. Every size, offset
//! and four-character code is verified against Apple's headers by
//! `tests/abi_probe.c`, driven from `tests/abi.rs`.

use std::ffi::{c_char, c_void};

pub type OSStatus = i32;
pub type AudioObjectID = u32;
pub type AudioObjectPropertySelector = u32;
pub type AudioObjectPropertyScope = u32;
pub type AudioObjectPropertyElement = u32;
pub type CFStringRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFBundleRef = *const c_void;
pub type CFURLRef = *const c_void;
pub type Pid = i32;

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

pub const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

// Ownership follows CoreFoundation's naming: a Get-rule result
// (`CFBundleGetBundleWithIdentifier`) is borrowed and must never be released;
// a Copy- or Create-rule result is owned and must be released or handed on.
// `CFRelease(NULL)` crashes, so null-check first. A NULL allocator is the default.
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;

    pub fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;

    pub fn CFBundleGetBundleWithIdentifier(bundle_id: CFStringRef) -> CFBundleRef;

    pub fn CFBundleCopyResourceURL(
        bundle: CFBundleRef,
        resource_name: CFStringRef,
        resource_type: CFStringRef,
        sub_dir_name: CFStringRef,
    ) -> CFURLRef;

    pub fn CFRelease(cf: *const c_void);
}

/// `mach_timebase_info_data_t`: `ns = ticks * numer / denom`. Not 1:1 on
/// Apple Silicon (about 125/3), so always queried at runtime, never assumed.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct MachTimebaseInfo {
    pub numer: u32,
    pub denom: u32,
}

// libSystem, linked into every macOS binary; no `#[link]` needed.
extern "C" {
    pub fn mach_absolute_time() -> u64;

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

/// `'sfma'`/`'pfta'` return arrays of this 56-byte struct, not of the 40-byte
/// `AudioStreamBasicDescription` that `'sfmt'`/`'pft '` return.
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

/// The header's `mMainHostTicksPerFrame` is a union with the deprecated
/// `mMasterHostTicksPerFrame` alias; a single `f64` reproduces the layout.
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
