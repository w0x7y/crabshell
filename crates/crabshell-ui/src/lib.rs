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

mod animation;
mod canvas;
mod cx;
mod edit;
mod floating;
mod geometry;
mod grid_view;
mod image;
mod input;
mod keyboard;
mod keyed;
mod layout;
mod lock;
mod menu;
mod popup;
mod prop;
mod region;
mod rich;
mod screencopy;
mod scroll;
mod scrolling;
mod select;
mod slider;
mod style;
mod text;
mod text_area;
mod text_input;
mod timer;
mod tree;
mod view;
mod widgets;
mod window;

#[cfg(test)]
mod tests;

pub use animation::{
  Animate, Animation, Easing, Spring, Transition, advance_animations, animating,
};
pub use canvas::{Canvas, CanvasOp, Cap, Join, Paint, Path, PathCommand, Pen, Stroke};
pub use cx::Cx;
pub use floating::{FloatingConfig, FloatingWindow, WindowRequest};
pub use geometry::{Affine, Point, Rect, Size};
pub use grid_view::GridView;
pub use image::{ImageTransition, dropped_images};
pub use input::{CursorShape, Drag, DragPhase, PointerButton, ScrollSource, Wheel, WheelAxis};
pub use keyboard::{Clipboard, Key, KeyEvent, Modifiers};
pub use lock::{Lock, SessionLock};
pub use menu::{Menu, MenuEntry, MenuToggle};
pub use popup::{Adjust, Placement, PopupWindow};
pub use prop::{IntoProp, Prop};
pub use region::Region;
pub use rich::{MARKUP_CAP, Span, parse_markup};
pub use screencopy::{CaptureFrame, CaptureRequest, CaptureSource, ScreencopyView};
pub use scroll::{ListView, ScrollView};
pub use slider::Slider;
pub use style::{
  Border, Color, Font, FontFeature, Gradient, GradientShape, Mask, Padding, Radii, Shadow, Style,
  TextAlign,
};
pub use text::{TextMetrics, TextRun, TextSystem};
pub use text_area::TextArea;
pub use text_input::{FieldState, Preedit, TextInput};
pub use timer::{Timer, fire_timers, next_timer};
pub use view::{
  CanvasView, CaptureView, Content, IconView, ImageView, InputView, NodeView, TextView, WindowView,
};
pub use widgets::{
  Align, Button, Column, Element, Elide, Fit, Grid, Icon, IconPixels, IconSource, Image,
  ImageFilters, Justify, Row, Spacer, Stack, Text, Widget,
};
pub use window::{
  Anchors, Edge, KeyboardFocus, Layer, Margins, Output, PanelWindow, SurfaceConfig, Window,
};

/// What a bar's code needs, in one `use`.
pub mod prelude {
  pub use crate::{
    Align, Button, Canvas, Cap, Color, Column, CursorShape, Cx, Drag, DragPhase, Edge, Elide, Font,
    FontFeature, Grid, GridView, Icon, IconSource, ImageTransition, Join, Justify, Layer, Mask,
    Menu, Output, Paint, PanelWindow, Path, Pen, Point, PopupWindow, Radii, Rect, Row,
    ScrollSource, Size, Slider, Spacer, Span, Stack, Stroke, Style, Text, TextAlign, TextMetrics,
    TextSystem, Timer, Wheel, WheelAxis, Widget, Window, parse_markup,
  };
}
