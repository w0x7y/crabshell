//! Signals: values you set, and read-only views of them.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::handle::{Marker, handle_impls};
use crate::runtime::{self, NodeId};

/// A value you set. A memo or effect that reads it reruns after it changes.
///
/// Create one with [`Owner::signal`](crate::Owner::signal). The handle is a
/// small `Copy` key, so closures capture it by value. The value itself lives
/// in this thread's runtime until the signal's owner is disposed.
pub struct Signal<T> {
    id: NodeId,
    _marker: Marker<T>,
}

handle_impls!(Signal<T>);

impl<T: 'static> Signal<T> {
    #[track_caller]
    pub(crate) fn new(owner: NodeId, value: T) -> Self {
        let value: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(Some(value)));
        Signal {
            id: runtime::create_signal(owner, value),
            _marker: PhantomData,
        }
    }

    /// How many memos and effects read this on their last run.
    #[cfg(test)]
    pub(crate) fn subscriber_count(&self) -> usize {
        runtime::subscriber_count(self.id)
    }

    /// The slot index and version of this signal's key.
    #[cfg(test)]
    pub(crate) fn slot(&self) -> (u32, u32) {
        runtime::slot_of(self.id)
    }

    /// A read-only view of this signal, for code that should only read it.
    pub fn read_only(&self) -> ReadSignal<T> {
        ReadSignal {
            id: self.id,
            _marker: PhantomData,
        }
    }

    /// Returns a clone of the value. Inside a memo or effect, this makes it
    /// depend on the signal.
    ///
    /// # Panics
    ///
    /// If the signal's owner was disposed. [`try_get`](Self::try_get)
    /// returns `None` instead.
    #[track_caller]
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.with(T::clone)
    }

    /// Calls `f` with a reference to the value, without cloning it. Inside a
    /// memo or effect, this makes it depend on the signal.
    ///
    /// # Panics
    ///
    /// If the signal's owner was disposed, or if `f` writes this signal.
    #[track_caller]
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        runtime::read_or_panic(self.id, true, "Signal", f)
    }

    /// Like [`get`](Self::get), but records no dependency.
    #[track_caller]
    pub fn get_untracked(&self) -> T
    where
        T: Clone,
    {
        self.with_untracked(T::clone)
    }

    /// Like [`with`](Self::with), but records no dependency.
    #[track_caller]
    pub fn with_untracked<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        runtime::read_or_panic(self.id, false, "Signal", f)
    }

    /// Like [`get`](Self::get), but returns `None` if the owner was disposed.
    #[track_caller]
    pub fn try_get(&self) -> Option<T>
    where
        T: Clone,
    {
        self.try_with(T::clone)
    }

    /// Like [`with`](Self::with), but returns `None` if the owner was
    /// disposed.
    #[track_caller]
    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        runtime::read(self.id, true, "Signal", f).ok()
    }

    /// Stores a new value, marks every memo and effect that read this signal
    /// as stale, and queues the effects. Nothing runs until the next
    /// [`Runtime::flush`](crate::Runtime::flush). Dependents are notified
    /// even when the new value equals the old one.
    ///
    /// # Panics
    ///
    /// If the signal's owner was disposed, or if this runs inside a
    /// [`with`](Self::with) call on the same signal.
    #[track_caller]
    pub fn set(&self, value: T) {
        let old = runtime::write_or_panic(self.id, |slot: &mut Option<T>| slot.replace(value));
        // Dropped here, after every borrow has ended, in case `T`'s `Drop`
        // uses signals.
        drop(old);
    }

    /// Changes the value in place, then notifies like [`set`](Self::set).
    ///
    /// # Panics
    ///
    /// Like [`set`](Self::set), and also if `f` reads this signal.
    #[track_caller]
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        runtime::write_or_panic(self.id, |slot: &mut Option<T>| update_slot(slot, f));
    }

    /// Like [`set`](Self::set), but if the owner was disposed it does
    /// nothing and returns `false`.
    #[track_caller]
    pub fn try_set(&self, value: T) -> bool {
        match runtime::write(self.id, |slot: &mut Option<T>| slot.replace(value)) {
            Ok(old) => {
                drop(old);
                true
            }
            Err(_) => false,
        }
    }

    /// Like [`update`](Self::update), but if the owner was disposed it does
    /// nothing and returns `false`.
    #[track_caller]
    pub fn try_update(&self, f: impl FnOnce(&mut T)) -> bool {
        runtime::write(self.id, |slot: &mut Option<T>| update_slot(slot, f)).is_ok()
    }

    /// Gives the signal a debug name, such as a widget path.
    pub fn named(self, name: impl Into<String>) -> Self {
        runtime::set_name(self.id, name.into());
        self
    }
}

fn update_slot<T>(slot: &mut Option<T>, f: impl FnOnce(&mut T)) {
    f(slot
        .as_mut()
        .expect("invariant: a signal always holds a value"));
}

/// A read-only view of a [`Signal`], made with [`Signal::read_only`].
/// Services hand these out so only they can change the value.
pub struct ReadSignal<T> {
    id: NodeId,
    _marker: Marker<T>,
}

handle_impls!(ReadSignal<T>);

impl<T: 'static> ReadSignal<T> {
    /// See [`Signal::get`].
    #[track_caller]
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.with(T::clone)
    }

    /// See [`Signal::with`].
    #[track_caller]
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        runtime::read_or_panic(self.id, true, "ReadSignal", f)
    }

    /// See [`Signal::get_untracked`].
    #[track_caller]
    pub fn get_untracked(&self) -> T
    where
        T: Clone,
    {
        self.with_untracked(T::clone)
    }

    /// See [`Signal::with_untracked`].
    #[track_caller]
    pub fn with_untracked<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        runtime::read_or_panic(self.id, false, "ReadSignal", f)
    }

    /// See [`Signal::try_get`].
    #[track_caller]
    pub fn try_get(&self) -> Option<T>
    where
        T: Clone,
    {
        self.try_with(T::clone)
    }

    /// See [`Signal::try_with`].
    #[track_caller]
    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        runtime::read(self.id, true, "ReadSignal", f).ok()
    }
}
