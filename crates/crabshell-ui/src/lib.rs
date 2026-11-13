//! Crabshell's widgets, styles, layout, text layout, hit testing and
//! invalidation.
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
//!         Row::new().gap(8).padding(4, 8).child(
//!             Button::new(Text::new(move || format!("clicked {} times", count.get())))
//!                 .on_click(move |_cx| count.update(|n| *n += 1)),
//!         ),
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
//! - Pointer events go to [`Window::pointer_motion`] and friends. Hit testing
//!   finds the deepest [`Button`] under the pointer, and its handlers run
//!   with no borrow held, inside `catch_unwind`.
//! - Dropping a [`Window`] disposes its owner, freeing every node, binding
//!   and handler it made.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cx;
mod edit;
mod geometry;
mod input;
mod keyboard;
mod keyed;
mod layout;
mod menu;
mod popup;
mod prop;
mod region;
mod scroll;
mod style;
mod text;
mod text_input;
mod tree;
mod view;
mod widgets;
mod window;

#[cfg(test)]
mod tests;

pub use cx::Cx;
pub use geometry::{Point, Rect, Size};
pub use input::{CursorShape, PointerButton, ScrollDelta};
pub use keyboard::{Clipboard, Key, KeyEvent, Modifiers};
pub use menu::{Menu, MenuEntry, MenuToggle};
pub use popup::PopupWindow;
pub use prop::{IntoProp, Prop};
pub use region::Region;
pub use scroll::{ListView, ScrollView};
pub use style::{Border, Color, Font, Padding, Style};
pub use text::TextSystem;
pub use text_input::{FieldState, Preedit, TextInput};
pub use view::{Content, IconView, ImageView, InputView, NodeView, TextView, WindowView};
pub use widgets::{
  Align, Button, Column, Element, Fit, Icon, IconPixels, IconSource, Image, Justify, Row, Spacer,
  Stack, Text, Widget,
};
pub use window::{
  Anchors, Edge, KeyboardFocus, Layer, Margins, Output, PanelWindow, SurfaceConfig, Window,
};

/// What a bar's code needs, in one `use`.
pub mod prelude {
  pub use crate::{
    Align, Button, Color, Column, Cx, Edge, Font, Icon, IconSource, Justify, Layer, Menu, Output,
    PanelWindow, PopupWindow, Row, Size, Spacer, Stack, Style, Text, TextSystem, Widget, Window,
  };
}
