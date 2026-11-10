//! Crabshell's widgets, styles, layout, text layout and invalidation.
//!
//! A bar is described with widgets and turned into a live [`Window`]:
//!
//! ```
//! use crabshell_reactive::Runtime;
//! use crabshell_ui::prelude::*;
//!
//! const BAR: Style = Style::new()
//!     .background(Color::rgb(0x1e, 0x1e, 0x2e))
//!     .color(Color::rgb(0xcd, 0xd6, 0xf4));
//!
//! let runtime = Runtime::current();
//! let cx = Cx::new(runtime.new_owner());
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
//!
//! // The platform does the rest once per event-loop iteration.
//! let window = Window::new(&cx, bar);
//! let mut text = TextSystem::new();
//! window.set_size(Size::new(1920.0, 30.0));
//! runtime.flush(); // bindings run
//! window.layout(&mut text);
//! let _damage = window.take_damage(); // what the renderer repaints
//! ```
//!
//! # How it fits together
//!
//! - Widgets are descriptions. [`Window::new`] turns them into a tree of
//!   nodes, each with its own reactive owner. A property given a closure
//!   becomes an effect that writes the node when a signal it read changes.
//! - Each property is a layout property (text, font, padding, gap, size,
//!   children) or a paint property (colors, background, border, opacity,
//!   radius). A paint change records the widget's area as damage. A layout
//!   change marks the window for [`Window::layout`], which records the old
//!   and new area of every widget that moved or changed.
//! - [`TextSystem`] loads only the fonts fontconfig matches for the families
//!   styles name, plus fallbacks for missing characters. Each `Text` keeps
//!   one cosmic-text buffer that layout measures and the renderer draws.
//! - Dropping a [`Window`] disposes its owner, freeing every node and
//!   binding it made.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cx;
mod geometry;
mod layout;
mod prop;
mod style;
mod text;
mod tree;
mod view;
mod widgets;
mod window;

#[cfg(test)]
mod tests;

pub use cx::Cx;
pub use geometry::{Point, Rect, Size};
pub use prop::{IntoProp, Prop};
pub use style::{Border, Color, Font, Padding, Style};
pub use text::TextSystem;
pub use view::{Content, IconView, NodeView, TextView, WindowView};
pub use widgets::{
    Align, Button, Column, Element, Icon, IconPixels, IconSource, Justify, Row, Spacer, Stack,
    Text, Widget,
};
pub use window::{Anchors, Edge, Layer, Output, PanelWindow, SurfaceConfig, Window};

/// What a bar's code needs, in one `use`.
pub mod prelude {
    pub use crate::{
        Align, Button, Color, Column, Cx, Edge, Font, Icon, IconSource, Justify, Layer, Output,
        PanelWindow, Row, Size, Spacer, Stack, Style, Text, TextSystem, Widget, Window,
    };
}
