// For the Apple-cased vtable field names and the `SendaLinkCreate` symbol named in `Info.plist`.
#![allow(non_snake_case)]

pub mod engine;
pub mod ffi;

use ffi::plugin::driver_interface_ptr;
use std::ffi::c_void;

static mut INTERFACE_REF: *mut ffi::types::AudioServerPlugInDriverInterface = std::ptr::null_mut();

/// CFPlugIn factory, named in `Info.plist` under `CFPlugInFactories`. Returns
/// `&raw mut INTERFACE_REF`, an `Interface**`: COM requires the double indirection.
///
/// # Safety
/// Called only by `coreaudiod`, never concurrently; neither parameter is dereferenced.
#[no_mangle]
pub unsafe extern "C" fn SendaLinkCreate(
    _allocator: *const c_void,
    _requested_type_uuid: *const c_void,
) -> *mut c_void {
    INTERFACE_REF = driver_interface_ptr();
    &raw mut INTERFACE_REF as *mut c_void
}
