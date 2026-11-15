//! Turning `wl_pointer` and `wl_touch` events into calls on a window:
//! hit testing, hover, pressed state, drags and scrolling all live in
//! `crabshell-ui`.

use crabshell_ui::{CursorShape, Point, PointerButton, ScrollSource, Wheel, WheelAxis, Window};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::AxisSource;
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use smithay_client_toolkit::seat::pointer::{
  AxisScroll, BTN_BACK, BTN_EXTRA, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE,
  PointerEventKind,
};

use crate::windows::WindowId;

/// What one pointer event means to a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Input {
  /// The pointer entered or moved, to this point in logical pixels.
  Motion(Point),
  /// The pointer left.
  Leave,
  /// A button went down (`true`) or up, at this time in milliseconds.
  Button(PointerButton, bool, u32),
  /// Scrolling on either axis, or its end.
  Scroll(Wheel),
}

/// The inputs a pointer event at `position` makes. Buttons other than
/// left, right, middle, back and forward, and positions that aren't
/// numbers, make none.
pub(crate) fn inputs(kind: &PointerEventKind, (x, y): (f64, f64)) -> Vec<Input> {
  match kind {
    PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
      point(x, y).map(Input::Motion).into_iter().collect()
    }
    PointerEventKind::Leave { .. } => vec![Input::Leave],
    PointerEventKind::Press { button, time, .. } => button_input(*button, true, *time),
    PointerEventKind::Release { button, time, .. } => button_input(*button, false, *time),
    PointerEventKind::Axis {
      horizontal,
      vertical,
      source,
      ..
    } => scroll_input(horizontal, vertical, *source)
      .map(Input::Scroll)
      .into_iter()
      .collect(),
  }
}

fn point(x: f64, y: f64) -> Option<Point> {
  (x.is_finite() && y.is_finite()).then(|| Point::new(x as f32, y as f32))
}

fn button_input(button: u32, pressed: bool, time: u32) -> Vec<Input> {
  let button = match button {
    BTN_LEFT => PointerButton::Left,
    BTN_RIGHT => PointerButton::Right,
    BTN_MIDDLE => PointerButton::Middle,
    BTN_SIDE | BTN_BACK => PointerButton::Back,
    BTN_EXTRA | BTN_FORWARD => PointerButton::Forward,
    _ => return Vec::new(),
  };
  vec![Input::Button(button, pressed, time)]
}

/// A frame's scrolling on both axes. A wheel reports `axis_value120`, or
/// whole notches on compositors too old for it; a touchpad reports
/// pixels, and `axis_stop` when the fingers lift. Without a source, one
/// with notches is a wheel. Nothing to report, or a number that isn't
/// one, makes none.
fn scroll_input(
  horizontal: &AxisScroll,
  vertical: &AxisScroll,
  source: Option<AxisSource>,
) -> Option<Wheel> {
  let (x, y) = (axis(horizontal)?, axis(vertical)?);
  if x == WheelAxis::default() && y == WheelAxis::default() {
    return None;
  }
  let source = match source {
    Some(AxisSource::Finger) => ScrollSource::Finger,
    Some(AxisSource::Continuous) => ScrollSource::Continuous,
    Some(AxisSource::WheelTilt) => ScrollSource::WheelTilt,
    Some(AxisSource::Wheel) => ScrollSource::Wheel,
    _ if x.notches != 0.0 || y.notches != 0.0 => ScrollSource::Wheel,
    _ => ScrollSource::Continuous,
  };
  Some(Wheel { source, x, y })
}

fn axis(axis: &AxisScroll) -> Option<WheelAxis> {
  if !axis.absolute.is_finite() {
    return None;
  }
  let value120 = if axis.value120 != 0 {
    axis.value120
  } else {
    axis.discrete.saturating_mul(120)
  };
  Some(WheelAxis {
    pixels: axis.absolute as f32,
    notches: value120 as f32 / 120.0,
    stop: axis.stop,
  })
}

/// Hands `input` to `window`, which may run a button's handler, and
/// returns the cursor to show afterward.
pub(crate) fn apply(window: &Window, input: Input) -> CursorShape {
  match input {
    Input::Motion(position) => return window.pointer_motion(position),
    Input::Leave => window.pointer_leave(),
    Input::Button(button, pressed, time) => window.pointer_button_at(button, pressed, time),
    Input::Scroll(wheel) => window.pointer_scroll(wheel),
  }
  window.cursor()
}

