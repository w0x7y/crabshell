//! Crabshell's Wayland platform: a single-threaded calloop event loop with
//! the compositor connection, layer-shell windows drawn into shared-memory
//! buffers, outputs as they come and go, and pointer input.
//!
//! [`EventLoop`] owns the loop and the [`Platform`], which every source's
//! callback receives. Windows are `crabshell-ui` [`Window`]s handed to
//! [`Platform::add_window`]; outputs coming and going, and windows the
//! compositor closes, are reported through [`Platform::on_event`]:
//!
//! ```no_run
//! use crabshell_reactive::Runtime;
//! use crabshell_ui::prelude::*;
//! use crabshell_wayland::{Event, EventLoop};
//!
//! let runtime = Runtime::current();
//! let cx = Cx::new(runtime.new_owner());
//! let mut event_loop = EventLoop::connect()?;
//! event_loop.platform().on_event(move |platform, event| {
//!     if let Event::OutputAdded(output) = event {
//!         let bar = PanelWindow::new(&Output::new(output.name.clone()))
//!             .anchors(Edge::Top | Edge::Left | Edge::Right)
//!             .height(30)
//!             .child(Text::new("hello"));
//!         platform.add_window(Window::new(&cx, bar));
//!     }
//! });
//! event_loop.run()?;
//! # Ok::<(), crabshell_wayland::Error>(())
//! ```
//!
//! # How it works
//!
//! - Each loop iteration dispatches the ready sources, runs
//!   `Runtime::flush`, lays out the windows that need it, draws each one
//!   that is dirty and has its frame callback, and flushes the connection.
//! - A window's layer surface is created after the first flush, once its
//!   output is announced, and draws nothing before its first configure.
//!   Settings layer-shell would refuse, such as a width of 0 without both
//!   side anchors, are logged and never sent. The compositor's `closed`, or
//!   the output going away, drops the window and its owner.
//! - Each window has two or three ARGB8888 buffers from sctk's `SlotPool`,
//!   reused once released, sized `logical × scale` rounded halfway away
//!   from zero as `wp_fractional_scale_v1` says, with a damage
//!   history per window and an age per buffer. Every successful draw is
//!   committed, reporting its rectangles with `damage_buffer`.
//! - Each commit asks for a frame callback. A window draws only once it
//!   arrives and something changed; with nothing changed nothing is
//!   committed, so an idle window gets no events.
//! - Scale comes from `wp_fractional_scale_v1` with `wp_viewporter` when
//!   the compositor has both, otherwise from whole scales and
//!   `set_buffer_scale`. A new scale repaints the whole window.
//! - Pointer events go to `crabshell-ui`'s hit testing; the cursor shape is
//!   set with `wp_cursor_shape_v1` when offered.
//!
//! [`Window`]: crabshell_ui::Window

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod buffers;
mod events;
mod handlers;
mod outputs;
mod pacing;
mod platform;
mod pointer;
mod popups;
mod scale;
mod surface;
mod windows;

#[cfg(test)]
mod tests;

pub use calloop::{LoopHandle, LoopSignal};
pub use events::Event;
pub use outputs::{Mode, OutputInfo};
pub use platform::{Error, EventLoop, Platform, Snapshot, Stats, WindowInfo};
pub use windows::WindowId;
