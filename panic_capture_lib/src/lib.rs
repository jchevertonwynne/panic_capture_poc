use std::sync::atomic::AtomicUsize;

pub use panic_capture_macro::capture_panics;

pub fn increment_counter() -> usize {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}
