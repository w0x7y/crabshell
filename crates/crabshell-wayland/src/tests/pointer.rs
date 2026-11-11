//! `wl_pointer` events mapped onto a window, checked without a compositor:
//! the events are built by hand and given to real `crabshell-ui` windows.

use std::cell::RefCell;
use std::rc::Rc;

use crabshell_reactive::Runtime;
use crabshell_ui::{
  Align, Button, CursorShape, Cx, Icon, Output, PanelWindow, Point, PointerButton, Row,
  ScrollDelta, Size, TextSystem, Window,
};
use smithay_client_toolkit::seat::pointer::{
  AxisScroll, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE, PointerEventKind,
};

use crate::pointer::{Input, apply, inputs};

fn press(button: u32) -> PointerEventKind {
  PointerEventKind::Press {
    time: 0,
    button,
    serial: 1,
  }
}

fn release(button: u32) -> PointerEventKind {
  PointerEventKind::Release {
    time: 0,
    button,
    serial: 2,
  }
}

fn vertical(axis: AxisScroll) -> PointerEventKind {
  PointerEventKind::Axis {
    time: 0,
    horizontal: AxisScroll::default(),
    vertical: axis,
    source: None,
  }
}

#[test]
fn positions_buttons_and_leaving() {
  let at = (12.5, 7.0);
  let point = Input::Motion(Point::new(12.5, 7.0));
  assert_eq!(inputs(&PointerEventKind::Enter { serial: 1 }, at), [point]);
  assert_eq!(inputs(&PointerEventKind::Motion { time: 0 }, at), [point]);
  assert_eq!(
    inputs(&PointerEventKind::Leave { serial: 1 }, at),
    [Input::Leave]
  );
  assert_eq!(
    inputs(&press(BTN_LEFT), at),
    [Input::Button(PointerButton::Left, true)]
  );
  assert_eq!(
    inputs(&release(BTN_RIGHT), at),
    [Input::Button(PointerButton::Right, false)]
  );
  assert_eq!(
    inputs(&press(BTN_MIDDLE), at),
    [Input::Button(PointerButton::Middle, true)]
  );
  // Back and forward buttons do nothing.
  assert_eq!(inputs(&press(BTN_SIDE), at), []);
  // A position that isn't a number is dropped.
  assert_eq!(
    inputs(&PointerEventKind::Motion { time: 0 }, (f64::NAN, 1.0)),
    []
  );
}

#[test]
fn wheels_touchpads_and_the_end_of_a_gesture() {
  let at = (0.0, 0.0);
  let wheel = AxisScroll {
    absolute: 15.0,
    discrete: 1,
    value120: 120,
    ..AxisScroll::default()
  };
  assert_eq!(
    inputs(&vertical(wheel), at),
    [Input::Scroll(ScrollDelta::Wheel(120))]
  );
  // A high-resolution wheel moves a quarter notch.
  let fine_wheel = AxisScroll {
    absolute: 3.75,
    value120: 30,
    ..AxisScroll::default()
  };
  assert_eq!(
    inputs(&vertical(fine_wheel), at),
    [Input::Scroll(ScrollDelta::Wheel(30))]
  );
  // A compositor too old for axis_value120 sends whole notches.
  let old_wheel = AxisScroll {
    absolute: -15.0,
    discrete: -2,
    ..AxisScroll::default()
  };
  assert_eq!(
    inputs(&vertical(old_wheel), at),
    [Input::Scroll(ScrollDelta::Wheel(-240))]
  );
  let touchpad = AxisScroll {
    absolute: 3.5,
    ..AxisScroll::default()
  };
  assert_eq!(
    inputs(&vertical(touchpad), at),
    [Input::Scroll(ScrollDelta::Pixels(3.5))]
  );
  let lift = AxisScroll {
    stop: true,
    ..AxisScroll::default()
  };
  assert_eq!(inputs(&vertical(lift), at), [Input::ScrollEnd]);
  let last_and_lift = AxisScroll {
    absolute: 1.0,
    stop: true,
    ..AxisScroll::default()
  };
  assert_eq!(
    inputs(&vertical(last_and_lift), at),
    [Input::Scroll(ScrollDelta::Pixels(1.0)), Input::ScrollEnd]
  );
  // Horizontal scrolling has no handler.
  let sideways = PointerEventKind::Axis {
    time: 0,
    horizontal: wheel,
    vertical: AxisScroll::default(),
    source: None,
  };
  assert_eq!(inputs(&sideways, at), []);
  let not_a_number = AxisScroll {
    absolute: f64::NAN,
    ..AxisScroll::default()
  };
  assert_eq!(inputs(&vertical(not_a_number), at), []);
}

/// A 200 by 30 window with one 20-pixel button at the left that logs its
/// clicks and scroll steps.
fn window(log: &Rc<RefCell<Vec<String>>>) -> Window {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let clicks = Rc::clone(log);
  let right_clicks = Rc::clone(log);
  let scrolls = Rc::clone(log);
  let button = Button::new(Icon::new("x").size(20))
    .on_click(move |_| clicks.borrow_mut().push(String::from("click")))
    .on_right_click(move |_| right_clicks.borrow_mut().push(String::from("right")))
    .on_scroll(move |_, steps| scrolls.borrow_mut().push(format!("scroll {steps}")));
  let panel = PanelWindow::new(&Output::new("HEADLESS-1"))
    .child(Row::new().align(Align::Start).child(button));
  let window = Window::new(&cx, panel);
  window.set_size(Size::new(200.0, 30.0));
  runtime.flush();
  window.layout(&mut TextSystem::new());
  window
}

