//! What every exit test needs: a panic hook that lets only the deliberate
//! panic through quietly, and drop counting.

use std::panic;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The message of the one panic these programs cause on purpose.
pub const DELIBERATE_PANIC: &str = "this Drop panics on purpose";

/// How many probe values have been dropped so far, on any thread.
static DROPS: AtomicUsize = AtomicUsize::new(0);

/// Installs a panic hook that stays silent for the deliberate panic and
/// hands every other one to the default hook, then aborts. The abort makes
/// an unexpected panic fail the test even where the runtime would catch and
/// only log it, as it does for a value's `Drop` at thread exit.
pub fn fail_on_unexpected_panics() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if info.payload_as_str() != Some(DELIBERATE_PANIC) {
            default_hook(info);
            std::process::abort();
        }
    }));
}

/// Aborts the process unless `ok`. Some checks run inside a `Drop` during
/// thread exit, where returning an error is not possible.
pub fn must_hold(ok: bool) {
    if !ok {
        std::process::abort();
    }
}

/// Records that a probe value was dropped.
pub fn count_drop() {
    DROPS.fetch_add(1, Ordering::SeqCst);
}

/// Aborts unless exactly `expected` probe values have been dropped.
pub fn expect_drops(expected: usize) {
    must_hold(DROPS.load(Ordering::SeqCst) == expected);
}

/// A probe value that panics when dropped. The runtime must catch and log
/// that, not abort the process.
pub struct PanicsOnDrop;

impl Drop for PanicsOnDrop {
    fn drop(&mut self) {
        count_drop();
        panic!("{DELIBERATE_PANIC}");
    }
}
