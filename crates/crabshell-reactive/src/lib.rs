//! Crabshell's reactive core: a small single-threaded signal system.
//!
//! | Type | Created with | What it is |
//! |---|---|---|
//! | [`Signal<T>`] | [`Owner::signal`] | A value you set |
//! | [`ReadSignal<T>`] | [`Signal::read_only`] | A read-only view of a signal |
//! | [`Effect`] | [`Owner::effect`] | Code that reruns when what it read changes |
//!
//! `crabshell-ui`'s `Cx` wraps an [`Owner`], so widget code writes
//! `cx.signal(..)` and `cx.effect(..)`.
//!
//! # How it behaves
//!
//! - Handles are `Copy` keys into this thread's arena, and can't be sent to
//!   another thread.
//! - `get()` and `with()` inside an effect record a dependency.
//!   `get_untracked()` and `with_untracked()` don't. Dependencies are
//!   recorded afresh on every run.
//! - `set()` and `update()` store the value, mark what read it as stale and
//!   queue effects. They never run anything; [`Runtime::flush`] does.
//!   `try_set()` and `try_update()` return `false` on a disposed signal
//!   instead of panicking.
//! - `flush()` runs queued effects in creation order, each at most once per
//!   round. Effects queued by writes during the flush run in a later round
//!   of the same flush. An effect queued again after running
//!   [`MAX_RUNS_PER_FLUSH`] times in one flush is logged and disabled.
//! - Disposing an [`Owner`] frees everything created under it. After that,
//!   `get()` on its handles panics and `try_get()` returns `None`.
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
//! let shown = Rc::new(Cell::new(0));
//! let sink = Rc::clone(&shown);
//! owner.effect(move || sink.set(count.get() * 2)); // a binding is one of these
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
mod owner;
mod runtime;
mod signal;

#[cfg(test)]
mod tests;

pub use effect::Effect;
pub use owner::Owner;
pub use runtime::{MAX_RUNS_PER_FLUSH, Runtime};
pub use signal::{ReadSignal, Signal};
