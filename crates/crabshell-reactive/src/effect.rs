//! Effects: code that reruns when what it read changes.

use std::marker::PhantomData;

use crate::handle::{NotSend, handle_impls};
use crate::runtime::{self, Computed, NodeId};

/// Code that reruns when what it read changes. Every widget binding is one.
///
/// Create one with [`Owner::effect`](crate::Owner::effect) or
/// [`Owner::effect_scoped`](crate::Owner::effect_scoped). A new effect is
/// queued straight away. Created during a [`Runtime::flush`](crate::Runtime::flush),
/// for example by another effect, it runs later in that same flush; created
/// anywhere else, it runs at the next flush. After that it reruns whenever a
/// signal or memo it read on its last run changes. It stops for good when
/// its owner is disposed, when its closure panics, or when it is caught in a
/// dependency cycle. The last two are logged with its debug name.
pub struct Effect {
  id: NodeId,
  _not_send: NotSend,
}

handle_impls!(Effect);

impl Effect {
  #[track_caller]
  pub(crate) fn new(owner: NodeId, mut f: impl FnMut() + 'static) -> Self {
    let compute = move || {
      f();
      Computed {
        changed: true,
        leftover: None,
      }
    };
    Effect {
      id: runtime::create_effect(owner, Box::new(compute)),
      _not_send: PhantomData,
    }
  }

  /// Gives the effect a debug name, such as the widget path of the binding
  /// it implements. Panic and cycle log lines show it.
  pub fn named(self, name: impl Into<String>) -> Self {
    runtime::set_name(self.id, name.into());
    self
  }
}
