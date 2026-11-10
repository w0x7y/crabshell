//! Memos: derived values, recomputed only when read while stale.

use std::any::{Any, type_name};
use std::cell::RefCell;
use std::marker::PhantomData;
use std::panic::Location;
use std::rc::Rc;

use crate::handle::{Marker, handle_impls};
use crate::runtime::{self, Computed, NodeId, drop_isolated};

/// A value derived from signals and other memos.
///
/// Create one with [`Owner::memo`](crate::Owner::memo). It computes nothing
/// until something reads it. When a value it read changes, it only becomes
/// stale, and recomputes the next time something reads it. If the new value
/// equals the old one, the memos and effects that read it don't rerun.
///
/// If its closure panics, the memo is logged and disabled. It keeps its last
/// value, and `try_get()` returns `None` if it never had one. A memo must not
/// write signals or call [`Runtime::flush`](crate::Runtime::flush): either
/// panics, which disables it the same way.
pub struct Memo<T> {
    id: NodeId,
    _marker: Marker<T>,
}

handle_impls!(Memo<T>);

impl<T: PartialEq + 'static> Memo<T> {
    #[track_caller]
    pub(crate) fn new(owner: NodeId, mut f: impl FnMut() -> T + 'static) -> Self {
        let created = Location::caller();
        let value: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(None));
        let slot = Rc::clone(&value);
        let compute = move || {
            let new = f();
            let Ok(mut current) = slot.try_borrow_mut() else {
                drop_isolated(new);
                runtime::borrow_conflict(format!(
                    "Memo<{}> (created at {created}) was recomputed while a with() call on it \
                     was still running",
                    type_name::<T>()
                ));
            };
            // The value that isn't kept goes back to the runtime, which drops
            // it once the readers have been told.
            if current.as_ref() == Some(&new) {
                return Computed {
                    changed: false,
                    leftover: Some(Box::new(new)),
                };
            }
            let old = current.replace(new);
            Computed {
                changed: true,
                leftover: old.map(|old| Box::new(old) as Box<dyn Any>),
            }
        };
        Memo {
            id: runtime::create_memo(owner, value, Box::new(compute)),
            _marker: PhantomData,
        }
    }
}

impl<T: 'static> Memo<T> {
    /// How many memos and effects read this on their last run.
    #[cfg(test)]
    pub(crate) fn subscriber_count(&self) -> usize {
        runtime::subscriber_count(self.id)
    }

    /// Returns a clone of the value, recomputing it first if it is stale.
    /// Inside a memo or effect, this makes it depend on this memo.
    ///
    /// # Panics
    ///
    /// If the memo's owner was disposed, if it has no value because its
    /// closure panicked on the first run, or if the memo reads itself.
    /// [`try_get`](Self::try_get) returns `None` in the first two cases.
    #[track_caller]
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.with(T::clone)
    }

    /// Calls `f` with a reference to the value. Otherwise like
    /// [`get`](Self::get).
    #[track_caller]
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        runtime::read_or_panic(self.id, true, "Memo", f)
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
        runtime::read_or_panic(self.id, false, "Memo", f)
    }

    /// Like [`get`](Self::get), but returns `None` if the owner was disposed
    /// or the memo has no value.
    #[track_caller]
    pub fn try_get(&self) -> Option<T>
    where
        T: Clone,
    {
        self.try_with(T::clone)
    }

    /// Like [`with`](Self::with), but returns `None` if the owner was
    /// disposed or the memo has no value.
    #[track_caller]
    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        runtime::read(self.id, true, "Memo", f).ok()
    }

    /// Gives the memo a debug name, such as a widget path, for log lines.
    pub fn named(self, name: impl Into<String>) -> Self {
        runtime::set_name(self.id, name.into());
        self
    }
}
