//! The 22-entry COM-style `AudioServerPlugInDriverInterface` vtable.
//! `QueryInterface` echoes the driver handle and `AddRef`/`Release` are
//! constant; devices are static, so the lifecycle entries are no-ops.
//! Property entries marshal raw HAL pointers to and from `engine::properties`.
//! `StartIO`/`StopIO` ref-count clients per device; the first client's
//! `StartIO` anchors the clock and wipes the ring. `DoIOOperation` hands an
//! `f32` slice to `engine::io`, indexed by the HAL's sample time for the cycle.
//! Nothing on the IO path allocates, locks or can panic.

use super::types::*;
use crate::engine::clock::CLOCKS;
use crate::engine::device;
use crate::engine::io;
use crate::engine::properties::{self, Value};
use std::ffi::{c_char, c_void};
use std::sync::atomic::Ordering;

pub const OK: OSStatus = 0;
/// `kAudioHardwareUnknownPropertyError`: the selector isn't one this object kind answers.
pub const ERR_UNKNOWN_PROPERTY: OSStatus = fourcc(b"who?") as OSStatus;
/// `kAudioHardwareUnsupportedOperationError`: a real property, but not this operation on it.
pub const ERR_UNSUPPORTED: OSStatus = fourcc(b"unop") as OSStatus;
/// `kAudioHardwareBadObjectError`: the `AudioObjectID` names nothing this driver owns.
pub const ERR_BAD_OBJECT: OSStatus = fourcc(b"!obj") as OSStatus;
/// `kAudioHardwareBadPropertySizeError`: the host's buffer is smaller than
/// the property needs; returned rather than truncating (see `write_value`).
pub const ERR_BAD_PROPERTY_SIZE: OSStatus = fourcc(b"!siz") as OSStatus;
/// `kAudioHardwareUnspecifiedError`: CoreFoundation failed to create an object.
pub const ERR_UNSPECIFIED: OSStatus = fourcc(b"what") as OSStatus;

/// `kAudioServerPlugInIOOperationWriteMix`. With any value but Apple's
/// `'rite'` the device would start and keep time but pass no audio.
const IO_OP_WRITE_MIX: u32 = fourcc(b"rite");
/// `kAudioServerPlugInIOOperationReadInput`.
const IO_OP_READ_INPUT: u32 = fourcc(b"read");

// REFIID is a 16-byte by-value aggregate with the same ABI on x86_64 and
// arm64, so `[u8; 16]` by value is FFI-safe here despite the lint.
#[allow(improper_ctypes_definitions)]
/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_QueryInterface(
    s: *mut c_void,
    _uuid: [u8; 16],
    out: *mut *mut c_void,
) -> i32 {
    // Echo `s`, the `Interface**` the host holds. `driver_interface_ptr()` is
    // an `Interface*`, one level short: the host would read `_reserved` (null)
    // as the vtable.
    // SAFETY: `out` is null-checked below; per the HAL contract it is then a
    // valid, writable, pointer-aligned `*mut c_void` for this call.
    if !out.is_null() {
        unsafe { *out = s };
    }
    0
}
/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_AddRef(_s: *mut c_void) -> u32 {
    1
}
/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_Release(_s: *mut c_void) -> u32 {
    1
}

/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_Initialize(
    _d: AudioServerPlugInDriverRef,
    _host: *const c_void,
) -> OSStatus {
    OK
}

/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_CreateDevice(
    _d: AudioServerPlugInDriverRef,
    _desc: CFDictionaryRef,
    _c: *const AudioServerPlugInClientInfo,
    _out: *mut AudioObjectID,
) -> OSStatus {
    ERR_UNSUPPORTED
}
/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_DestroyDevice(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
) -> OSStatus {
    ERR_UNSUPPORTED
}
/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_AddDeviceClient(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _c: *const AudioServerPlugInClientInfo,
) -> OSStatus {
    OK
}
/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_RemoveDeviceClient(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _c: *const AudioServerPlugInClientInfo,
) -> OSStatus {
    OK
}
/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_PerformDeviceConfigurationChange(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _a: u64,
    _i: *mut c_void,
) -> OSStatus {
    OK
}
/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_AbortDeviceConfigurationChange(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _a: u64,
    _i: *mut c_void,
) -> OSStatus {
    OK
}

