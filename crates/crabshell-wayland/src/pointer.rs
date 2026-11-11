//! Turning `wl_pointer` events into calls on a window: hit testing, hover,
//! pressed state and scrolling all live in `crabshell-ui`.

use crabshell_ui::{CursorShape, Point, PointerButton, ScrollDelta, Window};
use smithay_client_toolkit::seat::pointer::{
  AxisScroll, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, PointerEventKind,
};

use crate::windows::WindowId;

/// What one pointer event means to a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Input {
  /// The pointer entered or moved, to this point in logical pixels.
  Motion(Point),
  /// The pointer left.
  Leave,
  /// A button went down (`true`) or up.
  Button(PointerButton, bool),
  /// Vertical scrolling.
  Scroll(ScrollDelta),
  /// A touchpad scroll ended.
  ScrollEnd,
}

/// The inputs a pointer event at `position` makes. Buttons other than
/// left, right and middle, horizontal scrolling and positions that aren't
/// numbers make none.
pub(crate) fn inputs(kind: &PointerEventKind, (x, y): (f64, f64)) -> Vec<Input> {
  let position = (x.is_finite() && y.is_finite()).then(|| Point::new(x as f32, y as f32));
  match kind {
    PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
      position.map(Input::Motion).into_iter().collect()
    }
    PointerEventKind::Leave { .. } => vec![Input::Leave],
    PointerEventKind::Press { button, .. } => button_input(*button, true),
    PointerEventKind::Release { button, .. } => button_input(*button, false),
    PointerEventKind::Axis { vertical, .. } => scroll_inputs(vertical),
  }
}

fn button_input(button: u32, pressed: bool) -> Vec<Input> {
  let button = match button {
    BTN_LEFT => PointerButton::Left,
    BTN_RIGHT => PointerButton::Right,
    BTN_MIDDLE => PointerButton::Middle,
    _ => return Vec::new(),
  };
  vec![Input::Button(button, pressed)]
}

/// A wheel reports `axis_value120`, or whole notches on compositors too old
/// for it; a touchpad reports pixels, and `axis_stop` when the fingers
/// lift.
fn scroll_inputs(axis: &AxisScroll) -> Vec<Input> {
  let delta = if axis.value120 != 0 {
    Some(ScrollDelta::Wheel(axis.value120))
  } else if axis.discrete != 0 {
    Some(ScrollDelta::Wheel(axis.discrete.saturating_mul(120)))
  } else if axis.absolute != 0.0 && axis.absolute.is_finite() {
    Some(ScrollDelta::Pixels(axis.absolute as f32))
  } else {
    None
  };
  let mut inputs: Vec<Input> = delta.map(Input::Scroll).into_iter().collect();
  if axis.stop {
    inputs.push(Input::ScrollEnd);
  }
  inputs
}

/// Hands `input` to `window`, which may run a button's handler, and
/// returns the cursor to show afterward.
pub(crate) fn apply(window: &Window, input: Input) -> CursorShape {
  match input {
    Input::Motion(position) => return window.pointer_motion(position),
    Input::Leave => window.pointer_leave(),
    Input::Button(button, pressed) => window.pointer_button(button, pressed),
    Input::Scroll(delta) => window.pointer_scroll(delta),
    Input::ScrollEnd => window.pointer_scroll_end(),
  }
  window.cursor()
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
