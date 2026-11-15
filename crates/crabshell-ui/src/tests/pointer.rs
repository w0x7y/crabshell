//! Pointer input added in Phase 15: both scroll axes, side buttons and
//! cursors.

use std::cell::RefCell;
use std::rc::Rc;

use super::{Counter, Harness, lift, rect_at, wheel};
use crate::{
  Align, Button, CursorShape, Icon, Point, PointerButton, Row, ScrollSource, Stack, TextInput,
  Wheel, WheelAxis, Window,
};

type Log = Rc<RefCell<Vec<String>>>;

fn log() -> Log {
  Rc::new(RefCell::new(Vec::new()))
}

/// A handler that writes `name` to `log`.
fn note(log: &Log, name: &'static str) -> impl Fn(&crate::Cx) + 'static {
  let log = Rc::clone(log);
  move |_| log.borrow_mut().push(name.to_owned())
}

fn square() -> Icon {
  Icon::new("x").size(20)
}

/// A 200 by 30 window holding `button`, 20 pixels square at the left.
fn one(h: &mut Harness, button: Button) -> Window {
  let row = Row::new().align(Align::Start).child(button);
  h.window(h.panel().child(row), 200.0, 30.0)
}

#[test]
fn on_wheel_gets_both_axes_and_on_scroll_only_vertical_steps() {
  let mut h = Harness::new();
  let wheels = Rc::new(RefCell::new(Vec::new()));
  let steps = Rc::new(RefCell::new(Vec::new()));
  let (got, stepped) = (Rc::clone(&wheels), Rc::clone(&steps));
  let window = one(
    &mut h,
    Button::new(square())
      .on_wheel(move |_, wheel| got.borrow_mut().push(wheel))
      .on_scroll(move |_, n| stepped.borrow_mut().push(n)),
  );
  window.pointer_motion(Point::new(5.0, 5.0));
  let sideways = Wheel {
    source: ScrollSource::Finger,
    x: WheelAxis {
      pixels: 30.0,
      ..WheelAxis::default()
    },
    y: WheelAxis::default(),
  };
  window.pointer_scroll(sideways);
  window.pointer_scroll(wheel(120));
  window.pointer_scroll(lift());
  assert_eq!(*wheels.borrow(), [sideways, wheel(120), lift()]);
  assert_eq!(*steps.borrow(), [-1], "sideways gives no steps");
}

#[test]
fn a_wheel_handler_takes_the_scroll_from_a_scroll_view() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0_f32);
  let wheels = Counter::default();
  let counted = wheels.clone();
  let list = (0..20).fold(crate::Column::new(), |column, _| column.child(square()));
  let view = Stack::new()
    .child(crate::ScrollView::new(list).offset(offset))
    .child(Button::new(square()).on_wheel(move |_, _| counted.hit()));
  let window = h.window(h.panel().child(view), 200.0, 30.0);
  window.pointer_motion(Point::new(5.0, 5.0));
  window.pointer_scroll(wheel(120));
  assert_eq!((wheels.get(), offset.get_untracked()), (1, 0.0));
}

#[test]
fn the_side_buttons_run_back_and_forward() {
  let mut h = Harness::new();
  let log = log();
  let window = one(
    &mut h,
    Button::new(square())
      .on_back(note(&log, "back"))
      .on_forward(note(&log, "forward"))
      .on_click(note(&log, "click")),
  );
  window.pointer_motion(Point::new(5.0, 5.0));
  for button in [PointerButton::Back, PointerButton::Forward] {
    window.pointer_button(button, true);
    window.pointer_button(button, false);
  }
  // Released elsewhere, it isn't one.
  window.pointer_button(PointerButton::Back, true);
  window.pointer_motion(Point::new(100.0, 5.0));
  window.pointer_button(PointerButton::Back, false);
  assert_eq!(*log.borrow(), ["back", "forward"]);
}

#[test]
fn a_button_sets_its_cursor_and_a_field_keeps_the_i_beam() {
  let mut h = Harness::new();
  let shape = h.cx.signal(CursorShape::Grab);
  let row = Row::new()
    .align(Align::Start)
    .child(Button::new(square()).cursor(move || shape.get()))
    .child(Button::new(square()))
    .child(TextInput::new(h.cx.signal(String::new())));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(
    window.pointer_motion(Point::new(5.0, 5.0)),
    CursorShape::Grab
  );
  shape.set(CursorShape::NsResize);
  h.frame(&window);
  assert_eq!(window.cursor(), CursorShape::NsResize);
  assert_eq!(
    window.pointer_motion(Point::new(25.0, 5.0)),
    CursorShape::Pointer
  );
  let field = rect_at(&window, &[0, 2]);
  assert_eq!(
    window.pointer_motion(Point::new(field.x + 5.0, 5.0)),
    CursorShape::Text
  );
}