/// Copies a `*const AudioObjectPropertyAddress` into an owned value; `None` if null.
///
/// # Safety contract
/// Non-null `a` is trusted, per the HAL contract, to be a valid `AudioObjectPropertyAddress` for this call.
fn read_property_address(
    a: *const AudioObjectPropertyAddress,
) -> Option<AudioObjectPropertyAddress> {
    if a.is_null() {
        return None;
    }
    // SAFETY: `a` is non-null and, per the HAL contract, points to a valid,
    // initialised, 4-byte-aligned `AudioObjectPropertyAddress` for this call;
    // the type is `Copy` and `repr(C)`, so this is a plain read.
    Some(unsafe { *a })
}

/// Decodes a `CFStringRef` qualifier into a `&str` borrowed from `scratch`;
/// `None` if it is absent, the wrong size or undecodable. Never panics.
fn read_qualifier_str(
    qualifier_size: u32,
    qualifier_data: *const c_void,
    scratch: &mut [u8; 256],
) -> Option<&str> {
    if qualifier_data.is_null() || qualifier_size as usize != std::mem::size_of::<CFStringRef>() {
        return None;
    }
    // SAFETY: `qualifier_data` is non-null and exactly one `CFStringRef` in
    // size (checked above); the HAL allocates it as a `CFStringRef`, so it
    // is pointer-aligned. Read only.
    let cf: CFStringRef = unsafe { *(qualifier_data as *const CFStringRef) };
    if cf.is_null() {
        return None;
    }
    // SAFETY: `cf` is a non-null `CFStringRef` the host owns for this call;
    // `scratch` is a valid, writable local buffer of the length passed.
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

/// Nanoseconds per `mach_absolute_time` tick. Not on the IO path: called
/// from `senda_StartIO` and on a sample-rate change only.
fn host_ns_per_tick() -> f64 {
    let mut info = MachTimebaseInfo::default();
    // SAFETY: `&mut info` is a valid, uniquely-owned `MachTimebaseInfo` for this call.
    let kr = unsafe { mach_timebase_info(&mut info) };
    // On failure, or a `denom` that would divide by zero, 1 tick == 1 ns.
    if kr != 0 || info.denom == 0 {
        return 1.0;
    }
    f64::from(info.numer) / f64::from(info.denom)
}

/// A fresh `CFStringRef` per call: the HAL takes ownership of the result.
fn cfstring_from_str(s: &str) -> CFStringRef {
    let cstr = std::ffi::CString::new(s).unwrap_or_default();
    // SAFETY: `cstr` is a valid NUL-terminated C string for this call; a
    // `NULL` allocator means the default allocator.
    unsafe { CFStringCreateWithCString(std::ptr::null(), cstr.as_ptr(), K_CF_STRING_ENCODING_UTF8) }
}

/// Must match `CFBundleIdentifier` in `Info.plist`; kept in sync by hand.
const DRIVER_BUNDLE_ID: &str = "com.senda.link.driver";

/// A Copy-rule `CFURLRef` to `DeviceIcon.icns` in this driver's bundle, for
/// `kAudioDevicePropertyIcon`; `None` if the bundle is not registered (as in
/// `cargo test`) or the resource is missing. Ownership passes to the caller.
fn icon_resource_url() -> Option<CFURLRef> {
    let bundle_id = cfstring_from_str(DRIVER_BUNDLE_ID);
    if bundle_id.is_null() {
        return None;
    }
    // SAFETY: `bundle_id` is a non-null `CFStringRef` this function owns.
    let bundle: CFBundleRef = unsafe { CFBundleGetBundleWithIdentifier(bundle_id) };
    // SAFETY: `bundle_id` is non-null and Create-rule owned here; the Get
    // call above did not take ownership.
    unsafe { CFRelease(bundle_id) };
    // `bundle` is Get-rule: never released.
    if bundle.is_null() {
        return None;
    }

    let name = cfstring_from_str("DeviceIcon");
    let ext = cfstring_from_str("icns");
    let url = if name.is_null() || ext.is_null() {
        None
    } else {
        // SAFETY: `bundle` is a non-null Get-rule `CFBundleRef`; `name` and
        // `ext` are non-null `CFStringRef`s owned here until released below.
        let url = unsafe { CFBundleCopyResourceURL(bundle, name, ext, std::ptr::null()) };
        if url.is_null() {
            None
        } else {
            Some(url)
        }
    };
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

/// Copies at most `size_of::<T>()` of `n` bytes from `*src` into `out`.
///
/// # Safety
/// `out` must be non-null with at least `n` writable bytes.
unsafe fn copy_out<T>(src: &T, out: *mut c_void, n: usize) {
    let n = n.min(std::mem::size_of::<T>());
    std::ptr::copy_nonoverlapping(src as *const T as *const u8, out.cast::<u8>(), n);
}

/// Serialises `value` into the host's buffer and returns the bytes written.
/// An undersized buffer is rejected, not truncated into: a partial
/// `CFStringRef`/`CFURLRef` would leave `coreaudiod` releasing garbage, and
/// the check runs before those arms create anything, so nothing can leak.
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
    // SAFETY: `out` is non-null and `needed <= out_size` (checked above);
    // each arm copies exactly `size_of::<T>()` bytes from a local,
    // initialised value of that type, so every `copy_out` stays within both.
    unsafe {
        match value {
            Value::U32(v) => copy_out(&v, out, needed),
            Value::F64(v) => copy_out(&v, out, needed),
            Value::Str(s) => {
                let cf = cfstring_from_str(s);
                // A null `CFStringRef` handed to the host would be `CFRelease`d.
                if cf.is_null() {
                    return Err(ERR_UNSPECIFIED);
                }
                copy_out(&cf, out, needed);
            }
            Value::Url => {
                let Some(url) = icon_resource_url() else {
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

/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
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
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
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
            // SAFETY: `out` is non-null; per the HAL contract a valid, writable `u8` for this call.
            unsafe { *out = u8::from(settable) };
            OK
        }
        Err(status) => status,
    }
}

/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
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
    // Only `TranslateUIDToDevice` takes a qualifier; any other selector's
    // must not be handed to `CFStringGetCString`.
    let mut scratch = [0u8; 256];
    let qualifier = if addr.selector == fourcc(b"uidd") {
        read_qualifier_str(qs, q, &mut scratch)
    } else {
        None
    };
    match properties::get_property_data_size(id, &addr, qualifier) {
        Ok(size) => {
            // SAFETY: `out` is non-null; per the HAL contract a valid, writable `u32` for this call.
            unsafe { *out = size as u32 };
            OK
        }
        Err(status) => status,
    }
}

/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
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
    let mut scratch = [0u8; 256];
    let qualifier = if addr.selector == fourcc(b"uidd") {
        read_qualifier_str(qs, q, &mut scratch)
    } else {
        None
    };
    match properties::get_property_data(id, &addr, qualifier) {
        Ok(value) => match write_value(value, data_size, out) {
            Ok(written) => {
                // SAFETY: `out_size` is non-null; per the HAL contract a valid, writable `u32` for this call.
                unsafe { *out_size = written };
                OK
            }
            Err(status) => status,
        },
        Err(status) => status,
    }
}

/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
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
    // SAFETY: `data` is non-null and, per the HAL contract, `data_size` is the
    // size of the allocation behind it, so `len <= data_size` bytes lie within
    // it. The 8-byte cap bounds the copy; it is no defence against a wrong `data_size`.
    let len = (data_size as usize).min(8);
    let slice: &[u8] = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) };
    let result = properties::set_property_data(id, &addr, slice);
    // A successful NominalSampleRate change voids the zero-timestamp
    // timeline; the validated rate is read back from `STATE`.
    if result.is_ok() && addr.selector == fourcc(b"nsrt") {
        if let Some(dev) = device::find_by_object_id(id) {
            if let (Some(clock), Some(state)) = (CLOCKS.get(dev), device::STATE.get(dev)) {
                // Skipped while IO runs: `Ring::zero` would be a second writer,
                // and the next 0->1 `StartIO` zeroes anyway. Not atomic with
                // respect to `StartIO`; `START_STOP_LOCKS` is not held here.
                if !state.io_running.load(Ordering::Acquire) {
                    if let Some(ring) = io::rings_for(dev) {
                        ring.zero();
                    }
                }
                // SAFETY: reads the monotonic tick counter; touches no memory.
                let now = unsafe { mach_absolute_time() };
                clock.start(now, state.rate(), host_ns_per_tick());
            }
        }
    }
    match result {
        Ok(()) => OK,
        Err(status) => status,
    }
}