/// Sends each event at `at` to `window`, as the platform does, and returns
/// the cursor after the last.
fn send(window: &Window, events: &[PointerEventKind], at: (f64, f64)) -> Option<CursorShape> {
  let mut cursor = None;
  for event in events {
    for input in inputs(event, at) {
      cursor = Some(apply(window, input));
    }
  }
  cursor
}

#[test]
fn a_click_reaches_the_button_under_the_pointer() {
  let log = Rc::new(RefCell::new(Vec::new()));
  let window = window(&log);
  let over = (10.0, 15.0);
  assert_eq!(
    send(&window, &[PointerEventKind::Enter { serial: 1 }], over),
    Some(CursorShape::Pointer)
  );
  send(&window, &[press(BTN_LEFT), release(BTN_LEFT)], over);
  send(&window, &[press(BTN_RIGHT), release(BTN_RIGHT)], over);
  assert_eq!(*log.borrow(), ["click", "right"]);
  // Away from the button: the default cursor, and clicks go nowhere.
  let away = (150.0, 15.0);
  assert_eq!(
    send(&window, &[PointerEventKind::Motion { time: 0 }], away),
    Some(CursorShape::Default)
  );
  send(&window, &[press(BTN_LEFT), release(BTN_LEFT)], away);
  assert_eq!(log.borrow().len(), 2);
}

#[test]
fn leaving_ends_hover_and_a_press_in_progress() {
  let log = Rc::new(RefCell::new(Vec::new()));
  let window = window(&log);
  let over = (10.0, 15.0);
  send(
    &window,
    &[PointerEventKind::Enter { serial: 1 }, press(BTN_LEFT)],
    over,
  );
  assert_eq!(
    send(&window, &[PointerEventKind::Leave { serial: 2 }], over),
    Some(CursorShape::Default)
  );
  // The release after coming back isn't a click: the press was dropped.
  send(
    &window,
    &[PointerEventKind::Enter { serial: 3 }, release(BTN_LEFT)],
    over,
  );
  assert!(log.borrow().is_empty());
}

#[test]
fn scrolling_reaches_the_button_and_a_lift_drops_the_partial_step() {
  let log = Rc::new(RefCell::new(Vec::new()));
  let window = window(&log);
  let over = (10.0, 15.0);
  send(&window, &[PointerEventKind::Enter { serial: 1 }], over);
  let notch_down = AxisScroll {
    absolute: 15.0,
    value120: 120,
    ..AxisScroll::default()
  };
  send(&window, &[vertical(notch_down)], over);
  assert_eq!(*log.borrow(), ["scroll -1"]);
  // Ten pixels, the fingers lift, ten more: no step, since the first ten
  // were dropped.
  let ten = AxisScroll {
    absolute: -10.0,
    ..AxisScroll::default()
  };
  let lift = AxisScroll {
    stop: true,
    ..AxisScroll::default()
  };
  send(
    &window,
    &[vertical(ten), vertical(lift), vertical(ten)],
    over,
  );
  assert_eq!(*log.borrow(), ["scroll -1"]);
  send(&window, &[vertical(ten)], over);
  assert_eq!(*log.borrow(), ["scroll -1", "scroll 1"]);
}

#[test]
fn a_layout_that_moves_the_button_away_changes_the_cursor() {
  use crate::pointer::Cursor;
  use crate::windows::WindowId;

  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let gap = cx.signal(0_u32);
  let panel = PanelWindow::new(&Output::new("HEADLESS-1")).child(
    Row::new()
      .align(Align::Start)
      .child(Icon::new("x").size(move || gap.get()))
      .child(Button::new(Icon::new("x").size(20))),
  );
  let window = Window::new(&cx, panel);
  let mut text = TextSystem::new();
  window.set_size(Size::new(200.0, 30.0));
  runtime.flush();
  window.layout(&mut text);

  let mut cursor = Cursor::default();
  cursor.enter(5, WindowId(0));
  let over = window.pointer_motion(Point::new(10.0, 10.0));
  assert_eq!(cursor.update(over), Some((5, CursorShape::Pointer)));
  // The button moves 100 pixels right; the pointer stays. After the
  // layout the platform asks again, and the arrow is sent.
  gap.set(100);
  runtime.flush();
  window.layout(&mut text);
  assert_eq!(
    cursor.update(window.cursor()),
    Some((5, CursorShape::Default))
  );
  assert_eq!(cursor.update(window.cursor()), None, "sent only once");
}

#[test]
fn a_cursor_is_sent_only_over_a_window_and_again_after_each_enter() {
  use crate::pointer::Cursor;
  use crate::windows::WindowId;

  let mut cursor = Cursor::default();
  assert_eq!(cursor.update(CursorShape::Pointer), None, "over no window");
  cursor.enter(1, WindowId(3));
  assert_eq!(cursor.focus(), Some(WindowId(3)));
  assert_eq!(
    cursor.update(CursorShape::Default),
    Some((1, CursorShape::Default))
  );
  assert_eq!(cursor.update(CursorShape::Default), None);
  assert_eq!(
    cursor.update(CursorShape::Pointer),
    Some((1, CursorShape::Pointer))
  );
  // Every enter needs the cursor set again, with its own serial.
  cursor.enter(9, WindowId(3));
  assert_eq!(
    cursor.update(CursorShape::Pointer),
    Some((9, CursorShape::Pointer))
  );
  cursor.leave();
  assert_eq!(cursor.focus(), None);
  assert_eq!(cursor.update(CursorShape::Default), None);
}
