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
//! [`run`] gives the root closure a context that carries the shell's
//! services, and each service starts the first time a config asks for it,
//! so a bar with no volume never connects to the audio server.
//! `per_output` runs its closure under a child owner per output, disposed
//! when the output goes or the compositor closes one of its windows. See
//! [`run`] for exit codes and logging.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod clipboard;
mod colors;
mod control;
mod icon_theme;
mod ipc;
mod per_output;
mod persist;
mod shell;
mod switch_log;
#[cfg(target_env = "gnu")]
mod tuned;

#[cfg(test)]
mod tests;

pub use clipboard::{Clipboard, ClipboardContent, ClipboardEntry};
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
/// The JSON library behind [`Niri::action`](crabshell_services::Niri::action)
/// and `on_event`, so a config can write `serde_json::json!` without its
/// own copy.
pub use serde_json;
pub use shell::{
  EXIT_NO_COMPOSITOR, EXIT_ROOT_PANICKED, Error, Shell, ShellExt, Toplevels, WindowManager, run,
};

/// What a bar uses, in one `use crabshell::prelude::*`.
pub mod prelude {
  pub use crate::{
    Clipboard, ClipboardContent, ClipboardEntry, IntoPanels, ShellExt, Toplevels, WindowManager,
  };
  pub use crabshell_reactive::{Effect, Memo, ReadSignal, Signal, untrack};
  pub use crabshell_services::Locale;
  pub use crabshell_services::Scanner;
  pub use crabshell_services::{
    AudioChannel, AudioNode, AudioStream, AuthRequest, AuthState, BatteryState, Bluetooth,
    BluetoothAdapter, BluetoothAdapterState, BluetoothAgent, BluetoothDevice, BluetoothDeviceState,
    CloseReason, ConnectionFailReason, Connectivity, DesktopAction, DesktopEntries, DesktopEntry,
    DeviceKind, FileError, FileErrorKind, FileView, FolderEntry, FolderView, GreetMessage,
    GreetState, Greetd, Http, HttpError, HttpErrorKind, Hyprland, HyprlandEvent, HyprlandMonitor,
    HyprlandToplevel, HyprlandWorkspace, IntoCommand, LoopStatus, Mpris, Network, NetworkDevice,
    NetworkKind, NetworkState, Niri, NiriKeyboardLayouts, NiriWindow, NiriWorkspace, Notification,
    Notifications, PairingKind, PairingRequest, Pam, PamError, PamMessage, PamResult, PamState,
    PeakTarget, PerformanceDegradation, PlaybackStatus, Player, PolkitAgent, PowerDevice,
    PowerProfile, PowerProfiles, Precision, Process, SavedConnection, ServicesExt, Sink, Socket,
    SocketServer, Sway, SwayEvent, SwayEventKind, SwayOutput, SystemStats, Temperature, Timeout,
    Tray, TrayCategory, TrayHost, TrayItem, TrayMenu, TrayStatus, UPower, Urgency, WifiMode,
    WifiNetwork, WifiSecurity, Workspace,
  };
  pub use crabshell_services::{Backlight, Brightness, Logind};
  pub use crabshell_services::{PipeWire, PwLink, PwLinkGroup, PwLinkState, PwNode};
  pub use crabshell_ui::{
    Adjust, Align, Anchors, Animate, Animation, Border, Button, Canvas, Cap, CaptureSource, Color,
    Column, CursorShape, Cx, Drag, DragContent, DragPhase, Easing, Edge, Elide, Fit,
    FloatingWindow, Font, FontFeature, Gradient, GradientShape, Grid, GridView, Icon, IconPixels,
    IconSource, Image, ImageTransition, IntoProp, Join, Justify, Key, KeyEvent, KeyboardFocus,
    Layer, ListView, Margins, Mask, Menu, MenuEntry, MenuToggle, Modifiers, Output, Padding, Paint,
    PanelWindow, Path, Pen, Point, PopupWindow, Radii, Rect, Region, Row, ScreencopyView,
    ScrollSource, ScrollView, SessionLock, Shadow, Size, Slider, Spacer, Span, Spring, Stack,
    Stroke, Style, Text, TextAlign, TextArea, TextInput, TextMetrics, Timer, Transition, Wheel,
    WheelAxis, Widget, parse_markup,
  };
  pub use crabshell_wayland::{OutputInfo, Toplevel, Windowset, WindowsetProjection};
  /// For [`cx.after`](Cx::after) and [`cx.interval`](Cx::interval).
  pub use std::time::Duration;
}
