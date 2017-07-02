//! Logging utility macros

use std::cell::Cell;
use std::ops::Deref;
use std::fmt::Debug;
use std::thread;

/// Evaluates the given expression once (when first reached).
macro_rules! once {
    ( $e:expr ) => {{
        use std::sync::atomic::{AtomicBool, Ordering, ATOMIC_BOOL_INIT};

        static REACHED: AtomicBool = ATOMIC_BOOL_INIT;
        if REACHED.swap(true, Ordering::SeqCst) == false {
            $e;
        }
    }}
}

/// Wraps a `Cell<T>` and writes its contents to stdout if dropped while panicking.
pub struct LogOnPanic<T: Copy + Debug> {
    name: &'static str,
    data: Cell<T>,
}

impl<T: Copy + Debug> LogOnPanic<T> {
    pub fn new(name: &'static str, t: T) -> Self {
        LogOnPanic {
            name: name,
            data: Cell::new(t),
        }
    }
}

impl<T: Copy + Debug> Deref for LogOnPanic<T> {
    type Target = Cell<T>;
    fn deref(&self) -> &Cell<T> { &self.data }
}

impl<T: Copy + Debug> Drop for LogOnPanic<T> {
    fn drop(&mut self) {
        if thread::panicking() {
            // NOTE `error!` is probably not safe to be used while the thread panics, but it should
            // be alright for now
            error!("[panic log] {}: {:?}", self.name, self.data.get())
        }
    }
}
