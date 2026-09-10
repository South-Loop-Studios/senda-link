// For the Apple-cased vtable field names and the `SendaLinkCreate` symbol named in `Info.plist`.
#![allow(non_snake_case)]

pub mod engine;
pub mod ffi;

use ffi::plugin::driver_interface_ptr;
use std::ffi::c_void;

static mut INTERFACE_REF: *mut ffi::types::AudioServerPlugInDriverInterface = std::ptr::null_mut();

/// CFPlugIn factory. Named in Info.plist under CFPlugInFactories.
///
/// The HAL expects a pointer to a pointer to the interface (an
/// `Interface**`, i.e. `AudioServerPlugInDriverRef`), not the interface
/// itself: COM requires the double indirection, and the HAL dereferences
/// twice. That is why this returns the address of `INTERFACE_REF`
/// (`&raw mut INTERFACE_REF`) rather than `driver_interface_ptr()`
/// directly — the latter is only one indirection (`Interface*`), and
/// returning it here would corrupt every call the host makes through the
/// resulting handle.
///
/// # Safety
///
/// This is a CFPlugIn factory entry point invoked by `coreaudiod` (via
/// dlopen + symbol lookup driven by `Info.plist`'s `CFPlugInFactories`), not
/// by any Rust caller. The host guarantees it is called on a single thread
/// per instantiation and supplies well-formed allocator/type-UUID pointers
/// per the CFPlugIn contract; neither parameter is dereferenced here. The
/// write to `INTERFACE_REF` is to a process-global `static mut` — sound only
/// because the HAL does not call this factory concurrently with itself.
#[no_mangle]
pub unsafe extern "C" fn SendaLinkCreate(
    _allocator: *const c_void,
    _requested_type_uuid: *const c_void,
) -> *mut c_void {
    INTERFACE_REF = driver_interface_ptr();
    &raw mut INTERFACE_REF as *mut c_void
}
