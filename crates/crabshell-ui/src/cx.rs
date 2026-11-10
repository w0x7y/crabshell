//! [`Cx`], the context that widget code, event handlers and list views get.

use crabshell_reactive::{Effect, Memo, Owner, Signal};

/// The context widget code runs in.
///
/// A `Cx` wraps the reactive [`Owner`] of one part of the shell: a window,
/// a widget or one item of a keyed list. Signals, memos and effects created
/// through it belong to that owner, so they are freed when that part of the
/// shell goes away. Event handlers receive the `Cx` of their button, and
/// [`children_keyed`](crate::Row::children_keyed) views receive the `Cx` of
/// their item.
#[derive(Clone, Debug)]
pub struct Cx {
    owner: Owner,
}

impl Cx {
    /// A context that creates everything under `owner`.
    pub fn new(owner: Owner) -> Cx {
        Cx { owner }
    }

    /// The owner that signals, memos and effects created through this
    /// context belong to.
    pub fn owner(&self) -> Owner {
        self.owner
    }

    /// A context for a part of the shell inside this one: what it creates
    /// is freed when this context's owner is. Everything else a context
    /// carries is shared with the child.
    pub(crate) fn child(&self) -> Cx {
        Cx {
            owner: self.owner.child(),
        }
    }

    /// Gives the context's owner a debug name, such as a widget path.
    pub(crate) fn named(self, name: impl Into<String>) -> Cx {
        Cx {
            owner: self.owner.named(name),
        }
    }

    /// Creates a [`Signal`] holding `value`. See [`Owner::signal`].
    #[track_caller]
    pub fn signal<T: 'static>(&self, value: T) -> Signal<T> {
        self.owner.signal(value)
    }

    /// Creates a [`Memo`] that derives its value with `f`. See
    /// [`Owner::memo`].
    #[track_caller]
    pub fn memo<T: PartialEq + 'static>(&self, f: impl FnMut() -> T + 'static) -> Memo<T> {
        self.owner.memo(f)
    }

    /// Creates an [`Effect`] that reruns `f` when something it read changes.
    /// See [`Owner::effect`].
    #[track_caller]
    pub fn effect(&self, f: impl FnMut() + 'static) -> Effect {
        self.owner.effect(f)
    }
}
