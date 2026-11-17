//! Crabshell's reactive core: a small single-threaded signal system.
//!
//! | Type | Created with | What it is |
//! |---|---|---|
//! | [`Signal<T>`] | [`Owner::signal`] | A value you set |
//! | [`ReadSignal<T>`] | [`Signal::read_only`] | A read-only view of a signal |
//! | [`Memo<T>`] | [`Owner::memo`], needs `T: PartialEq` | A derived value, recomputed only when read while stale |
//! | [`Effect`] | [`Owner::effect`] or [`Owner::effect_scoped`] | Code that reruns when what it read changes |
//!
//! `crabshell-ui`'s `Cx` wraps an [`Owner`], so widget code writes
//! `cx.signal(..)`, `cx.memo(..)` and `cx.effect(..)`.
//!
//! # How it behaves
//!
//! Handles are `Copy` keys into this thread's arena. `get()` inside a memo
//! or effect records a dependency; [`untrack`] and the `_untracked` methods
//! don't. `set()` only marks readers stale and queues effects, and
//! [`Runtime::flush`] runs them. A memo recomputes when read while stale and
//! wakes its dependents only if its value changed. Disposing an [`Owner`]
//! frees everything created under it. A memo or effect that panics is logged
//! and disabled, and the runtime keeps working. The full rules are in the
//! design doc's
//! [Reactive core](https://github.com/w0x7y/crabshell/blob/main/docs/design.md#reactive-core)
//! section.
//!
//! # Example
//!
//! ```
//! use std::cell::Cell;
//! use std::rc::Rc;
//!
//! use crabshell_reactive::Runtime;
//!
//! let runtime = Runtime::current();
//! let owner = runtime.new_owner(); // a widget opens one of these
//!
//! let count = owner.signal(1);
//! let doubled = owner.memo(move || count.get() * 2);
//! let shown = Rc::new(Cell::new(0));
//! let sink = Rc::clone(&shown);
//! owner.effect(move || sink.set(doubled.get())); // a binding is one of these
//!
//! runtime.flush(); // the event loop calls this once per iteration
//! assert_eq!(shown.get(), 2);
//!
//! count.set(5); // queues the effect but runs nothing
//! assert_eq!(shown.get(), 2);
//! runtime.flush();
//! assert_eq!(shown.get(), 10);
//!
//! owner.dispose(); // dropping the widget does this
//! assert_eq!(count.try_get(), None);
//! ```
//!
//! Handles stay on the thread that made them, so this doesn't compile:
//!
//! ```compile_fail,E0277
//! let owner = crabshell_reactive::Runtime::current().new_owner();
//! let count = owner.signal(0);
//! std::thread::spawn(move || count.get());
//! ```
//!
//! Handles are invariant in `T`, so subtyping can't turn a handle into one
//! for a different type than the value it points to:
//!
//! ```compile_fail,E0308
//! use crabshell_reactive::{Runtime, Signal};
//!
//! fn print_it(_: &str) {}
//! let owner = Runtime::current().new_owner();
//! let general: Signal<for<'a> fn(&'a str)> = owner.signal(print_it as for<'a> fn(&'a str));
//! let narrowed: Signal<fn(&'static str)> = general;
//! narrowed.try_get();
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod effect;
mod handle;
mod memo;
mod owner;
mod runtime;
mod signal;

#[cfg(test)]
mod tests;

pub use effect::Effect;
pub use memo::Memo;
pub use owner::Owner;
pub use runtime::{MAX_RUNS_PER_FLUSH, Runtime, untrack};
pub use signal::{ReadSignal, Signal};
