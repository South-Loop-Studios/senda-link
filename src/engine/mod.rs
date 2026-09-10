//! Safe Rust only. No `unsafe`, no `Mutex`, no allocation on the IO path.
//!
//! `#![forbid(unsafe_code)]` makes the "no unsafe in engine/" invariant a
//! compile error rather than a review-time hope: pointer handling belongs in
//! `src/ffi/`, which converts raw pointers to safe slices/values *before*
//! calling into this module, and turns this module's plain return values
//! back into raw writes *after*.
//!
//! The four clippy lints below close the actual panic routes reachable from
//! `extern "C"` — a panic unwinding across that boundary aborts `coreaudiod`
//! and kills all system audio. `arithmetic_side_effects` is deliberately NOT
//! denied — it rejects ordinary `+`, `%` and `/` and would force `#[allow]`
//! sprinkles, which is worse. Instead: use `saturating_*`, `checked_*`, or a
//! proven-nonzero divisor anywhere overflow or divide-by-zero is reachable.
//! `panic = "abort"` in the release profile makes any escape loud rather
//! than silent, and Rust 1.81 and later abort on an unwind out of
//! `extern "C"` whatever the profile, so debug and test builds are covered
//! as well.
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
