//! Safe Rust only: no `unsafe`, no `Mutex`, no allocation on the IO path.
//! `forbid(unsafe_code)` makes that a compile error; `src/ffi/` converts raw
//! pointers to safe values before calling in.
//! The clippy denies close the panic routes reachable from `extern "C"`, where
//! an unwind aborts `coreaudiod`. Use `saturating_*`/`checked_*` for arithmetic.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

pub mod clock;
pub mod device;
pub mod io;
pub mod properties;
pub mod ring;
