//! What every handle type shares.
//!
//! A handle is only a key into the runtime. Two handles are equal exactly
//! when they point to the same node, and printing one shows its key. None of
//! this needs anything from `T`, so the impls are written out here instead of
//! derived: `#[derive(PartialEq)]` on `Signal<T>` would only implement it
//! when `T: PartialEq`, and the same for each other trait.

use std::marker::PhantomData;

/// The marker field of a typed handle. It stores nothing.
///
/// `fn(T) -> T` ties the handle to `T` and makes it invariant in `T`:
/// subtyping can't turn a `Signal<A>` into a `Signal<B>` for any other type
/// `B`, which would no longer match the value the runtime holds. `*const ()`
/// is neither `Send` nor `Sync`, so the handle can't leave its thread.
pub(crate) type Marker<T> = PhantomData<(fn(T) -> T, *const ())>;

/// The marker field of `Effect` and `Owner`, which have no `T`.
pub(crate) type NotSend = PhantomData<*const ()>;

/// Implements `Clone`, `Copy`, `PartialEq`, `Eq`, `Hash` and `Debug` for a
/// handle type from its `id` field alone, with no bounds on `T`.
macro_rules! handle_impls {
    ($name:ident $(<$t:ident>)?) => {
        impl$(<$t>)? Clone for $name$(<$t>)? {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl$(<$t>)? Copy for $name$(<$t>)? {}

        impl$(<$t>)? PartialEq for $name$(<$t>)? {
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id
            }
        }

        impl$(<$t>)? Eq for $name$(<$t>)? {}

        impl$(<$t>)? std::hash::Hash for $name$(<$t>)? {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                std::hash::Hash::hash(&self.id, state);
            }
        }

        impl$(<$t>)? std::fmt::Debug for $name$(<$t>)? {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.id).finish()
            }
        }
    };
}

pub(crate) use handle_impls;
