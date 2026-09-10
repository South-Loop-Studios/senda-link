//! The 22-entry `AudioServerPlugInDriverInterface` vtable.
//!
//! The interface is COM-style: `QueryInterface` echoes the driver handle
//! back and `AddRef`/`Release` are constant, since the driver lives for the
//! whole `coreaudiod` process. Devices are published statically, so
//! `CreateDevice`/`DestroyDevice` answer 'unop' and the remaining lifecycle
//! entries are no-ops. The property entries marshal raw HAL pointers into
//! safe values for `engine::properties` and serialise its `Value`s back out.
//! `StartIO`/`StopIO` ref-count clients per device; the first client's
//! `StartIO` (re)anchors the clock and wipes the ring. `GetZeroTimeStamp`
//! is `Clock::zero_timestamp`. `DoIOOperation` clamps the frame count to
//! `MAX_BUFFER_FRAMES` and hands an `f32` slice to `engine::io`, indexed by
//! the HAL's own sample time for the cycle.
//!
//! `engine/` is `#![forbid(unsafe_code)]`; apart from the `SendaLinkCreate`
//! factory in `lib.rs`, every `unsafe` in the crate lives under `src/ffi/`.
//! Nothing on the IO path (`GetZeroTimeStamp`, `DoIOOperation`) allocates,
//! locks or can panic: a panic unwinding across `extern "C"` would abort
//! `coreaudiod` and take all system audio down with it.

use super::types::*;
use crate::engine::clock::CLOCKS;
use crate::engine::device;
use crate::engine::io;
use crate::engine::properties::{self, Value};
use std::ffi::{c_char, c_void};
use std::sync::atomic::Ordering;

pub const OK: OSStatus = 0;
/// `kAudioHardwareUnknownPropertyError` ('who?'): the selector isn't one
/// this object kind answers. Computed via `fourcc` rather than hand-copied
/// as a decimal literal, so a transcription error is impossible; equals
/// 2003332927.
pub const ERR_UNKNOWN_PROPERTY: OSStatus = fourcc(b"who?") as OSStatus;
/// `kAudioHardwareUnsupportedOperationError` ('unop'): a real property of
/// this object, but this driver doesn't allow the requested operation on
/// it. Same `fourcc`-not-decimal treatment as `ERR_UNKNOWN_PROPERTY` above.
/// Equals 1970171760.
pub const ERR_UNSUPPORTED: OSStatus = fourcc(b"unop") as OSStatus;
/// `kAudioHardwareBadObjectError` ('!obj'): the `AudioObjectID` doesn't name
/// an object this driver owns. Same `fourcc`-not-decimal treatment as
/// `ERR_UNKNOWN_PROPERTY` above. Equals 560947818.
pub const ERR_BAD_OBJECT: OSStatus = fourcc(b"!obj") as OSStatus;
/// `kAudioHardwareBadPropertySizeError` ('!siz'): the host's buffer is
/// smaller than the property actually needs. Returned instead of silently
/// truncating — see `write_value`. Computed via `fourcc` rather than
/// hand-copied as a decimal literal; equals 561211770.
pub const ERR_BAD_PROPERTY_SIZE: OSStatus = fourcc(b"!siz") as OSStatus;
/// `kAudioHardwareUnspecifiedError` ('what'): CoreFoundation failed to
/// create a string (e.g. allocation failure). Vanishingly unlikely, but
/// must not be treated as success — see `write_value`'s `Value::Str` arm.
pub const ERR_UNSPECIFIED: OSStatus = fourcc(b"what") as OSStatus;

/// `kAudioServerPlugInIOOperationWriteMix` ('rite'): the HAL handing this
/// driver the final, already-mixed output buffer for one IO cycle to
/// consume.
///
/// The value matters more than most: with anything other than Apple's
/// `'rite'`, `senda_WillDoIOOperation` would never match a real
/// `inOperationID`, would answer `*will = 0`, and the HAL would never call
/// `senda_DoIOOperation` at all — the device would enumerate, start and
/// keep time, but pass no audio. `tests/abi.rs`'s
/// `fourcc_matches_known_constants` asserts this against the real SDK
/// header (via `tests/abi_probe.c`) so a typo fails `cargo test`.
const IO_OP_WRITE_MIX: u32 = fourcc(b"rite");
/// `kAudioServerPlugInIOOperationReadInput` ('read'): the HAL asking this
/// driver to fill in this device's input buffer for one IO cycle. Pinned
/// against the SDK header by the same test as `IO_OP_WRITE_MIX`.
const IO_OP_READ_INPUT: u32 = fourcc(b"read");

