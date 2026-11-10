//! Owners: who frees which nodes.

use std::marker::PhantomData;

use crate::handle::{NotSend, handle_impls};
use crate::runtime::{self, NodeId};
use crate::{Effect, Memo, Signal};

/// Owns every signal, memo, effect and child owner created through it, and
/// frees them all when it is disposed.
///
/// Each widget opens one, and dropping the widget disposes it.
/// `crabshell-ui`'s `Cx` wraps an `Owner`. Get a root owner from
/// [`Runtime::new_owner`](crate::Runtime::new_owner) and nested ones from
/// [`child`](Self::child).
pub struct Owner {
    id: NodeId,
    _not_send: NotSend,
}

handle_impls!(Owner);

impl Owner {
    #[track_caller]
    pub(crate) fn new(parent: Option<NodeId>) -> Self {
        Owner {
            id: runtime::create_owner(parent),
            _not_send: PhantomData,
        }
    }

    /// Creates an owner inside this one. Disposing this owner disposes the
    /// child too.
    ///
    /// # Panics
    ///
    /// This and the other create methods panic if this owner was disposed.
    #[track_caller]
    pub fn child(&self) -> Owner {
        Owner::new(Some(self.id))
    }

    /// Creates a [`Signal`] holding `value`.
    #[track_caller]
    pub fn signal<T: 'static>(&self, value: T) -> Signal<T> {
        Signal::new(self.id, value)
    }

    /// Creates a [`Memo`] that derives its value with `f`. `f` first runs
    /// when something reads the memo.
    #[track_caller]
    pub fn memo<T: PartialEq + 'static>(&self, f: impl FnMut() -> T + 'static) -> Memo<T> {
        Memo::new(self.id, f)
    }

    /// Creates an [`Effect`] that runs `f` in the next flush round (see
    /// [`Effect`]), and again whenever something `f` read changes.
    #[track_caller]
    pub fn effect(&self, f: impl FnMut() + 'static) -> Effect {
        Effect::new(self.id, f)
    }

    /// Frees this owner and everything created under it, child owners
    /// included. Their values and closures are dropped and their effects
    /// stop. Afterwards their handles' `get()` panics and `try_get()` returns
    /// `None`. Disposing an owner twice does nothing.
    pub fn dispose(self) {
        runtime::dispose(self.id);
    }

    /// Gives the owner a debug name, such as a widget path. Log lines about
    /// its unnamed nodes mention it.
    pub fn named(self, name: impl Into<String>) -> Self {
        runtime::set_name(self.id, name.into());
        self
    }
}
