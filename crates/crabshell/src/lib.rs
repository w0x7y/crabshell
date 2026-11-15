//! Crabshell: Wayland bars and panels written in Rust.
//!
//! A shell is a program whose `main` calls [`run`] with a root closure.
//! The closure usually builds a bar for every output with
//! [`per_output`](ShellExt::per_output):
//!
//! ```no_run
//! use crabshell::prelude::*;
//!
//! const BAR: Style = Style::new()
//!     .background(Color::rgb(0x1e, 0x1e, 0x2e))
//!     .color(Color::rgb(0xcd, 0xd6, 0xf4));
//!
//! fn main() {
//!     crabshell::run(|cx| cx.per_output(bar));
//! }
//!
//! fn bar(cx: &Cx, output: &Output) -> PanelWindow {
//!     let count = cx.signal(0);
//!     PanelWindow::new(output)
//!         .anchors(Edge::Top | Edge::Left | Edge::Right)
//!         .height(30)
//!         .style(BAR)
//!         .child(
//!             Row::new().padding(4, 8).child(
//!                 Button::new(Text::new(move || format!("clicked {} times", count.get())))
//!                     .on_click(move |_| count.update(|count| *count += 1)),
//!             ),
//!         )
//! }
//! ```
//!
//! Widgets, styles and layout come from `crabshell-ui`, signals and effects
//! from `crabshell-reactive`, the clock, Sway's workspaces and audio from
//! `crabshell-services`, drawing from `crabshell-render` and the Wayland
//! platform from `crabshell-wayland`; the [`prelude`] has what a bar uses,
//! including [`ServicesExt`](crabshell_services::ServicesExt) for
//! `cx.clock(Precision::Minute)`, `cx.sway()`, `cx.audio()` and
//! `cx.tray()`, and the [`Tray`](crabshell_services::Tray) widget.
//! `examples/bar.rs` is the v0.1 milestone's bar, with each output's
//! workspaces, the tray, the volume and the clock.
//!
//! # How it works
//!
//! - [`run`] connects to the compositor, gives the root closure a context
//!   that carries the shell's services, and runs the event loop. It never
//!   returns: a fatal error is logged and the process exits with
//!   [`EXIT_ROOT_PANICKED`] (1) if the root closure panics, or with
//!   [`EXIT_NO_COMPOSITOR`] (2) if there is no compositor, it lacks a
//!   protocol the shell needs, or the connection is lost.
//! - Each service starts the first time a config asks for it, so a bar
//!   that shows no workspaces never connects to Sway, and one with no
//!   volume never connects to the audio server.
//! - `per_output` runs its closure for each output under a child owner of
//!   the context it was called with. The owner is disposed when the output
//!   goes, or when the compositor closes one of its windows.
//! - Logs go to stderr through `tracing`, including the time from [`run`]
//!   to the first committed frame, and from each of Sway's workspace
//!   events to the first commit after it, from any window.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod colors;
mod control;
mod ipc;
mod per_output;
mod persist;
mod shell;
mod switch_log;

#[cfg(test)]
mod tests;

/// The type of an output's `transform`, from
/// [`cx.outputs()`](ShellExt::outputs). Out of the prelude, beside `Mode`.
pub use crabshell_render::Turn;
/// The type of an output's `mode`, from [`cx.outputs()`](ShellExt::outputs).
/// Out of the prelude, since `Mode` is a common name.
pub use crabshell_wayland::Mode;
/// The time library behind [`cx.clock()`](crabshell_services::ServicesExt::clock),
/// so a config can name `jiff::Zoned` without its own copy.
pub use jiff;
pub use per_output::IntoPanels;
pub use shell::{
  EXIT_NO_COMPOSITOR, EXIT_ROOT_PANICKED, Error, Shell, ShellExt, Toplevels, WindowManager, run,
};

/// What a bar uses, in one `use crabshell::prelude::*`.
pub mod prelude {
  pub use crate::{IntoPanels, ShellExt, Toplevels, WindowManager};
  pub use crabshell_reactive::{Effect, Memo, ReadSignal, Signal, untrack};
  pub use crabshell_services::{
    AudioChannel, AudioNode, AudioStream, AuthRequest, AuthState, BatteryState, Bluetooth,
    BluetoothAdapter, BluetoothAdapterState, BluetoothDevice, BluetoothDeviceState, CloseReason,
    ConnectionFailReason, Connectivity, DesktopAction, DesktopEntries, DesktopEntry, DeviceKind,
    FileError, FileErrorKind, FileView, GreetMessage, GreetState, Greetd, Hyprland, HyprlandEvent,
    HyprlandMonitor, HyprlandToplevel, HyprlandWorkspace, IntoCommand, LoopStatus, Mpris, Network,
    NetworkDevice, NetworkKind, NetworkState, Notification, Notifications, Pam, PamError,
    PamMessage, PamResult, PamState, PeakTarget, PerformanceDegradation, PlaybackStatus, Player,
    PolkitAgent, PowerDevice, PowerProfile, PowerProfiles, Precision, Process, SavedConnection,
    ServicesExt, Sink, Socket, SocketServer, Sway, SwayEvent, SwayEventKind, SwayOutput, Timeout,
    Tray, TrayCategory, TrayHost, TrayItem, TrayMenu, TrayStatus, UPower, Urgency, WifiMode,
    WifiNetwork, WifiSecurity, Workspace,
  };
  pub use crabshell_ui::{
    Adjust, Align, Anchors, Animate, Animation, Border, Button, CaptureSource, Color, Column, Cx,
    Easing, Edge, Fit, FloatingWindow, Font, Gradient, GradientShape, Icon, IconPixels, IconSource,
    Image, Justify, Key, KeyEvent, KeyboardFocus, Layer, ListView, Margins, Menu, MenuEntry,
    MenuToggle, Modifiers, Output, Padding, PanelWindow, PopupWindow, Region, Row, ScreencopyView,
    ScrollView, SessionLock, Shadow, Size, Spacer, Stack, Style, Text, TextInput, Timer,
    Transition, Widget,
  };
  pub use crabshell_wayland::{OutputInfo, Toplevel, Windowset, WindowsetProjection};
  /// For [`cx.after`](Cx::after) and [`cx.interval`](Cx::interval).
  pub use std::time::Duration;
}
