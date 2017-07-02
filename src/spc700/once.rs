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
