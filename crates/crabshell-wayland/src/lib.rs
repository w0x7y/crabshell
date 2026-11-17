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
//! Each loop iteration dispatches the ready sources, runs `Runtime::flush`,
//! lays out the windows that need it, draws each dirty window whose frame
//! callback has arrived, and flushes the connection. A window draws only
//! when something changed, so an idle window gets no events. Buffers are
//! `logical × scale`, from `wp_fractional_scale_v1` when offered. Popups
//! are `xdg_popup`s and the session lock uses `ext-session-lock-v1`. The
//! design doc's
//! [Wayland platform](https://github.com/w0x7y/crabshell/blob/main/docs/design.md#wayland-platform)
//! section has the full rules.
//!
//! [`Window`]: crabshell_ui::Window

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod activation;
mod blur;
mod buffers;
mod clipboard;
mod data_control;
mod dnd;
mod events;
mod ext_toplevels;
mod focus_grab;
mod foreign_toplevels;
mod global_shortcuts;
mod handlers;
mod hyprland;
mod hyprland_surface;
mod idle;
mod keyboard;
mod lock;
mod outputs;
mod pacing;
mod platform;
mod pointer;
mod popups;
mod scale;
mod screencopy;
mod shortcuts_inhibit;
mod surface;
mod sync;
mod text_input;
mod toplevels;
mod windows;
mod windowsets;

#[cfg(test)]
mod tests;

pub use calloop::{LoopHandle, LoopSignal};
pub use data_control::Selection;
pub use events::Event;
pub use foreign_toplevels::Toplevel;
pub use global_shortcuts::ShortcutWatch;
pub use idle::IdleWatch;
pub use lock::LockId;
pub use outputs::{Mode, OutputInfo};
pub use platform::{Error, EventLoop, Platform, Snapshot, Stats, WindowInfo};
pub use windows::WindowId;
pub use windowsets::{Windowset, WindowsetProjection};
