//! Crabshell: Wayland bars and panels written in Rust.
//!
//! A shell is a program whose `main` calls [`run`] with a root closure,
//! which sets the shell up through the context it is given:
//!
//! ```no_run
//! use crabshell::prelude::*;
//!
//! fn main() {
//!     crabshell::run(|cx| cx.set_icon_theme("Adwaita"));
//! }
//! ```
//!
//! Widgets, styles and layout come from `crabshell-ui`, signals and effects
//! from `crabshell-reactive`, drawing from `crabshell-render` and the
//! Wayland platform from `crabshell-wayland`; the [`prelude`] has what a
//! bar uses.
//!
//! # How it works
//!
//! - [`run`] connects to the compositor, gives the root closure a context
//!   that carries the shell's services, and runs the event loop. It never
//!   returns: it exits with status 1 if the root closure panics, and 2 if
//!   the compositor can't be reached or the connection is lost.
//! - Logs go to stderr through `tracing`, including the time from [`run`]
//!   to the first committed frame.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod shell;

#[cfg(test)]
mod tests;

pub use shell::{EXIT_NO_COMPOSITOR, EXIT_ROOT_PANICKED, Error, Shell, ShellExt, run};

/// What a bar uses, in one `use crabshell::prelude::*`.
pub mod prelude {
    pub use crate::ShellExt;
    pub use crabshell_reactive::{Effect, Memo, ReadSignal, Signal};
    pub use crabshell_ui::{
        Align, Anchors, Border, Button, Color, Column, Cx, Edge, Font, Icon, IconPixels,
        IconSource, Justify, Layer, Output, Padding, PanelWindow, Row, Size, Spacer, Stack, Style,
        Text, Widget,
    };
}