/// Serialises the `StartIO`/`StopIO` ref-count transition, one lock per
/// device: a bare `fetch_add` would let a second client return `OK` before
/// the winner's ring and clock setup completed. NullAudio holds a mutex
/// across the same transition. Never touched on the IO path.
static START_STOP_LOCKS: [std::sync::Mutex<()>; 5] = [
    std::sync::Mutex::new(()),
    std::sync::Mutex::new(()),
    std::sync::Mutex::new(()),
    std::sync::Mutex::new(()),
    std::sync::Mutex::new(()),
];

/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_StartIO(
    _d: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _c: u32,
) -> OSStatus {
    // `StartIO` arrives once per client; only the 0->1 transition (re)anchors
    // the clock and zeroes the ring. An unknown `id` is a no-op, not an error.
    if let Some(dev) = device::find_by_object_id(id) {
        if let Some(state) = device::STATE.get(dev) {
            if let Some(lock) = START_STOP_LOCKS.get(dev) {
                // Held across the whole transition. A poisoned lock is
                // recovered, never propagated: no panic may cross `extern "C"`.
                let _guard = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let prev = state.client_count.fetch_add(1, Ordering::AcqRel);
                if prev == 0 {
                    // A reused `Ring` must not carry the previous session's audio.
                    if let Some(ring) = io::ensure_ring(dev) {
                        ring.zero();
                    }
                    if let Some(clock) = CLOCKS.get(dev) {
                        // SAFETY: reads the monotonic tick counter; touches no memory.
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
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_StopIO(
    _d: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _c: u32,
) -> OSStatus {
    // Only the last client (1->0) clears `io_running`. `checked_sub` declines
    // an unmatched `StopIO` rather than wrapping; same lock as `StartIO`.
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
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
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
    // SAFETY: reads the monotonic tick counter; touches no memory.
    let now = unsafe { mach_absolute_time() };
    let (sample_time, host_time, seed_val) = clock.zero_timestamp(now);
    if !st.is_null() {
        // SAFETY: per the HAL out-param contract `st`, `ht` and `seed` are
        // valid, writable, 8-byte-aligned `Float64`/`UInt64` for this call.
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
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
unsafe extern "C" fn senda_WillDoIOOperation(
    _d: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _c: u32,
    op: u32,
    will: *mut u8,
    will_do_in_place: *mut u8,
) -> OSStatus {
    // Only the two operations `senda_DoIOOperation` implements are declared.
    // The second out-param is `outWillDoInPlace`, not a direction flag; only
    // `main` is ever touched, so it is true for both, as in NullAudio and BlackHole.
    let declared = op == IO_OP_WRITE_MIX || op == IO_OP_READ_INPUT;
    let (will_do, in_place) = if declared { (1u8, 1u8) } else { (0u8, 0u8) };
    if !will.is_null() {
        // SAFETY: per the HAL out-param contract, a valid, writable `u8` for this call.
        unsafe { *will = will_do };
    }
    if !will_do_in_place.is_null() {
        // SAFETY: same contract as `will` above.
        unsafe { *will_do_in_place = in_place };
    }
    OK
}
/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
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
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
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
    // Without `cycle_info`'s sample time there is nothing to index the ring by.
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

    // SAFETY: `cycle_info` is non-null and, per the HAL contract, points to a
    // valid, initialised, 8-byte-aligned `AudioServerPlugInIOCycleInfo` for
    // this call; the type is `Copy`, so this is a plain read.
    let info = unsafe { *cycle_info };

    // `n` is clamped to `MAX_BUFFER_FRAMES` (the `'fsz#'` ceiling), bounding
    // the slice below whatever `n` claims. Not to this device's `'fsiz'`: `n`
    // is the maximum across clients, so one narrowing must not truncate the rest.
    if (n as usize) > device::MAX_BUFFER_FRAMES as usize {
        // A silent clamp is invisible in practice, so it is counted.
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
        // `f64 as u64` saturates; no explicit clamp needed.
        let sample_time = info.output_time.sample_time as u64;
        // SAFETY: `main` is non-null; per the HAL's `DoIOOperation` contract
        // it holds at least `n * channels` valid, `f32`-aligned samples, and
        // `frames <= n` after the clamp, so `len` never exceeds that.
        let src = unsafe { std::slice::from_raw_parts(main.cast::<f32>(), len) };
        // No sanitising or gain: `Ring::write` preserves every bit pattern.
        io::on_write_mix(dev, sample_time, src, frames);
    } else if op == IO_OP_READ_INPUT {
        let sample_time = info.input_time.sample_time as u64;
        // SAFETY: as the `WriteMix` arm, but written; the HAL does not touch
        // the buffer while `DoIOOperation` runs, so the slice is exclusive.
        let dst = unsafe { std::slice::from_raw_parts_mut(main.cast::<f32>(), len) };
        io::on_read_input(dev, sample_time, dst, frames);
    }
    // Any other operation was never declared in `WillDoIOOperation`; decline silently.
    OK
}
/// # Safety
/// Called by the HAL through the vtable; every pointer is valid per the AudioServerPlugIn contract.
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

/// A `static` raw pointer would need a `Sync` wrapper; a function is equivalent.
pub fn driver_interface_ptr() -> *mut AudioServerPlugInDriverInterface {
    &raw mut DRIVER_INTERFACE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_constants_match_the_documented_decimal_values() {
        assert_eq!(ERR_BAD_PROPERTY_SIZE, 561211770);
        assert_eq!(ERR_UNKNOWN_PROPERTY, 2003332927);
        assert_eq!(ERR_UNSUPPORTED, 1970171760);
        assert_eq!(ERR_BAD_OBJECT, 560947818);
    }

    #[test]
    fn write_value_rejects_undersized_out_size_and_writes_exact_bytes_when_sufficient() {
        const POISON: u8 = 0xAA;
        let mut buf = [POISON; 32];

        let out = buf.as_mut_ptr().cast::<c_void>();
        assert_eq!(
            write_value(Value::U32(0x1122_3344), 0, out),
            Err(ERR_BAD_PROPERTY_SIZE)
        );
        assert_eq!(buf, [POISON; 32]);

        let out = buf.as_mut_ptr().cast::<c_void>();
        assert_eq!(write_value(Value::U32(0x1122_3344), 4, out), Ok(4));
        assert_eq!(&buf[..4], 0x1122_3344_u32.to_ne_bytes().as_slice());
        assert_eq!(&buf[4..], [POISON; 28].as_slice());

        buf = [POISON; 32];
        let out = buf.as_mut_ptr().cast::<c_void>();
        assert_eq!(write_value(Value::U32(0x5566_7788), 8, out), Ok(4));
        assert_eq!(&buf[..4], 0x5566_7788_u32.to_ne_bytes().as_slice());
        assert_eq!(&buf[4..], [POISON; 28].as_slice());

        // out_size = 64 overstates the real 32-byte buffer; only `needed` bytes are copied.
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

    #[test]
    fn write_value_rejects_undersized_buffer_for_icon_url_before_creating_anything() {
        const POISON: u8 = 0xAA;
        let mut buf = [POISON; 8];
        let out = buf.as_mut_ptr().cast::<c_void>();
        assert_eq!(write_value(Value::Url, 4, out), Err(ERR_BAD_PROPERTY_SIZE));
        assert_eq!(buf, [POISON; 8]);
    }

    // No bundle is registered under `DRIVER_BUNDLE_ID` in the `cargo test` process.
    #[test]
    fn write_value_of_icon_url_is_unspecified_when_the_driver_bundle_is_not_registered() {
        assert!(icon_resource_url().is_none());
        let mut buf = [0u8; 8];
        let out = buf.as_mut_ptr().cast::<c_void>();
        assert_eq!(write_value(Value::Url, 8, out), Err(ERR_UNSPECIFIED));
    }

    #[test]
    fn get_property_data_size_ignores_qualifier_for_non_uidd_selectors() {
        let bogus: u64 = 0xDEAD_BEEF_DEAD_BEEF;
        let addr = AudioObjectPropertyAddress {
            selector: fourcc(b"dev#"),
            scope: 0,
            element: 0,
        };
        let mut out: u32 = 0;
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

        let running = AudioObjectPropertyAddress {
            selector: fourcc(b"goin"),
            scope: 0,
            element: 0,
        };
        assert!(
            matches!(properties::device_property(2, &running), Ok(Value::U32(1))),
            "io_running must be set true after StartIO's 0->1 transition"
        );

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

        // An unmatched StopIO must not wrap `client_count`; a wrapped count would stop the next StartIO re-arming.
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

    // Uses device id 20 (index 1); the rejected rate never reaches its clock.
    #[test]
    fn set_property_data_rejects_unsupported_rate_without_touching_the_clock() {
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

    // A test that starts IO on device index N is the only test in the crate to do so.

    /// One populated timestamp: `output_time` for `WriteMix`, `input_time` for `ReadInput`.
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

    // Exclusive to device id 50 (index 4, 128ch).
    #[test]
    fn do_io_operation_indexes_by_hal_sample_time_and_bounds_untrusted_frame_counts() {
        assert_eq!(unsafe { senda_StartIO(std::ptr::null_mut(), 50, 0) }, OK);
        let channels = 128usize;

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
        assert_eq!(
            device::STATE.get(4).map(device::PerDevice::frame_clamps),
            Some(1),
            "a corrupt/hostile HAL frame count must be recorded, not just silently clamped"
        );

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

    // Exclusive to device id 10 (index 0, 2ch). Each write's marker is its
    // sample time + 1, so a cleanly delivered block proves which cycle it came
    // from; 450ms clears the 20-block reader lag in debug and release builds.
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
                // Negative, since every delivered or faded value is >= 0.
                const SENTINEL: f32 = -999.0;
                let mut dst = vec![SENTINEL; device::BLOCK * channels];
                let mut delivered_correct = 0u64;
                let mut wrong_value_delivered = 0u64;
                let mut sentinel_leaks = 0u64;
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