/// The shape to send for `cursor` to a shape device of `version`. The
/// two shapes version 2 added fall back to the nearest of version 1.
pub(crate) fn shape(cursor: CursorShape, version: u32) -> Shape {
  match cursor {
    CursorShape::Default => Shape::Default,
    CursorShape::ContextMenu => Shape::ContextMenu,
    CursorShape::Help => Shape::Help,
    CursorShape::Pointer => Shape::Pointer,
    CursorShape::Progress => Shape::Progress,
    CursorShape::Wait => Shape::Wait,
    CursorShape::Cell => Shape::Cell,
    CursorShape::Crosshair => Shape::Crosshair,
    CursorShape::Text => Shape::Text,
    CursorShape::VerticalText => Shape::VerticalText,
    CursorShape::Alias => Shape::Alias,
    CursorShape::Copy => Shape::Copy,
    CursorShape::Move => Shape::Move,
    CursorShape::NoDrop => Shape::NoDrop,
    CursorShape::NotAllowed => Shape::NotAllowed,
    CursorShape::Grab => Shape::Grab,
    CursorShape::Grabbing => Shape::Grabbing,
    CursorShape::EResize => Shape::EResize,
    CursorShape::NResize => Shape::NResize,
    CursorShape::NeResize => Shape::NeResize,
    CursorShape::NwResize => Shape::NwResize,
    CursorShape::SResize => Shape::SResize,
    CursorShape::SeResize => Shape::SeResize,
    CursorShape::SwResize => Shape::SwResize,
    CursorShape::WResize => Shape::WResize,
    CursorShape::EwResize => Shape::EwResize,
    CursorShape::NsResize => Shape::NsResize,
    CursorShape::NeswResize => Shape::NeswResize,
    CursorShape::NwseResize => Shape::NwseResize,
    CursorShape::ColResize => Shape::ColResize,
    CursorShape::RowResize => Shape::RowResize,
    CursorShape::AllScroll => Shape::AllScroll,
    CursorShape::ZoomIn => Shape::ZoomIn,
    CursorShape::ZoomOut => Shape::ZoomOut,
    CursorShape::DndAsk if version >= 2 => Shape::DndAsk,
    CursorShape::DndAsk => Shape::ContextMenu,
    CursorShape::AllResize if version >= 2 => Shape::AllResize,
    CursorShape::AllResize => Shape::Move,
  }
}

/// A seat's touchscreen, as one pointer: the first finger down is
/// followed until it lifts, and other fingers are ignored meanwhile.
#[derive(Debug, Default)]
pub(crate) struct Touch {
  /// The finger followed, and the window it went down on.
  active: Option<(i32, WindowId)>,
}

impl Touch {
  /// A finger went down on `window`: a motion there and a left press,
  /// unless another finger is down.
  pub(crate) fn down(
    &mut self,
    id: i32,
    window: WindowId,
    (x, y): (f64, f64),
    time: u32,
  ) -> Vec<Input> {
    let Some(at) = point(x, y).filter(|_| self.active.is_none()) else {
      return Vec::new();
    };
    self.active = Some((id, window));
    vec![
      Input::Motion(at),
      Input::Button(PointerButton::Left, true, time),
    ]
  }

  /// The followed finger moved.
  pub(crate) fn motion(&mut self, id: i32, (x, y): (f64, f64)) -> Vec<Input> {
    match point(x, y) {
      Some(at) if self.follows(id) => vec![Input::Motion(at)],
      _ => Vec::new(),
    }
  }

  /// A finger lifted; the followed one releases and leaves, so a tap is a
  /// click.
  pub(crate) fn up(&mut self, id: i32, time: u32) -> Vec<Input> {
    if !self.follows(id) {
      return Vec::new();
    }
    self.active = None;
    vec![
      Input::Button(PointerButton::Left, false, time),
      Input::Leave,
    ]
  }

  /// The compositor took the touch over: it leaves without a click.
  pub(crate) fn cancel(&mut self) -> Vec<Input> {
    match self.active.take() {
      Some(_) => vec![Input::Leave],
      None => Vec::new(),
    }
  }

  /// The window the followed finger went down on.
  pub(crate) fn window(&self) -> Option<WindowId> {
    self.active.map(|(_, window)| window)
  }

  fn follows(&self, id: i32) -> bool {
    self.active.is_some_and(|(active, _)| active == id)
  }
}

/// A pointer's cursor: the window it is over and the shape last set there,
/// so a shape is sent only when it changes.
#[derive(Debug, Default)]
pub(crate) struct Cursor {
  /// The window, with the serial of the enter, which setting a cursor
  /// needs.
  over: Option<(WindowId, u32)>,
  shown: Option<CursorShape>,
}

impl Cursor {
  /// The pointer entered `window`. Its cursor must be set again.
  pub(crate) fn enter(&mut self, serial: u32, window: WindowId) {
    *self = Cursor {
      over: Some((window, serial)),
      shown: None,
    };
  }

  /// The pointer left its window.
  pub(crate) fn leave(&mut self) {
    *self = Cursor::default();
  }

  /// The window the pointer is over.
  pub(crate) fn focus(&self) -> Option<WindowId> {
    self.over.map(|(window, _)| window)
  }

  /// The serial and shape to send so the cursor shows `wanted`; `None` if
  /// it already does, or the pointer is over no window.
  pub(crate) fn update(&mut self, wanted: CursorShape) -> Option<(u32, CursorShape)> {
    let (_, serial) = self.over?;
    if self.shown == Some(wanted) {
      return None;
    }
    self.shown = Some(wanted);
    Some((serial, wanted))
  }
}
