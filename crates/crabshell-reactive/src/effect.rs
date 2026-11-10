//! Effects: code that reruns when what it read changes.

use std::marker::PhantomData;

use crate::handle::{NotSend, handle_impls};
use crate::runtime::{self, NodeId};

/// Code that reruns when what it read changes. Every widget binding is one.
///
/// Create one with [`Owner::effect`](crate::Owner::effect). A new effect is
/// queued straight away. Created during a [`Runtime::flush`](crate::Runtime::flush),
/// for example by another effect, it runs later in that same flush; created
/// anywhere else, it runs at the next flush. After that it reruns whenever a
/// signal it read on its last run changes. It stops for good when
/// its owner is disposed, or when it is caught in a dependency cycle, which
/// is logged with its debug name.
pub struct Effect {
    id: NodeId,
    _not_send: NotSend,
}

handle_impls!(Effect);

impl Effect {
    #[track_caller]
    pub(crate) fn new(owner: NodeId, f: impl FnMut() + 'static) -> Self {
        Effect {
            id: runtime::create_effect(owner, Box::new(f)),
            _not_send: PhantomData,
        }
    }

    /// Gives the effect a debug name, such as the widget path of the binding
    /// it implements. Cycle log lines show it.
    pub fn named(self, name: impl Into<String>) -> Self {
        runtime::set_name(self.id, name.into());
        self
    }
}