// REFIID (CFUUIDBytes) is a 16-byte all-`UInt8` by-value aggregate; it lowers
// to the same by-value ABI on both x86_64 and arm64, so the `[u8; 16]`
// parameter here is safe despite `improper_ctypes_definitions` flagging
// fixed-size arrays passed by value as non-FFI-safe in general.
#[allow(improper_ctypes_definitions)]
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_QueryInterface(
    s: *mut c_void,
    _uuid: [u8; 16],
    out: *mut *mut c_void,
) -> i32 {
    // Apple's contract for a successful QueryInterface is to echo the same
    // handle back: `*outInterface = inDriver`. `s` here is the
    // `AudioServerPlugInDriverRef` (an `Interface**`) the host already holds;
    // it must not be replaced with `driver_interface_ptr()` (an `Interface*`),
    // which is one indirection level short and would leave the host
    // dereferencing `_reserved` (null) as if it were the vtable pointer.
    //
    // SAFETY: guarded by the null check that follows; per the HAL contract
    // `out` is then a valid, writable, pointer-aligned `*mut c_void` for the
    // duration of this call.
    if !out.is_null() {
        unsafe { *out = s };
    }
    0
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_AddRef(_s: *mut c_void) -> u32 {
    1
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_Release(_s: *mut c_void) -> u32 {
    1
}

/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_Initialize(
    _d: AudioServerPlugInDriverRef,
    _host: *const c_void,
) -> OSStatus {
    OK
}

/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_CreateDevice(
    _d: AudioServerPlugInDriverRef,
    _desc: CFDictionaryRef,
    _c: *const AudioServerPlugInClientInfo,
    _out: *mut AudioObjectID,
) -> OSStatus {
    ERR_UNSUPPORTED
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_DestroyDevice(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
) -> OSStatus {
    ERR_UNSUPPORTED
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_AddDeviceClient(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _c: *const AudioServerPlugInClientInfo,
) -> OSStatus {
    OK
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_RemoveDeviceClient(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _c: *const AudioServerPlugInClientInfo,
) -> OSStatus {
    OK
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_PerformDeviceConfigurationChange(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _a: u64,
    _i: *mut c_void,
) -> OSStatus {
    OK
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_AbortDeviceConfigurationChange(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _a: u64,
    _i: *mut c_void,
) -> OSStatus {
    OK
}

// ---- Property dispatch helpers -------------------------------------------
//
// Everything from here to the property vtable functions below is the only
// place in this crate that turns raw HAL pointers into safe Rust values (or
// back again).
// `engine::properties` never sees a pointer — it takes `&AudioObjectPropertyAddress`
// by value/reference and `Option<&str>`/`&[u8]` slices, and hands back a
// logical `Value`; this module is responsible for every unsafe read/write
// and for all CoreFoundation interop. Nothing here may panic: a panic
// unwinding across `extern "C"` aborts `coreaudiod` and takes all system
// audio down with it.

/// Copies a `*const AudioObjectPropertyAddress` into an owned value.
///
/// # Safety contract
/// Null-checks first. If non-null, trusts the host's contract that the
/// pointer refers to a fully-initialised, correctly-sized
/// `AudioObjectPropertyAddress` for the duration of this call — the
/// standard assumption at every AudioServerPlugIn entry point.
fn read_property_address(
    a: *const AudioObjectPropertyAddress,
) -> Option<AudioObjectPropertyAddress> {
    if a.is_null() {
        return None;
    }
    // SAFETY: non-null per the check above; the type is `Copy` and repr(C),
    // so this is a plain read of host-owned memory the host guarantees is
    // valid and initialised for the call. The read relies on 4-byte
    // alignment (three `u32` fields); the HAL allocates the address as its
    // C type, so that holds.
    Some(unsafe { *a })
}

/// Decodes the `CFStringRef` qualifier `TranslateUIDToDevice` receives (the
/// only property in this driver that uses a qualifier) into a `&str`
/// borrowed from `scratch`. Returns `None` for every other property, or if
/// decoding fails for any reason — never panics.
fn read_qualifier_str(
    qualifier_size: u32,
    qualifier_data: *const c_void,
    scratch: &mut [u8; 256],
) -> Option<&str> {
    if qualifier_data.is_null() || qualifier_size as usize != std::mem::size_of::<CFStringRef>() {
        return None;
    }
    // SAFETY: size-checked above to be exactly one CFStringRef-sized value,
    // non-null; we only read it. The read relies on 8-byte (pointer)
    // alignment; the HAL allocates the qualifier as a `CFStringRef`, so
    // that holds.
    let cf: CFStringRef = unsafe { *(qualifier_data as *const CFStringRef) };
    if cf.is_null() {
        return None;
    }
    // SAFETY: `cf` is a non-null CFStringRef the host owns for the duration
    // of this call; `scratch` is a valid, appropriately-sized, writable
    // local buffer.
    let ok = unsafe {
        CFStringGetCString(
            cf,
            scratch.as_mut_ptr() as *mut c_char,
            scratch.len() as isize,
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    let nul = scratch
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(scratch.len());
    std::str::from_utf8(scratch.get(..nul)?).ok()
}

/// Reads the current machine's tick-to-nanosecond conversion ratio via
/// `mach_timebase_info` — the number of nanoseconds one `mach_absolute_time`
/// tick represents (not ticks per nanosecond, despite how that reads;
/// `numer`/`denom` name it the other way round). Not RT-hot: called only
/// from `senda_StartIO` and on a sample-rate change in
/// `senda_SetPropertyData`, never from `senda_GetZeroTimeStamp` itself —
/// that call only needs a raw tick reading, see
/// `engine::clock::Clock::zero_timestamp`.
fn host_ns_per_tick() -> f64 {
    let mut info = MachTimebaseInfo::default();
    // SAFETY: `&mut info` is a valid, uniquely-owned, correctly-sized
    // `MachTimebaseInfo` for the duration of this call.
    let kr = unsafe { mach_timebase_info(&mut info) };
    // `kr != 0` is a `kern_return_t` failure; `denom == 0` would divide by
    // zero in `Clock::start`. Either way, 1 tick == 1 ns is a safe fallback
    // rather than propagating a bogus ratio into the clock.
    if kr != 0 || info.denom == 0 {
        return 1.0;
    }
    f64::from(info.numer) / f64::from(info.denom)
}

/// Creates a fresh `CFStringRef` from `s`. The HAL takes ownership of the
/// returned string, so a new one is created per call rather than reusing a
/// cached reference this module would also need to release.
fn cfstring_from_str(s: &str) -> CFStringRef {
    let cstr = std::ffi::CString::new(s).unwrap_or_default();
    // SAFETY: `cstr` is a valid, NUL-terminated C string for the duration
    // of this call; passing `NULL` as the allocator is documented by Apple
    // to mean "use the default allocator".
    unsafe { CFStringCreateWithCString(std::ptr::null(), cstr.as_ptr(), K_CF_STRING_ENCODING_UTF8) }
}

/// Must match `CFBundleIdentifier` in `Info.plist` exactly — this is how the
/// driver finds its own bundle at runtime to resolve `DeviceIcon.icns`.
/// Duplicated here rather than read from the plist at build time (same
/// trade-off already made for other cross-file constants in this project):
/// no build-time file I/O, at the cost of the two needing to be kept in
/// sync by hand.
const DRIVER_BUNDLE_ID: &str = "com.senda.link.driver";

/// Resolves a `CFURLRef` to `Contents/Resources/DeviceIcon.icns` inside this
/// driver's own bundle, for `kAudioDevicePropertyIcon`.
///
/// Returns `None` if the bundle isn't registered under `DRIVER_BUNDLE_ID`
/// (e.g. running outside `coreaudiod`, as in `cargo test`) or the resource
/// is missing — callers must map `None` to `ERR_UNSPECIFIED` rather than
/// handing a null `CFURLRef` to the host, the same hazard `write_value`'s
/// `Value::Str` arm guards against for `CFRelease(NULL)`.
///
/// CoreFoundation ownership, per call:
/// - `bundle_id`/`name`/`ext` are Create-rule `CFStringRef`s this function
///   makes and owns — each is released here once it is no longer needed,
///   after a null check (`CFRelease(NULL)` crashes).
/// - `CFBundleGetBundleWithIdentifier` is a Get-rule API: `bundle` is
///   borrowed from CoreFoundation's registry and must never be released —
///   doing so corrupts that registry entry for every other caller in the
///   process.
/// - `CFBundleCopyResourceURL` is a Copy-rule API: this function owns the
///   returned `CFURLRef` and transfers that ownership to the caller (who, in
///   turn, hands it to the HAL) by returning it un-released.
fn icon_resource_url() -> Option<CFURLRef> {
    let bundle_id = cfstring_from_str(DRIVER_BUNDLE_ID);
    if bundle_id.is_null() {
        return None;
    }
    // SAFETY: `bundle_id` is a valid, non-null CFStringRef this function
    // just created and owns for the duration of this call.
    let bundle: CFBundleRef = unsafe { CFBundleGetBundleWithIdentifier(bundle_id) };
    // SAFETY: `bundle_id` is non-null (checked above); this function owns it
    // (Create rule) and is done with it — `CFBundleGetBundleWithIdentifier`
    // only reads its argument, it does not take ownership.
    unsafe { CFRelease(bundle_id) };
    // `bundle` is a Get-rule reference (see doc above) — never released,
    // whether or not it turns out to be null.
    if bundle.is_null() {
        return None;
    }

    let name = cfstring_from_str("DeviceIcon");
    let ext = cfstring_from_str("icns");
    let url = if name.is_null() || ext.is_null() {
        None
    } else {
        // SAFETY: `bundle` is a valid, non-null, Get-rule `CFBundleRef` this
        // function does not own (see above); `name`/`ext` are valid,
        // non-null CFStringRefs this function owns until released below.
        let url = unsafe { CFBundleCopyResourceURL(bundle, name, ext, std::ptr::null()) };
        if url.is_null() {
            None
        } else {
            Some(url)
        }
    };
    // Release whichever of `name`/`ext` were actually created, regardless of
    // whether the lookup succeeded — these are local Create-rule references,
    // never handed to anything else.
    if !name.is_null() {
        // SAFETY: non-null, Create-rule, owned by this function.
        unsafe { CFRelease(name) };
    }
    if !ext.is_null() {
        // SAFETY: non-null, Create-rule, owned by this function.
        unsafe { CFRelease(ext) };
    }
    url
}

/// Copies `n` bytes from `*src` into `out`. `n` is always clamped by the
/// caller to at most `size_of::<T>()`, so the read never runs past `src`.
///
/// # Safety
/// Caller must ensure `out` is non-null and has at least `n` writable
/// bytes.
unsafe fn copy_out<T>(src: &T, out: *mut c_void, n: usize) {
    let n = n.min(std::mem::size_of::<T>());
    std::ptr::copy_nonoverlapping(src as *const T as *const u8, out.cast::<u8>(), n);
}

/// Serialises `value` into the host's output buffer and returns the number
/// of bytes actually written.
///
/// Rejects rather than truncates: if `out_size` is smaller than
/// `value.size_in_bytes()` for a non-zero-sized value, this returns
/// `Err(ERR_BAD_PROPERTY_SIZE)` and writes nothing at all. Truncating
/// instead would be actively dangerous for `Value::Str`/`Value::Url` —
/// writing a partial `CFStringRef`/`CFURLRef` pointer into the host's buffer
/// would leave `coreaudiod` holding (and eventually `CFRelease`-ing)
/// garbage, and for every other variant it would silently hand back corrupt
/// data with no way for the caller to notice. Crucially, this size check
/// runs before the `Value::Str`/`Value::Url` arms create anything: an
/// undersized buffer is rejected without ever calling
/// `CFStringCreateWithCString`/`icon_resource_url`, so a truncation bug can
/// never leak a CoreFoundation object nobody will release. A zero-sized
/// value (`U32ArrEmpty`) is always fine to "write", regardless of `out_size`
/// or whether `out` is null, since there is nothing to copy.
fn write_value(value: Value, out_size: u32, out: *mut c_void) -> Result<u32, OSStatus> {
    let needed = value.size_in_bytes();
    if needed == 0 {
        return Ok(0);
    }
    if out.is_null() {
        return Err(ERR_BAD_OBJECT);
    }
    if (out_size as usize) < needed {
        return Err(ERR_BAD_PROPERTY_SIZE);
    }
    // SAFETY: `out` is non-null and `needed` never exceeds `out_size`
    // (checked above) or the source value's own size (each arm copies
    // exactly `size_of::<T>()` bytes from a local, fully-initialised value
    // of that same type), so every `copy_out` call stays within both
    // buffers.
    unsafe {
        match value {
            Value::U32(v) => copy_out(&v, out, needed),
            Value::F64(v) => copy_out(&v, out, needed),
            Value::Str(s) => {
                let cf = cfstring_from_str(s);
                // `CFRelease(NULL)` crashes. If CoreFoundation failed to
                // create the string, do not hand a null CFStringRef to the
                // host — it will eventually try to release whatever
                // pointer-sized value sits in this slot.
                if cf.is_null() {
                    return Err(ERR_UNSPECIFIED);
                }
                copy_out(&cf, out, needed);
            }
            Value::Url => {
                let Some(url) = icon_resource_url() else {
                    // Bundle not registered or resource missing (see
                    // `icon_resource_url`'s doc) — do not hand a null
                    // CFURLRef to the host, same hazard as the `Value::Str`
                    // arm above.
                    return Err(ERR_UNSPECIFIED);
                };
                copy_out(&url, out, needed);
            }
            Value::U32Arr1(v) => copy_out(&v, out, needed),
            Value::U32Arr2(v) => copy_out(&v, out, needed),
            Value::U32Arr5(v) => copy_out(&v, out, needed),
            Value::U32ArrEmpty => {}
            Value::Range1(v) => copy_out(&v, out, needed),
            Value::Range6(v) => copy_out(&v, out, needed),
            Value::Asbd(v) => copy_out(&v, out, needed),
            Value::RangedAsbdArr6(v) => copy_out(&v, out, needed),
        }
    }
    Ok(needed as u32)
}

// ---- The property and IO vtable functions ---------------------------------

/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_HasProperty(
    _d: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _p: Pid,
    a: *const AudioObjectPropertyAddress,
) -> u8 {
    let Some(addr) = read_property_address(a) else {
        return 0;
    };
    u8::from(properties::has_property(id, &addr))
}

/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_IsPropertySettable(
    _d: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _p: Pid,
    a: *const AudioObjectPropertyAddress,
    out: *mut u8,
) -> OSStatus {
    if out.is_null() {
        return ERR_BAD_OBJECT;
    }
    let Some(addr) = read_property_address(a) else {
        return ERR_BAD_OBJECT;
    };
    match properties::is_property_settable(id, &addr) {
        Ok(settable) => {
            // SAFETY: `out` was null-checked above; the host guarantees it
            // is a valid, writable `u8` for the duration of this call.
            unsafe { *out = u8::from(settable) };
            OK
        }
        Err(status) => status,
    }
}

/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_GetPropertyDataSize(
    _d: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _p: Pid,
    a: *const AudioObjectPropertyAddress,
    qs: u32,
    q: *const c_void,
    out: *mut u32,
) -> OSStatus {
    if out.is_null() {
        return ERR_BAD_OBJECT;
    }
    let Some(addr) = read_property_address(a) else {
        return ERR_BAD_OBJECT;
    };
    // Only decode the qualifier as a CFStringRef for the one property that
    // actually uses one (`TranslateUIDToDevice`). Any other selector
    // reaching here with an 8-byte qualifier (whatever it actually is) must
    // not be blindly handed to `CFStringGetCString`.
    let mut scratch = [0u8; 256];
    let qualifier = if addr.selector == fourcc(b"uidd") {
        read_qualifier_str(qs, q, &mut scratch)
    } else {
        None
    };
    match properties::get_property_data_size(id, &addr, qualifier) {
        Ok(size) => {
            // SAFETY: `out` was null-checked above; host-guaranteed valid
            // and writable for the duration of this call.
            unsafe { *out = size as u32 };
            OK
        }
        Err(status) => status,
    }
}

/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_GetPropertyData(
    _d: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _p: Pid,
    a: *const AudioObjectPropertyAddress,
    qs: u32,
    q: *const c_void,
    data_size: u32,
    out_size: *mut u32,
    out: *mut c_void,
) -> OSStatus {
    if out_size.is_null() {
        return ERR_BAD_OBJECT;
    }
    let Some(addr) = read_property_address(a) else {
        return ERR_BAD_OBJECT;
    };
    // See the matching comment in senda_GetPropertyDataSize.
    let mut scratch = [0u8; 256];
    let qualifier = if addr.selector == fourcc(b"uidd") {
        read_qualifier_str(qs, q, &mut scratch)
    } else {
        None
    };
    match properties::get_property_data(id, &addr, qualifier) {
        Ok(value) => match write_value(value, data_size, out) {
            Ok(written) => {
                // SAFETY: `out_size` was null-checked above; host-guaranteed
                // valid and writable for the duration of this call.
                unsafe { *out_size = written };
                OK
            }
            // Reject rather than truncate — see write_value's doc.
            Err(status) => status,
        },
        Err(status) => status,
    }
}

/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_SetPropertyData(
    _d: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _p: Pid,
    a: *const AudioObjectPropertyAddress,
    _qs: u32,
    _q: *const c_void,
    data_size: u32,
    data: *const c_void,
) -> OSStatus {
    let Some(addr) = read_property_address(a) else {
        return ERR_BAD_OBJECT;
    };
    if data.is_null() {
        return ERR_BAD_OBJECT;
    }
    // SAFETY: `data` is non-null; per the HAL contract, `data_size` is the
    // honest size of the allocation behind `data` for a SetPropertyData
    // call, so a slice of at most `data_size` bytes lies within it. The
    // 8-byte cap only bounds the copy — the largest value this driver
    // reads out of `data` is an f64 sample rate — and is no defence against
    // a wrong `data_size` (a `'fsiz'` set is a 4-byte allocation).
    let len = (data_size as usize).min(8);
    let slice: &[u8] = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) };
    let result = properties::set_property_data(id, &addr, slice);
    // A successful NominalSampleRate change is the one event that
    // legitimately voids this device's cached zero-timestamp timeline.
    // `properties::set_property_data` above already validated the requested
    // rate against `device::SAMPLE_RATES` (rejecting anything else with
    // `ERR_ILLEGAL_VALUE`) and stored it in `STATE`, so the new rate is read
    // back from there rather than re-parsed out of `slice` here.
    //
    if result.is_ok() && addr.selector == fourcc(b"nsrt") {
        if let Some(dev) = device::find_by_object_id(id) {
            if let (Some(clock), Some(state)) = (CLOCKS.get(dev), device::STATE.get(dev)) {
                // A sample-rate change also invalidates the ring contents.
                // The wipe is skipped while IO is running: `Ring::zero` is
                // a second writer, and a `'nsrt'` set applies synchronously
                // while a client's IO thread may be inside `DoIOOperation`.
                // A live writer supersedes old content anyway, and
                // `senda_StartIO`'s 0->1 transition zeroes unconditionally.
                // The `io_running` load and the wipe are not atomic with
                // respect to `StartIO` (`START_STOP_LOCKS` is not held
                // here); the HAL is not expected to issue a `StartIO`
                // concurrently with a rate change on this path. Only a ring
                // that already exists is zeroed (`rings_for`, not
                // `ensure_ring`).
                if !state.io_running.load(Ordering::Acquire) {
                    if let Some(ring) = io::rings_for(dev) {
                        ring.zero();
                    }
                }
                // SAFETY: reads the monotonic tick counter; touches no memory.
                let now = unsafe { mach_absolute_time() };
                // `start` bumps the seed inside its own seqlock transaction, so the
                // reset count is never observable under the old seed.
                clock.start(now, state.rate(), host_ns_per_tick());
            }
        }
    }
    match result {
        Ok(()) => OK,
        Err(status) => status,
    }
}

/// Serialises `senda_StartIO`/`senda_StopIO`'s ref-count transition, one
/// lock per device (indexed identically to `device::STATE`/`clock::CLOCKS`).
///
/// A bare `fetch_add` on `PerDevice::client_count` identifies who won the
/// 0->1 race but does nothing to make a losing caller wait: a second
/// client's `StartIO` could see `prev == 1` and return `OK` before the
/// winner's `ensure_ring`/`zero`/`clock.start`/`io_running` store had
/// completed, leaving the HAL free to call `DoIOOperation` for that client
/// while `io::rings_for` still returns `None` or while `Ring::zero` is
/// mid-flight. The lock is therefore held across the whole transition.
/// Apple's NullAudio sample holds a mutex across the same transition.
///
/// `std::sync::Mutex` is fine here: the "no `Mutex`" constraint binds
/// `engine::` only, and this lock is never touched on the `DoIOOperation`
/// path.
static START_STOP_LOCKS: [std::sync::Mutex<()>; 5] = [
    std::sync::Mutex::new(()),
    std::sync::Mutex::new(()),
    std::sync::Mutex::new(()),
    std::sync::Mutex::new(()),
    std::sync::Mutex::new(()),
];

/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_StartIO(
    _d: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _c: u32,
) -> OSStatus {
    // The HAL calls `StartIO` once per client attaching to a device, not
    // once overall (mirroring Apple's own NullAudio.c sample: it keeps its
    // own `gDevice_IOIsRunning` ref count and only resets its anchor time
    // when that count is going from 0 to 1) — a second client's `StartIO`
    // must not reset a timeline, or wipe a ring, the first client is
    // already relying on. `client_count` is that ref count; only the
    // transition out of zero (re)anchors the clock and (re)zeroes the
    // ring. An unrecognised `id` is not an error here — the HAL is not
    // required to have queried this device through the property-dispatch
    // path first — it is simply a no-op.
    if let Some(dev) = device::find_by_object_id(id) {
        if let Some(state) = device::STATE.get(dev) {
            if let Some(lock) = START_STOP_LOCKS.get(dev) {
                // Held across the entire fetch_add + conditional setup
                // below, not just the "did I win" check — see the doc
                // comment on `START_STOP_LOCKS` for why a losing caller
                // must not be able to proceed (and return `OK`) before the
                // winner's setup has fully completed. A poisoned lock
                // (only reachable if an earlier holder panicked while
                // holding it, which nothing in this block can do — no
                // `unwrap`/`expect`/`panic!`/indexing here) is recovered
                // rather than propagated: this function must never panic
                // across the `extern "C"` boundary either way.
                let _guard = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let prev = state.client_count.fetch_add(1, Ordering::AcqRel);
                if prev == 0 {
                    // Not real-time: `StartIO` is a control-thread call. A
                    // freshly created `Ring` already reads as "nothing
                    // written" (see `io::ensure_ring`'s doc comment), but a
                    // `Ring` reused from an earlier session must be wiped so
                    // this session never inherits the previous one's audio —
                    // `Ring::zero`'s own doc comment is why that call belongs
                    // here (a control-thread transition, no writer yet active
                    // for this session) and never on the `DoIOOperation` path.
                    if let Some(ring) = io::ensure_ring(dev) {
                        ring.zero();
                    }
                    if let Some(clock) = CLOCKS.get(dev) {
                        // SAFETY: reads the monotonic tick counter; touches no
                        // memory.
                        let now = unsafe { mach_absolute_time() };
                        clock.start(now, state.rate(), host_ns_per_tick());
                    }
                    state.io_running.store(true, Ordering::Release);
                }
            }
        }
    }
    OK
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_StopIO(
    _d: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _c: u32,
) -> OSStatus {
    // Mirrors `senda_StartIO`'s ref-count transition: only the last client
    // detaching (count 1 -> 0) marks the device as no longer running.
    // `fetch_update` rather than a bare `fetch_sub` — a `StopIO` without a
    // matching prior `StartIO` is not something the HAL's contract
    // permits, but this must never wrap `client_count` around to
    // `u32::MAX` (nor panic) if it somehow happened anyway; `checked_sub`
    // simply declines the update in that case. Guarded by the same
    // per-device lock as `senda_StartIO` (see its doc comment on
    // `START_STOP_LOCKS`) so a `StopIO` can never interleave with an
    // in-flight `StartIO` transition for the same device.
    if let Some(dev) = device::find_by_object_id(id) {
        if let Some(state) = device::STATE.get(dev) {
            if let Some(lock) = START_STOP_LOCKS.get(dev) {
                let _guard = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let prev =
                    state
                        .client_count
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |c| c.checked_sub(1));
                if prev == Ok(1) {
                    state.io_running.store(false, Ordering::Release);
                }
            }
        }
    }
    OK
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_GetZeroTimeStamp(
    _d: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _c: u32,
    st: *mut f64,
    ht: *mut u64,
    seed: *mut u64,
) -> OSStatus {
    let Some(dev) = device::find_by_object_id(id) else {
        return ERR_BAD_OBJECT;
    };
    let Some(clock) = CLOCKS.get(dev) else {
        return ERR_BAD_OBJECT;
    };
    // SAFETY: reads the monotonic tick counter; touches no memory. This is
    // the only clock read on the IO-thread hot path — `engine::clock::Clock`
    // itself never calls into FFI, per the "no unsafe in engine/" rule.
    let now = unsafe { mach_absolute_time() };
    let (sample_time, host_time, seed_val) = clock.zero_timestamp(now);
    if !st.is_null() {
        // SAFETY: host-guaranteed valid, writable `f64` for the duration of
        // this call (standard AudioServerPlugIn out-param contract). The
        // write relies on 8-byte alignment; the HAL allocates `st`, `ht`
        // and `seed` as their C types (`Float64`/`UInt64`), so that holds.
        unsafe { *st = sample_time };
    }
    if !ht.is_null() {
        // SAFETY: same contract as `st` above.
        unsafe { *ht = host_time };
    }
    if !seed.is_null() {
        // SAFETY: same contract as `st` above.
        unsafe { *seed = seed_val };
    }
    OK
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_WillDoIOOperation(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _c: u32,
    op: u32,
    will: *mut u8,
    will_do_in_place: *mut u8,
) -> OSStatus {
    // This driver only ever participates in the two operations
    // `senda_DoIOOperation` implements — everything else (the IO
    // thread/cycle bookkeeping operations, `ConvertInput`/`ProcessOutput`,
    // etc.) is declined with `*will = 0`.
    //
    // The second out-parameter is `outWillDoInPlace`, not a direction
    // flag. Apple's header (`AudioServerPlugIn.h`): "a Boolean where true
    // indicates that the device will perform the requested operation
    // entirely within the main buffer passed to the DoIOOperation routine.
    // If this value is false, it indicates that the device requires that
    // the secondary buffer be passed." `senda_DoIOOperation` never touches
    // the secondary buffer, only `main`, so in-place is true by
    // construction for both declared operations; Apple's NullAudio sample
    // and BlackHole answer `true` for both as well. `will_do`/`in_place`
    // are therefore identical in every case that isn't a decline — a
    // single condition, not a branch per operation with two identical arms.
    let declared = op == IO_OP_WRITE_MIX || op == IO_OP_READ_INPUT;
    let (will_do, in_place) = if declared { (1u8, 1u8) } else { (0u8, 0u8) };
    if !will.is_null() {
        // SAFETY: host-guaranteed valid, writable `u8` for the duration of
        // this call (standard AudioServerPlugIn out-param contract).
        unsafe { *will = will_do };
    }
    if !will_do_in_place.is_null() {
        // SAFETY: same contract as `will` above.
        unsafe { *will_do_in_place = in_place };
    }
    OK
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_BeginIOOperation(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _c: u32,
    _op: u32,
    _n: u32,
    _i: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus {
    OK
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_DoIOOperation(
    _d: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _sid: AudioObjectID,
    _c: u32,
    op: u32,
    n: u32,
    cycle_info: *const AudioServerPlugInIOCycleInfo,
    main: *mut c_void,
    _sec: *mut c_void,
) -> OSStatus {
    // Null-check first, per contract — return OK without touching audio
    // rather than dereferencing a null `AudioServerPlugInIOCycleInfo*` or
    // main-buffer pointer. `cycle_info` carries the HAL's sample time for
    // the cycle, which is what the loopback ring is indexed by; without it
    // there is nothing useful to do.
    if cycle_info.is_null() || main.is_null() {
        return OK;
    }
    let Some(dev) = device::find_by_object_id(id) else {
        return OK;
    };
    let Some(cfg) = device::DEVICES.get(dev) else {
        return OK;
    };
    let Some(state) = device::STATE.get(dev) else {
        return OK;
    };
    let channels = cfg.channels as usize;

    // SAFETY: `cycle_info` is non-null (checked above) and, per the HAL's
    // `DoIOOperation` contract, points to a valid, fully initialised
    // `AudioServerPlugInIOCycleInfo` for the duration of this call.
    // `AudioServerPlugInIOCycleInfo` is `Copy`, so this is a plain read of
    // host-owned memory, not a pointer retained past this call. The read
    // relies on 8-byte alignment (`u64`/`f64` fields); the HAL allocates
    // the struct as its C type, so that holds.
    let info = unsafe { *cycle_info };

    // The HAL-supplied frame count (`n`, `inIOBufferFrameSize`) is clamped
    // to `MAX_BUFFER_FRAMES`, the ceiling advertised via `'fsz#'`: the ring
    // cannot honour more frames than that, and the clamp bounds the slice
    // constructed below whatever `n` claims. The bound is the driver-wide
    // ceiling, not this device's `'fsiz'`-negotiated size — `n` is the
    // maximum across every attached client, so one client narrowing
    // `'fsiz'` must not truncate the others' audio.
    if (n as usize) > device::MAX_BUFFER_FRAMES as usize {
        // A clamp that silently truncates audio is invisible in practice,
        // so it is counted (`PerDevice::frame_clamps`).
        state.record_frame_clamp();
    }
    let frames = (n as usize).min(device::MAX_BUFFER_FRAMES as usize);
    if frames == 0 {
        return OK;
    }
    let Some(len) = frames.checked_mul(channels) else {
        return OK;
    };

    if op == IO_OP_WRITE_MIX {
        // `f64 as u64` saturates (negative -> 0, NaN -> 0, too-large ->
        // u64::MAX) rather than wrapping or invoking UB — plain Rust `as`
        // semantics, no explicit clamp needed.
        let sample_time = info.output_time.sample_time as u64;
        // SAFETY: `main` is non-null (checked above). Per the HAL's
        // `DoIOOperation` contract, `io_main_buffer` for a `WriteMix`
        // operation holds at least `n * channels` valid `f32` samples in
        // this device's stream format, and `f32` alignment of `main` is
        // part of that contract. `frames <= n` after the clamp above, so
        // `len` never exceeds what the HAL sized the call for.
        let src = unsafe { std::slice::from_raw_parts(main.cast::<f32>(), len) };
        // No sanitising, no clamping, no gain: bit-transparency is a
        // global constraint, and `Ring::write` already preserves every bit
        // pattern (NaN payloads, denormals, signed zero, infinities)
        // exactly.
        io::on_write_mix(dev, sample_time, src, frames);
    } else if op == IO_OP_READ_INPUT {
        let sample_time = info.input_time.sample_time as u64;
        // SAFETY: same contract as the `WriteMix` arm above, except this
        // operation writes into the host's buffer rather than reading from
        // it. The slice is exclusive for the call: the HAL does not touch
        // the buffer while `DoIOOperation` is running.
        let dst = unsafe { std::slice::from_raw_parts_mut(main.cast::<f32>(), len) };
        io::on_read_input(dev, sample_time, dst, frames);
    }
    // Any other operation ID: `senda_WillDoIOOperation` never declares
    // `*will = 1` for it, so the HAL should never route it here — declining
    // silently (rather than erroring) matches this driver's existing
    // stance on calls outside its declared contract.
    OK
}
/// # Safety
/// Called by the HAL through the vtable. Every pointer argument must be valid for
/// the use the AudioServerPlugIn contract documents for this entry point.
unsafe extern "C" fn senda_EndIOOperation(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _c: u32,
    _op: u32,
    _n: u32,
    _i: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus {
    OK
}

static mut DRIVER_INTERFACE: AudioServerPlugInDriverInterface = AudioServerPlugInDriverInterface {
    _reserved: std::ptr::null_mut(),
    QueryInterface: senda_QueryInterface,
    AddRef: senda_AddRef,
    Release: senda_Release,
    Initialize: senda_Initialize,
    CreateDevice: senda_CreateDevice,
    DestroyDevice: senda_DestroyDevice,
    AddDeviceClient: senda_AddDeviceClient,
    RemoveDeviceClient: senda_RemoveDeviceClient,
    PerformDeviceConfigurationChange: senda_PerformDeviceConfigurationChange,
    AbortDeviceConfigurationChange: senda_AbortDeviceConfigurationChange,
    HasProperty: senda_HasProperty,
    IsPropertySettable: senda_IsPropertySettable,
    GetPropertyDataSize: senda_GetPropertyDataSize,
    GetPropertyData: senda_GetPropertyData,
    SetPropertyData: senda_SetPropertyData,
    StartIO: senda_StartIO,
    StopIO: senda_StopIO,
    GetZeroTimeStamp: senda_GetZeroTimeStamp,
    WillDoIOOperation: senda_WillDoIOOperation,
    BeginIOOperation: senda_BeginIOOperation,
    DoIOOperation: senda_DoIOOperation,
    EndIOOperation: senda_EndIOOperation,
};

/// A `static` holding a raw pointer would need a `Sync` wrapper, so the
/// pointer is produced by a function instead; returning the address of the
/// static interface has identical semantics.
pub fn driver_interface_ptr() -> *mut AudioServerPlugInDriverInterface {
    &raw mut DRIVER_INTERFACE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_constants_match_the_documented_decimal_values() {
        // Self-check on the `fourcc`-derived constants, rather than trusting
        // hand arithmetic: each is pinned against the decimal value Apple's
        // headers give, so a change to how a constant is spelled can never
        // silently change the wire value a real HAL would see.
        assert_eq!(ERR_BAD_PROPERTY_SIZE, 561211770);
        assert_eq!(ERR_UNKNOWN_PROPERTY, 2003332927);
        assert_eq!(ERR_UNSUPPORTED, 1970171760);
        assert_eq!(ERR_BAD_OBJECT, 560947818);
    }

    // write_value must reject an undersized buffer rather than
    // truncate into it, and must write exactly `size_in_bytes()` bytes —
    // never more, never fewer — whenever the buffer is big enough.
    #[test]
    fn write_value_rejects_undersized_out_size_and_writes_exact_bytes_when_sufficient() {
        const POISON: u8 = 0xAA;
        let mut buf = [POISON; 32];

        // out_size = 0: Value::U32 needs 4 bytes, so this is rejected and
        // the buffer is left completely untouched.
        let out = buf.as_mut_ptr().cast::<c_void>();
        assert_eq!(
            write_value(Value::U32(0x1122_3344), 0, out),
            Err(ERR_BAD_PROPERTY_SIZE)
        );
        assert_eq!(buf, [POISON; 32]);

        // out_size = 4: exact fit.
        let out = buf.as_mut_ptr().cast::<c_void>();
        assert_eq!(write_value(Value::U32(0x1122_3344), 4, out), Ok(4));
        assert_eq!(&buf[..4], 0x1122_3344_u32.to_ne_bytes().as_slice());
        assert_eq!(&buf[4..], [POISON; 28].as_slice());

        // out_size = 8: more room than needed; still writes exactly 4
        // bytes, not 8.
        buf = [POISON; 32];
        let out = buf.as_mut_ptr().cast::<c_void>();
        assert_eq!(write_value(Value::U32(0x5566_7788), 8, out), Ok(4));
        assert_eq!(&buf[..4], 0x5566_7788_u32.to_ne_bytes().as_slice());
        assert_eq!(&buf[4..], [POISON; 28].as_slice());

        // out_size = 64: far more room than the real 32-byte buffer even
        // has, but write_value only ever copies `needed` (4) bytes — the
        // inflated out_size never turns into an out-of-bounds write.
        buf = [POISON; 32];
        let out = buf.as_mut_ptr().cast::<c_void>();
        assert_eq!(write_value(Value::U32(0x99AA_BBCC), 64, out), Ok(4));
        assert_eq!(&buf[..4], 0x99AA_BBCC_u32.to_ne_bytes().as_slice());
    }

    #[test]
    fn write_value_of_zero_sized_variant_is_always_ok_even_with_null_out() {
        assert_eq!(
            write_value(Value::U32ArrEmpty, 0, std::ptr::null_mut()),
            Ok(0)
        );
    }

    #[test]
    fn write_value_rejects_null_out_for_a_non_zero_sized_value() {
        assert_eq!(
            write_value(Value::U32(1), 4, std::ptr::null_mut()),
            Err(ERR_BAD_OBJECT)
        );
    }

    // An undersized buffer must be rejected before
    // `Value::Url`'s arm ever calls `icon_resource_url` (which would create
    // CoreFoundation objects that then leak). `out_size = 4 < 8` is checked
    // unconditionally ahead of the match in `write_value`, so this holds
    // regardless of whether a driver bundle is registered in this process —
    // the buffer stays untouched either way.
    #[test]
    fn write_value_rejects_undersized_buffer_for_icon_url_before_creating_anything() {
        const POISON: u8 = 0xAA;
        let mut buf = [POISON; 8];
        let out = buf.as_mut_ptr().cast::<c_void>();
        assert_eq!(write_value(Value::Url, 4, out), Err(ERR_BAD_PROPERTY_SIZE));
        assert_eq!(buf, [POISON; 8]);
    }

    // In the `cargo test` process, no CFBundle is registered under
    // `DRIVER_BUNDLE_ID` (that registration only happens when `coreaudiod`
    // actually loads the built `.driver` bundle), so `icon_resource_url`
    // deterministically returns `None` here. This exercises the "bundle not
    // found" branch and confirms
    // `write_value` reports `ERR_UNSPECIFIED` rather than writing a null
    // `CFURLRef` into the host's buffer.
    #[test]
    fn write_value_of_icon_url_is_unspecified_when_the_driver_bundle_is_not_registered() {
        assert!(icon_resource_url().is_none());
        let mut buf = [0u8; 8];
        let out = buf.as_mut_ptr().cast::<c_void>();
        assert_eq!(write_value(Value::Url, 8, out), Err(ERR_UNSPECIFIED));
    }

    // A qualifier is only ever decoded for TranslateUIDToDevice. This
    // doesn't call the real CFString-decoding path (that needs a live
    // CFStringRef), but it pins down that non-'uidd' selectors reaching
    // senda_GetPropertyDataSize/senda_GetPropertyData never even attempt
    // it, regardless of what garbage sits in the qualifier pointer/size.
    #[test]
    fn get_property_data_size_ignores_qualifier_for_non_uidd_selectors() {
        // An 8-byte qualifier that is NOT a valid CFStringRef (a bogus
        // non-null pointer value). If this were passed to
        // CFStringGetCString for a non-'uidd' property, it would crash.
        let bogus: u64 = 0xDEAD_BEEF_DEAD_BEEF;
        let addr = AudioObjectPropertyAddress {
            selector: fourcc(b"dev#"),
            scope: 0,
            element: 0,
        };
        let mut out: u32 = 0;
        // AudioObjectID 1 == kAudioObjectPlugInObject (device::PLUGIN_OBJECT_ID);
        // hardcoded here rather than imported to keep this ffi-layer test
        // from depending on engine::device internals.
        let status = unsafe {
            senda_GetPropertyDataSize(
                std::ptr::null_mut(),
                1,
                0,
                &addr,
                8,
                (&bogus as *const u64).cast::<c_void>(),
                &mut out,
            )
        };
        assert_eq!(status, OK);
        assert_eq!(out, 20); // DeviceList: 5 AudioObjectIDs
    }

    // ---- Clock wiring ------------------------------------------------------
    //
    // `STATE` and `CLOCKS` are process-global `static`s and `cargo test` runs
    // tests concurrently in one process, so each test below claims a device
    // index no other test in the crate mutates the NominalSampleRate of
    // (`engine::properties`'s tests already own id 50 / index 4).

    #[test]
    fn get_zero_time_stamp_rejects_unknown_device_and_leaves_outputs_untouched() {
        let mut st = 123.0_f64;
        let mut ht = 456_u64;
        let mut seed = 789_u64;
        let status = unsafe {
            senda_GetZeroTimeStamp(std::ptr::null_mut(), 9999, 0, &mut st, &mut ht, &mut seed)
        };
        assert_eq!(status, ERR_BAD_OBJECT);
        assert_eq!(st, 123.0);
        assert_eq!(ht, 456);
        assert_eq!(seed, 789);
    }

    // Uses device id 30 (index 2), exclusive to this test.
    #[test]
    fn start_io_then_get_zero_time_stamp_is_stable_and_period_aligned() {
        assert_eq!(unsafe { senda_StartIO(std::ptr::null_mut(), 30, 0) }, OK);

        let mut st1 = -1.0_f64;
        let mut ht1 = 0_u64;
        let mut seed1 = 0_u64;
        assert_eq!(
            unsafe {
                senda_GetZeroTimeStamp(std::ptr::null_mut(), 30, 0, &mut st1, &mut ht1, &mut seed1)
            },
            OK
        );

        let mut st2 = -1.0_f64;
        let mut ht2 = 0_u64;
        let mut seed2 = 0_u64;
        assert_eq!(
            unsafe {
                senda_GetZeroTimeStamp(std::ptr::null_mut(), 30, 0, &mut st2, &mut ht2, &mut seed2)
            },
            OK
        );

        assert!(
            st2 >= st1,
            "sample time must not go backwards: {st2} < {st1}"
        );
        let ring_frames = device::RING_FRAMES as f64;
        assert_eq!(
            st1 % ring_frames,
            0.0,
            "must land on a RING_FRAMES boundary"
        );
        assert_eq!(
            st2 % ring_frames,
            0.0,
            "must land on a RING_FRAMES boundary"
        );
        assert_eq!(
            seed1, seed2,
            "seed must not change without an explicit rate change"
        );

        // The 0->1 transition above must have set `io_running`; this checks
        // the value by number, not only its size.
        let running = AudioObjectPropertyAddress {
            selector: fourcc(b"goin"),
            scope: 0,
            element: 0,
        };
        assert!(
            matches!(properties::device_property(2, &running), Ok(Value::U32(1))),
            "io_running must be set true after StartIO's 0->1 transition"
        );

        // A second `StartIO` on an already-running device (`client_count`
        // going 1->2, not 0->1) must be a silent no-op with respect to the
        // clock: only the first client's `StartIO` may (re)anchor the
        // timeline. An unconditional `Clock::start` would reset `st` to 0
        // and change `seed` here.
        assert_eq!(unsafe { senda_StartIO(std::ptr::null_mut(), 30, 0) }, OK);
        let mut st3 = -1.0_f64;
        let mut ht3 = 0_u64;
        let mut seed3 = 0_u64;
        assert_eq!(
            unsafe {
                senda_GetZeroTimeStamp(std::ptr::null_mut(), 30, 0, &mut st3, &mut ht3, &mut seed3)
            },
            OK
        );
        assert!(
            st3 >= st2,
            "sample time must not go backwards after a second StartIO: {st3} < {st2}"
        );
        assert_eq!(
            seed3, seed1,
            "a second StartIO on an already-running device must not re-anchor the clock"
        );

        // ZeroTimeStampPeriod equals the clock advance period, and neither
        // changes when a client alters the buffer size. Drive `'fsiz'`
        // through the real `senda_SetPropertyData` entry point on a device
        // with IO already running, then confirm both that `'ring'`
        // (ZeroTimeStampPeriod) still reads back `RING_FRAMES` and that
        // `senda_GetZeroTimeStamp`'s advance period is unmoved.
        let fsiz_addr = AudioObjectPropertyAddress {
            selector: fourcc(b"fsiz"),
            scope: 0,
            element: 0,
        };
        let narrowed: u32 = 64;
        let narrowed_bytes = narrowed.to_ne_bytes();
        assert_eq!(
            unsafe {
                senda_SetPropertyData(
                    std::ptr::null_mut(),
                    30,
                    0,
                    &fsiz_addr,
                    0,
                    std::ptr::null(),
                    narrowed_bytes.len() as u32,
                    narrowed_bytes.as_ptr().cast::<c_void>(),
                )
            },
            OK
        );
        assert_eq!(
            device::STATE.get(2).map(device::PerDevice::buffer_frames),
            Some(64),
            "setup invalid: 'fsiz' must have actually narrowed this device's negotiated size"
        );

        let ring_period = AudioObjectPropertyAddress {
            selector: fourcc(b"ring"),
            scope: 0,
            element: 0,
        };
        assert!(
            matches!(
                properties::device_property(2, &ring_period),
                Ok(Value::U32(32768))
            ),
            "ZeroTimeStampPeriod ('ring') must remain RING_FRAMES even after a client narrows \
             'fsiz' — it must never be derived from ioBufferFrameSize"
        );

        let mut st4 = -1.0_f64;
        let mut ht4 = 0_u64;
        let mut seed4 = 0_u64;
        assert_eq!(
            unsafe {
                senda_GetZeroTimeStamp(std::ptr::null_mut(), 30, 0, &mut st4, &mut ht4, &mut seed4)
            },
            OK
        );
        assert!(
            st4 >= st3,
            "sample time must not go backwards after a 'fsiz' change: {st4} < {st3}"
        );
        assert_eq!(
            st4 % ring_frames,
            0.0,
            "the clock's advance period must still land on a RING_FRAMES boundary after a \
             'fsiz' change, not a boundary derived from the narrowed buffer size"
        );
        assert_eq!(
            seed4, seed1,
            "a 'fsiz' change must not touch the clock's seed — only a NominalSampleRate change may"
        );
    }

    // Uses device id 40 (index 3), exclusive to this test.
    #[test]
    fn set_property_data_sample_rate_change_resets_the_clock_and_bumps_the_seed() {
        assert_eq!(unsafe { senda_StartIO(std::ptr::null_mut(), 40, 0) }, OK);

        let mut st1 = -1.0_f64;
        let mut ht1 = 0_u64;
        let mut seed1 = 0_u64;
        assert_eq!(
            unsafe {
                senda_GetZeroTimeStamp(std::ptr::null_mut(), 40, 0, &mut st1, &mut ht1, &mut seed1)
            },
            OK
        );

        let addr = AudioObjectPropertyAddress {
            selector: fourcc(b"nsrt"),
            scope: 0,
            element: 0,
        };
        let rate: f64 = 96000.0;
        let bytes = rate.to_ne_bytes();
        let status = unsafe {
            senda_SetPropertyData(
                std::ptr::null_mut(),
                40,
                0,
                &addr,
                0,
                std::ptr::null(),
                bytes.len() as u32,
                bytes.as_ptr().cast::<c_void>(),
            )
        };
        assert_eq!(status, OK);

        let mut st2 = -1.0_f64;
        let mut ht2 = 0_u64;
        let mut seed2 = 0_u64;
        assert_eq!(
            unsafe {
                senda_GetZeroTimeStamp(std::ptr::null_mut(), 40, 0, &mut st2, &mut ht2, &mut seed2)
            },
            OK
        );

        assert_eq!(
            st2, 0.0,
            "a sample-rate change must restart the timeline from zero"
        );
        assert_ne!(
            seed1, seed2,
            "seed must change so the HAL discards its cached timeline"
        );

        // The ref-count state machine's other transition: `client_count`
        // goes 1->0 here (from the single `StartIO` above), which must
        // clear `io_running`.
        let running = AudioObjectPropertyAddress {
            selector: fourcc(b"goin"),
            scope: 0,
            element: 0,
        };
        assert!(
            matches!(properties::device_property(3, &running), Ok(Value::U32(1))),
            "io_running must have been set true by the StartIO above"
        );
        assert_eq!(unsafe { senda_StopIO(std::ptr::null_mut(), 40, 0) }, OK);
        assert!(
            matches!(properties::device_property(3, &running), Ok(Value::U32(0))),
            "StopIO's 1->0 transition must clear io_running"
        );

        // An unmatched StopIO (client_count already 0) must decline rather
        // than wrapping client_count to u32::MAX — the `checked_sub` guard
        // at `senda_StopIO`. Not directly observable as a return value
        // (StopIO always answers OK per the HAL's contract), so this is
        // proven indirectly: if the counter had wrapped, the very next
        // StartIO's `fetch_add` would land on a nonzero `prev`
        // (`u32::MAX`, not `0`) and would therefore not re-arm
        // `io_running` below.
        assert_eq!(unsafe { senda_StopIO(std::ptr::null_mut(), 40, 0) }, OK);
        assert!(
            matches!(properties::device_property(3, &running), Ok(Value::U32(0))),
            "an unmatched StopIO must not resurrect io_running"
        );
        assert_eq!(unsafe { senda_StartIO(std::ptr::null_mut(), 40, 0) }, OK);
        assert!(
            matches!(properties::device_property(3, &running), Ok(Value::U32(1))),
            "a legitimate StartIO after an unmatched StopIO must still re-arm io_running              (proves client_count was not corrupted/wrapped by the unmatched StopIO)"
        );
    }

    // Uses device id 20 (index 1) — only ever used elsewhere for BufferFrameSize
    // (a different STATE field, and this path never reaches the clock at all
    // since the rate is rejected before `properties::set_property_data`
    // returns `Ok`).
    #[test]
    fn set_property_data_rejects_unsupported_rate_without_touching_the_clock() {
        // Read the clock before and after, so this test verifies its own
        // name: checking only the rejection status could not catch a
        // regression that bumped the seed unconditionally, regardless of
        // whether the rate was accepted.
        let mut st_before = -1.0_f64;
        let mut ht_before = 0_u64;
        let mut seed_before = 0_u64;
        assert_eq!(
            unsafe {
                senda_GetZeroTimeStamp(
                    std::ptr::null_mut(),
                    20,
                    0,
                    &mut st_before,
                    &mut ht_before,
                    &mut seed_before,
                )
            },
            OK
        );

        let addr = AudioObjectPropertyAddress {
            selector: fourcc(b"nsrt"),
            scope: 0,
            element: 0,
        };
        let bad_rate: f64 = 12345.0;
        let bytes = bad_rate.to_ne_bytes();
        let status = unsafe {
            senda_SetPropertyData(
                std::ptr::null_mut(),
                20,
                0,
                &addr,
                0,
                std::ptr::null(),
                bytes.len() as u32,
                bytes.as_ptr().cast::<c_void>(),
            )
        };
        assert_eq!(status, properties::ERR_ILLEGAL_VALUE);

        let mut st_after = -1.0_f64;
        let mut ht_after = 0_u64;
        let mut seed_after = 0_u64;
        assert_eq!(
            unsafe {
                senda_GetZeroTimeStamp(
                    std::ptr::null_mut(),
                    20,
                    0,
                    &mut st_after,
                    &mut ht_after,
                    &mut seed_after,
                )
            },
            OK
        );
        assert_eq!(
            seed_before, seed_after,
            "a rejected rate change must not touch the clock's seed"
        );
    }

    // ---- IO path wiring ----------------------------------------------------
    //
    // `STATE`/`CLOCKS`/`engine::io`'s ring registry are process-global
    // `static`s and `cargo test` runs tests concurrently in one process, so
    // a test that calls `senda_StartIO` on device index N is the only test
    // in the crate to start IO on N. Indices 2 and 3 belong to the
    // clock-wiring tests above and index 1 to `engine::io`'s tests, leaving
    // 0 (id 10, 2ch) and 4 (id 50, 128ch); `engine::properties`'s id-50
    // test never starts IO, so it cannot race index 4 here.
    //
    // `senda_StartIO` is transition-sensitive: only the 0->1 transition
    // (re)anchors the clock and zeroes the ring, so a second call on the
    // same index is a 1->2 no-op. Do not add a test that calls it on an
    // index another test owns expecting a fresh 0->1 transition.

    /// Builds a minimal `AudioServerPlugInIOCycleInfo` carrying exactly one
    /// populated timestamp — `output_time` for a `WriteMix` cycle,
    /// `input_time` for a `ReadInput` one — matching what
    /// `senda_DoIOOperation` actually reads. Every other field is a benign
    /// zero; nothing under test consults them.
    fn cycle_info_with(sample_time: f64, is_input: bool) -> AudioServerPlugInIOCycleInfo {
        let ts = AudioTimeStamp {
            sample_time,
            ..AudioTimeStamp::default()
        };
        let mut info = AudioServerPlugInIOCycleInfo {
            io_cycle_counter: 0,
            nominal_io_buffer_frame_size: device::BLOCK as u32,
            current_time: AudioTimeStamp::default(),
            input_time: AudioTimeStamp::default(),
            output_time: AudioTimeStamp::default(),
            main_host_ticks_per_frame: 0.0,
            device_host_ticks_per_frame: 0.0,
        };
        if is_input {
            info.input_time = ts;
        } else {
            info.output_time = ts;
        }
        info
    }

    #[test]
    fn will_do_io_operation_declares_wmix_and_rinp_and_declines_everything_else() {
        // `IO_OP_WRITE_MIX`/`IO_OP_READ_INPUT` hold Apple's `'rite'`/`'read'`,
        // and the second out-param is `outWillDoInPlace` — always `1` for
        // both operations this driver declares, since `senda_DoIOOperation`
        // only ever touches the main buffer.
        let mut will = 9u8;
        let mut will_do_in_place = 9u8;
        assert_eq!(
            unsafe {
                senda_WillDoIOOperation(
                    std::ptr::null_mut(),
                    10,
                    0,
                    IO_OP_WRITE_MIX,
                    &mut will,
                    &mut will_do_in_place,
                )
            },
            OK
        );
        assert_eq!(
            (will, will_do_in_place),
            (1, 1),
            "WriteMix: will-do, entirely in the main buffer"
        );

        will = 9;
        will_do_in_place = 9;
        assert_eq!(
            unsafe {
                senda_WillDoIOOperation(
                    std::ptr::null_mut(),
                    10,
                    0,
                    IO_OP_READ_INPUT,
                    &mut will,
                    &mut will_do_in_place,
                )
            },
            OK
        );
        assert_eq!(
            (will, will_do_in_place),
            (1, 1),
            "ReadInput: will-do, entirely in the main buffer"
        );

        will = 9;
        will_do_in_place = 9;
        assert_eq!(
            unsafe {
                senda_WillDoIOOperation(
                    std::ptr::null_mut(),
                    10,
                    0,
                    fourcc(b"proc"),
                    &mut will,
                    &mut will_do_in_place,
                )
            },
            OK
        );
        assert_eq!(
            (will, will_do_in_place),
            (0, 0),
            "an undeclared operation must be declined"
        );

        // Null out-params must not crash.
        assert_eq!(
            unsafe {
                senda_WillDoIOOperation(
                    std::ptr::null_mut(),
                    10,
                    0,
                    IO_OP_WRITE_MIX,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            OK
        );
    }

    // Exclusive to device id 50 (index 4, 128ch) — see this section's
    // module-level exclusivity note above.
    #[test]
    fn do_io_operation_indexes_by_hal_sample_time_and_bounds_untrusted_frame_counts() {
        assert_eq!(unsafe { senda_StartIO(std::ptr::null_mut(), 50, 0) }, OK);
        let channels = 128usize;

        // Null cycle_info: OK, no-op — never dereferenced.
        let mut buf = vec![1.0f32; device::BLOCK * channels];
        assert_eq!(
            unsafe {
                senda_DoIOOperation(
                    std::ptr::null_mut(),
                    50,
                    51,
                    0,
                    IO_OP_WRITE_MIX,
                    device::BLOCK as u32,
                    std::ptr::null(),
                    buf.as_mut_ptr().cast::<c_void>(),
                    std::ptr::null_mut(),
                )
            },
            OK
        );

        // Null main buffer: OK, no-op — never dereferenced.
        let info_zero = cycle_info_with(0.0, false);
        assert_eq!(
            unsafe {
                senda_DoIOOperation(
                    std::ptr::null_mut(),
                    50,
                    51,
                    0,
                    IO_OP_WRITE_MIX,
                    device::BLOCK as u32,
                    &info_zero,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            OK
        );

        // Two `WriteMix` cycles at different HAL-provided sample times must
        // land at independently addressable offsets. If `senda_DoIOOperation`
        // ignored `cycle_info` (always used sample_time == 0, say), the
        // second write below would clobber the first and `dst_a` would come
        // back `2.0`, not `1.0`.
        let block = device::BLOCK as u64;
        let mut src_a = vec![1.0f32; device::BLOCK * channels];
        let mut src_b = vec![2.0f32; device::BLOCK * channels];

        let info_a = cycle_info_with(0.0, false);
        assert_eq!(
            unsafe {
                senda_DoIOOperation(
                    std::ptr::null_mut(),
                    50,
                    51,
                    0,
                    IO_OP_WRITE_MIX,
                    device::BLOCK as u32,
                    &info_a,
                    src_a.as_mut_ptr().cast::<c_void>(),
                    std::ptr::null_mut(),
                )
            },
            OK
        );
        let info_b = cycle_info_with(block as f64, false);
        assert_eq!(
            unsafe {
                senda_DoIOOperation(
                    std::ptr::null_mut(),
                    50,
                    51,
                    0,
                    IO_OP_WRITE_MIX,
                    device::BLOCK as u32,
                    &info_b,
                    src_b.as_mut_ptr().cast::<c_void>(),
                    std::ptr::null_mut(),
                )
            },
            OK
        );

        let mut dst_a = vec![9.0f32; device::BLOCK * channels];
        let read_a = cycle_info_with(0.0, true);
        assert_eq!(
            unsafe {
                senda_DoIOOperation(
                    std::ptr::null_mut(),
                    50,
                    52,
                    0,
                    IO_OP_READ_INPUT,
                    device::BLOCK as u32,
                    &read_a,
                    dst_a.as_mut_ptr().cast::<c_void>(),
                    std::ptr::null_mut(),
                )
            },
            OK
        );
        assert!(
            dst_a.iter().all(|&x| x == 1.0),
            "sample time 0 must still read back the FIRST write, not the second"
        );

        let mut dst_b = vec![9.0f32; device::BLOCK * channels];
        let read_b = cycle_info_with(block as f64, true);
        assert_eq!(
            unsafe {
                senda_DoIOOperation(
                    std::ptr::null_mut(),
                    50,
                    52,
                    0,
                    IO_OP_READ_INPUT,
                    device::BLOCK as u32,
                    &read_b,
                    dst_b.as_mut_ptr().cast::<c_void>(),
                    std::ptr::null_mut(),
                )
            },
            OK
        );
        assert!(
            dst_b.iter().all(|&x| x == 2.0),
            "sample time BLOCK must read back the SECOND write, at its own independent offset"
        );

        // Frame-count clamping: `n` must never be trusted past the driver's
        // hard ceiling (`MAX_BUFFER_FRAMES`, advertised via `'fsz#'`). The
        // buffer passed in is allocated to `MAX_BUFFER_FRAMES * channels`
        // plus one extra frame of canary, so constructing the raw slice
        // inside `senda_DoIOOperation` stays memory-safe whether or not the
        // clamp is correct; the canary just past the ceiling is what detects
        // a broken clamp. The clamp bounds against the driver-wide ceiling,
        // not this device's `'fsiz'`-negotiated size — the round trip after
        // this proves why.
        let max_len = device::MAX_BUFFER_FRAMES as usize * channels;
        let mut dst = vec![9.0f32; max_len + channels];
        for slot in dst.iter_mut().skip(max_len) {
            *slot = -1.0; // canary, just past the driver's hard ceiling
        }
        let inflated = cycle_info_with(0.0, true);
        assert_eq!(
            unsafe {
                senda_DoIOOperation(
                    std::ptr::null_mut(),
                    50,
                    52,
                    0,
                    IO_OP_READ_INPUT,
                    u32::MAX, // a corrupt/hostile HAL frame count
                    &inflated,
                    dst.as_mut_ptr().cast::<c_void>(),
                    std::ptr::null_mut(),
                )
            },
            OK
        );
        assert!(
            dst.iter().skip(max_len).all(|&x| x == -1.0),
            "a corrupt/hostile HAL frame count must not be trusted past this driver's own hard \
             ceiling (MAX_BUFFER_FRAMES)"
        );
        // The clamp must also be counted, not just silently applied. Device
        // index 4 is exclusive to this test (see the section note), so this
        // reads back exactly the one clamp event the call above produced.
        assert_eq!(
            device::STATE.get(4).map(device::PerDevice::frame_clamps),
            Some(1),
            "a corrupt/hostile HAL frame count must be recorded, not just silently clamped"
        );

        // A client-settable negotiated buffer size must never bound the RT
        // frame clamp. Were `n` clamped to `buffer_frames()` instead of
        // `MAX_BUFFER_FRAMES`, `round_trip` would come back with only its
        // first 64 frames (`3.0`) written and the remaining 448 left at
        // `POISON`: client B narrows `'fsiz'` to 64 while the HAL keeps
        // running BLOCK-frame (512) cycles for client A, so 448 of every
        // 512 frames would never reach the ring at all, with no `Ring`
        // counter able to see it.
        let fsiz_addr = AudioObjectPropertyAddress {
            selector: fourcc(b"fsiz"),
            scope: 0,
            element: 0,
        };
        let narrowed: u32 = 64;
        let narrowed_bytes = narrowed.to_ne_bytes();
        assert_eq!(
            unsafe {
                senda_SetPropertyData(
                    std::ptr::null_mut(),
                    50,
                    0,
                    &fsiz_addr,
                    0,
                    std::ptr::null(),
                    narrowed_bytes.len() as u32,
                    narrowed_bytes.as_ptr().cast::<c_void>(),
                )
            },
            OK
        );
        assert_eq!(
            device::STATE.get(4).map(device::PerDevice::buffer_frames),
            Some(64),
            "setup invalid: 'fsiz' must have actually narrowed this device's negotiated size"
        );

        const POISON: f32 = -555.0;
        // A sample time far from every other offset this test already
        // wrote to (0 and `block`), so this round trip cannot alias an
        // earlier write.
        let far_block = 10 * block;
        let mut src = vec![3.0f32; device::BLOCK * channels];
        let write_info = cycle_info_with(far_block as f64, false);
        assert_eq!(
            unsafe {
                senda_DoIOOperation(
                    std::ptr::null_mut(),
                    50,
                    51,
                    0,
                    IO_OP_WRITE_MIX,
                    device::BLOCK as u32, // n = BLOCK (512), far above the negotiated 64
                    &write_info,
                    src.as_mut_ptr().cast::<c_void>(),
                    std::ptr::null_mut(),
                )
            },
            OK
        );
        let mut round_trip = vec![POISON; device::BLOCK * channels];
        let read_info = cycle_info_with(far_block as f64, true);
        assert_eq!(
            unsafe {
                senda_DoIOOperation(
                    std::ptr::null_mut(),
                    50,
                    52,
                    0,
                    IO_OP_READ_INPUT,
                    device::BLOCK as u32,
                    &read_info,
                    round_trip.as_mut_ptr().cast::<c_void>(),
                    std::ptr::null_mut(),
                )
            },
            OK
        );
        assert!(
            round_trip.iter().all(|&x| x == 3.0),
            "every one of the BLOCK frames the HAL actually handed over this cycle must round \
             trip through the ring, even though this device's client-settable 'fsiz' negotiated \
             size (64) is far smaller than n (BLOCK) — the RT clamp must bound `n` against this \
             driver's own hard ceiling (MAX_BUFFER_FRAMES), never against a client-settable \
             per-device property"
        );
    }

    // Exclusive to device id 10 (index 0, 2ch) — see this section's note
    // above. Reproduces, through the real `senda_DoIOOperation` FFI
    // boundary (raw pointers, HAL-shaped `AudioServerPlugInIOCycleInfo`
    // values) rather than `Ring`'s own methods, the loopback scenario the
    // ring exists for: one writer and two readers sharing a device.
    //
    // Each `WriteMix` cycle fills its buffer with `due_block as f32 + 1.0`
    // — the sample time it is writing at, offset by one so block 0's
    // marker never collides with `0.0`, the "nothing delivered here"
    // sentinel (see `cleanly_delivered` below). Each `ReadInput` cycle
    // checks that whenever it receives a fully, cleanly delivered block
    // (every sample identical and nonzero — a partially covered or faded
    // block never looks like this; see `engine::ring`'s coverage and
    // fading docs), that value equals the requested sample time, not
    // merely "some audio". A constant-content buffer could not tell
    // correct `cycle_info` indexing apart from every cycle aliasing onto
    // one fixed slot; this design does.
    //
    // The reader loop is bounded by wall-clock time, not a fixed iteration
    // count: cheap 2-channel calls can finish well inside the `lag_blocks`
    // warm-up window in an optimised build, leaving every request clamped
    // to sample time 0. 450ms clears the 20-block (~213ms) lag in both
    // debug and release profiles. 48kHz pacing, 20-block reader lag and
    // 20ms warm-up are the values already proven non-flaky by
    // `engine::ring::tests::concurrent_writer_and_reader_never_produce_a_silent_gap`.
    #[test]
    fn do_io_operation_lets_a_writer_and_two_readers_run_concurrently() {
        assert_eq!(unsafe { senda_StartIO(std::ptr::null_mut(), 10, 0) }, OK);
        let channels = 2usize;

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let epoch = std::time::Instant::now();
        const RATE: f64 = 48_000.0;
        let block = device::BLOCK as u64;

        let stop_w = std::sync::Arc::clone(&stop);
        let writer = std::thread::spawn(move || {
            let mut buf = vec![0.0f32; device::BLOCK * channels];
            let mut last_written: Option<u64> = None;
            while !stop_w.load(Ordering::Relaxed) {
                let due_block = (epoch.elapsed().as_secs_f64() * RATE) as u64 / block * block;
                if last_written != Some(due_block) {
                    // The marker: exactly the sample time this cycle
                    // claims to be writing at (+1, see above), so a
                    // reader can check it got back what it actually
                    // asked for, not merely "some real audio".
                    buf.fill(due_block as f32 + 1.0);
                    let info = cycle_info_with(due_block as f64, false);
                    unsafe {
                        senda_DoIOOperation(
                            std::ptr::null_mut(),
                            10,
                            11,
                            0,
                            IO_OP_WRITE_MIX,
                            device::BLOCK as u32,
                            &info,
                            buf.as_mut_ptr().cast::<c_void>(),
                            std::ptr::null_mut(),
                        )
                    };
                    last_written = Some(due_block);
                } else {
                    std::hint::spin_loop();
                }
            }
        });

        std::thread::sleep(std::time::Duration::from_millis(20));

        let lag_blocks = 20u64;
        let run_for = std::time::Duration::from_millis(450);
        let mut readers = Vec::new();
        for _ in 0..2 {
            readers.push(std::thread::spawn(move || {
                // A sentinel outside the fade's reachable range. Every real
                // value in this test — a marker (`due_block as f32 + 1.0`,
                // always >= 1.0) or anything `fade_span` decays it towards
                // (always in `[0, marker]`, per `engine::ring`'s fading
                // docs) — is non-negative, so a negative sentinel can never
                // be confused with delivered content, unlike a positive
                // value real content could pass through while decaying.
                const SENTINEL: f32 = -999.0;
                let mut dst = vec![SENTINEL; device::BLOCK * channels];
                let mut delivered_correct = 0u64;
                let mut wrong_value_delivered = 0u64;
                let mut sentinel_leaks = 0u64;
                // Safety cap on iterations alongside the time bound, so a
                // pathologically fast machine cannot spin far past what
                // this test actually needs.
                let mut iterations = 0u64;
                while epoch.elapsed() < run_for && iterations < 500_000 {
                    iterations += 1;
                    let elapsed_blocks = (epoch.elapsed().as_secs_f64() * RATE) as u64 / block;
                    let t = elapsed_blocks
                        .saturating_sub(lag_blocks)
                        .saturating_mul(block);
                    dst.fill(SENTINEL);
                    let info = cycle_info_with(t as f64, true);
                    unsafe {
                        senda_DoIOOperation(
                            std::ptr::null_mut(),
                            10,
                            12,
                            0,
                            IO_OP_READ_INPUT,
                            device::BLOCK as u32,
                            &info,
                            dst.as_mut_ptr().cast::<c_void>(),
                            std::ptr::null_mut(),
                        )
                    };
                    if dst.contains(&SENTINEL) {
                        sentinel_leaks += 1;
                    }
                    let first = dst.first().copied().unwrap_or(0.0);
                    let cleanly_delivered = first != 0.0 && dst.iter().all(|&x| x == first);
                    if cleanly_delivered {
                        if (first - (t as f32 + 1.0)).abs() < 0.5 {
                            delivered_correct += 1;
                        } else {
                            wrong_value_delivered += 1;
                        }
                    }
                }
                (delivered_correct, wrong_value_delivered, sentinel_leaks)
            }));
        }

        let mut results = Vec::new();
        for r in readers {
            results.push(r.join().unwrap_or((0, u64::MAX, u64::MAX)));
        }
        stop.store(true, Ordering::Relaxed);
        let _ = writer.join();

        for (delivered_correct, wrong_value_delivered, sentinel_leaks) in results {
            assert_eq!(
                sentinel_leaks, 0,
                "an untouched sentinel leaked through senda_DoIOOperation — a delivered/faded \
                 block must always be fully written"
            );
            assert_eq!(
                wrong_value_delivered, 0,
                "a cleanly delivered block carried a DIFFERENT cycle's marker than the sample \
                 time this read actually requested — the HAL-provided sample time is not being \
                 used to index the ring"
            );
            assert!(
                delivered_correct > 0,
                "a reader never observed a correctly-indexed block — writer and reader never \
                 overlapped, or sample-time indexing is not wired up at all"
            );
        }
    }
}
