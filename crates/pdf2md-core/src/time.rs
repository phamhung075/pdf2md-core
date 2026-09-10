//! Cross-platform monotonic clock.
//!
//! `std::time::Instant` panics on `wasm32-unknown-unknown` ("time not implemented
//! on this platform"), so the Wasm build measures durations with the JS clock via
//! js-sys/wasm-bindgen instead. The core only uses this for the `duration_us`
//! metric, so wall-clock precision is perfectly acceptable there.

/// Reads a monotonic microsecond timestamp.
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
fn now_us() -> u64 {
    // js_sys::Date::now() returns milliseconds since the UNIX epoch (f64).
    (js_sys::Date::now() * 1000.0) as u64
}

/// Native, true-monotonic fallback backed by `std::time::Instant`.
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
fn now_us() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static BASE: OnceLock<Instant> = OnceLock::new();
    let base = BASE.get_or_init(Instant::now);
    base.elapsed().as_micros() as u64
}

/// A capture point for measuring a duration in whole microseconds.
#[derive(Clone, Copy)]
pub(crate) struct MonoClock(u64);

impl MonoClock {
    pub(crate) fn now() -> Self {
        Self(now_us())
    }

    /// Whole microseconds elapsed since this capture point.
    pub(crate) fn elapsed_us(&self) -> u64 {
        now_us().saturating_sub(self.0)
    }
}
