//! Crabshell's widgets and styles.
//!
//! A bar is described with widgets. Every property takes a value, set once,
//! or a closure, which reruns when a signal it read changes:
//!
//! ```
//! use crabshell_reactive::Runtime;
//! use crabshell_ui::prelude::*;
//!
//! const BAR: Style = Style::new()
//!     .background(Color::rgb(0x1e, 0x1e, 0x2e))
//!     .color(Color::rgb(0xcd, 0xd6, 0xf4));
//!
//! let cx = Cx::new(Runtime::current().new_owner());
//! let count = cx.signal(0);
//!
//! let bar = PanelWindow::new(&Output::new("DP-1"))
//!     .anchors(Edge::Top | Edge::Left | Edge::Right)
//!     .height(30)
//!     .style(BAR)
//!     .child(
//!         Row::new()
//!             .gap(8)
//!             .padding(4, 8)
//!             .child(Text::new(move || format!("counted {}", count.get()))),
//!     );
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[allow(dead_code, reason = "the window tree, which comes later, uses it")]
mod cx;
mod geometry;
#[allow(dead_code, reason = "the window tree, which comes later, reads props")]
mod prop;
mod style;
#[allow(dead_code, reason = "only layout, which comes later, measures text")]
mod text;
#[allow(
    dead_code,
    reason = "the window tree, which comes later, reads elements"
)]
mod widgets;
#[allow(dead_code, reason = "the window tree, which comes later, reads panels")]
mod window;

#[cfg(test)]
mod tests;

pub use cx::Cx;
pub use geometry::{Point, Rect, Size};
pub use prop::{IntoProp, Prop};
pub use style::{Border, Color, Font, Padding, Style};
pub use text::TextSystem;
pub use widgets::{
    Align, Button, Column, Element, Icon, IconPixels, IconSource, Justify, Row, Spacer, Stack,
    Text, Widget,
};
pub use window::{Anchors, Edge, Layer, Output, PanelWindow};

/// What a bar's code needs, in one `use`.
pub mod prelude {
    pub use crate::{
        Align, Button, Color, Column, Cx, Edge, Font, Icon, IconSource, Justify, Layer, Output,
        PanelWindow, Row, Size, Spacer, Stack, Style, Text, TextSystem, Widget,
    };
}
