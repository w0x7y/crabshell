//! What the platform tells its listener, and calling the listener so that
//! a panic in it doesn't stop the shell.

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};

use crate::outputs::OutputInfo;
use crate::windows::WindowId;

/// Something the platform's listener hears about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// An output was announced. Windows can be built for it now.
    OutputAdded(OutputInfo),
    /// An output went away. Its windows were dropped first, with their
    /// owners.
    OutputRemoved(OutputInfo),
    /// The compositor closed a window, and it was dropped first, with its
    /// owner. Sway closes every layer surface on an output before it
    /// removes the output, so an unplug usually brings this, for each
    /// window there, before [`OutputRemoved`](Event::OutputRemoved).
    WindowClosed(WindowId),
}

/// A listener for a `T` that holds it.
pub(crate) type Listener<T> = Box<dyn FnMut(&mut T, &Event)>;

/// Calls the listener `slot` finds in `target`. It is taken out for the
/// call, so it can borrow all of `target`, and put back afterward unless
/// it set a new one. If it panics, the panic is logged and the listener
/// kept: one bad event shouldn't end the shell.
pub(crate) fn notify<T>(
    target: &mut T,
    slot: fn(&mut T) -> &mut Option<Listener<T>>,
    event: &Event,
) {
    let Some(mut listener) = slot(target).take() else {
        return;
    };
    if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| listener(target, event))) {
        tracing::error!(
            "the platform's listener panicked on {event:?}: {}",
            panic_message(payload.as_ref())
        );
    }
    let current = slot(target);
    if current.is_none() {
        *current = Some(listener);
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message
    } else {
        "(panic payload is not a string)"
    }
}
