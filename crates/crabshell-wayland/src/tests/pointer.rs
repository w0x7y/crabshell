//! `wl_pointer` events mapped onto a window, checked without a compositor:
//! the events are built by hand and given to real `crabshell-ui` windows.

use std::cell::RefCell;
use std::rc::Rc;

use crabshell_reactive::Runtime;
use crabshell_ui::{
  Align, Button, CursorShape, Cx, Icon, Output, PanelWindow, Point, PointerButton, Row,
  ScrollSource, Size, TextSystem, Wheel, WheelAxis, Window,
};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::AxisSource;
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use smithay_client_toolkit::seat::pointer::{
  AxisScroll, BTN_BACK, BTN_EXTRA, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE,
  BTN_TASK, PointerEventKind,
};

use crate::pointer::{Input, Touch, apply, inputs, shape};
use crate::windows::WindowId;

fn press(button: u32) -> PointerEventKind {
  PointerEventKind::Press {
    time: 7,
    button,
    serial: 1,
  }
}

fn release(button: u32) -> PointerEventKind {
  PointerEventKind::Release {
    time: 9,
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

/// What a mouse wheel turned on the vertical axis gives.
fn notches(pixels: f32, notches: f32) -> Input {
  Input::Scroll(Wheel {
    source: ScrollSource::Wheel,
    x: WheelAxis::default(),
    y: WheelAxis {
      pixels,
      notches,
      stop: false,
    },
  })
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
    [Input::Button(PointerButton::Left, true, 7)]
  );
  assert_eq!(
    inputs(&release(BTN_RIGHT), at),
    [Input::Button(PointerButton::Right, false, 9)]
  );
  assert_eq!(
    inputs(&press(BTN_MIDDLE), at),
    [Input::Button(PointerButton::Middle, true, 7)]
  );
  // Both names Linux has for each side button.
  for back in [BTN_SIDE, BTN_BACK] {
    assert_eq!(
      inputs(&press(back), at),
      [Input::Button(PointerButton::Back, true, 7)]
    );
  }
  for forward in [BTN_EXTRA, BTN_FORWARD] {
    assert_eq!(
      inputs(&release(forward), at),
      [Input::Button(PointerButton::Forward, false, 9)]
    );
  }
  assert_eq!(inputs(&press(BTN_TASK), at), []);
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
  assert_eq!(inputs(&vertical(wheel), at), [notches(15.0, 1.0)]);
  // A high-resolution wheel moves a quarter notch.
  let fine_wheel = AxisScroll {
    absolute: 3.75,
    value120: 30,
    ..AxisScroll::default()
  };
  assert_eq!(inputs(&vertical(fine_wheel), at), [notches(3.75, 0.25)]);
  // A compositor too old for axis_value120 sends whole notches.
  let old_wheel = AxisScroll {
    absolute: -15.0,
    discrete: -2,
    ..AxisScroll::default()
  };
  assert_eq!(inputs(&vertical(old_wheel), at), [notches(-15.0, -2.0)]);
  // Fingers, said so, then lifting with the last pixels.
  let fingers = |vertical, horizontal| PointerEventKind::Axis {
    time: 0,
    horizontal,
    vertical,
    source: Some(AxisSource::Finger),
  };
  let last_and_lift = AxisScroll {
    absolute: 1.0,
    stop: true,
    ..AxisScroll::default()
  };
  let sideways = AxisScroll {
    absolute: -4.5,
    stop: true,
    ..AxisScroll::default()
  };
  assert_eq!(
    inputs(&fingers(last_and_lift, sideways), at),
    [Input::Scroll(Wheel {
      source: ScrollSource::Finger,
      x: WheelAxis {
        pixels: -4.5,
        notches: 0.0,
        stop: true,
      },
      y: WheelAxis {
        pixels: 1.0,
        notches: 0.0,
        stop: true,
      },
    })]
  );
  // Pixels with no source count as continuous.
  let touchpad = AxisScroll {
    absolute: 3.5,
    ..AxisScroll::default()
  };
  let Input::Scroll(scrolled) = inputs(&vertical(touchpad), at)[0] else {
    panic!("scrolling");
  };
  assert_eq!(scrolled.source, ScrollSource::Continuous);
  // A tilted wheel scrolls sideways.
  let tilt = PointerEventKind::Axis {
    time: 0,
    horizontal: wheel,
    vertical: AxisScroll::default(),
    source: Some(AxisSource::WheelTilt),
  };
  let Input::Scroll(tilted) = inputs(&tilt, at)[0] else {
    panic!("scrolling");
  };
  assert_eq!(
    (tilted.source, tilted.x.notches),
    (ScrollSource::WheelTilt, 1.0)
  );
  // Nothing, and numbers that aren't, make no input.
  assert_eq!(inputs(&vertical(AxisScroll::default()), at), []);
  let not_a_number = AxisScroll {
    absolute: f64::NAN,
    ..AxisScroll::default()
  };
  assert_eq!(inputs(&vertical(not_a_number), at), []);
}

#[test]
fn every_cursor_shape_has_one_and_version_1_falls_back() {
  assert_eq!(shape(CursorShape::Default, 1), Shape::Default);
  assert_eq!(shape(CursorShape::Grabbing, 1), Shape::Grabbing);
  assert_eq!(shape(CursorShape::ZoomOut, 1), Shape::ZoomOut);
  assert_eq!(shape(CursorShape::DndAsk, 2), Shape::DndAsk);
  assert_eq!(shape(CursorShape::AllResize, 2), Shape::AllResize);
  assert_eq!(shape(CursorShape::DndAsk, 1), Shape::ContextMenu);
  assert_eq!(shape(CursorShape::AllResize, 1), Shape::Move);
}

#[test]
fn one_finger_is_a_pointer_that_taps_and_drags() {
  let mut touch = Touch::default();
  let window = WindowId(4);
  assert_eq!(touch.window(), None);
  assert_eq!(
    touch.down(1, window, (10.0, 5.0), 100),
    [
      Input::Motion(Point::new(10.0, 5.0)),
      Input::Button(PointerButton::Left, true, 100),
    ]
  );
  assert_eq!(touch.window(), Some(window));
  // A second finger is ignored, down, moving and up.
  assert_eq!(touch.down(2, WindowId(5), (50.0, 5.0), 110), []);
  assert_eq!(touch.motion(2, (60.0, 5.0)), []);
  assert_eq!(touch.up(2, 120), []);
  assert_eq!(
    touch.motion(1, (30.0, 6.0)),
    [Input::Motion(Point::new(30.0, 6.0))]
  );
  assert_eq!(touch.motion(1, (f64::NAN, 6.0)), []);
  assert_eq!(
    touch.up(1, 200),
    [Input::Button(PointerButton::Left, false, 200), Input::Leave]
  );
  assert_eq!(touch.window(), None);
  // A down at a point that isn't a number starts nothing.
  assert_eq!(touch.down(3, window, (f64::INFINITY, 0.0), 300), []);
  assert_eq!(touch.window(), None);
  // A cancel leaves, with no release, so nothing clicks.
  touch.down(4, window, (1.0, 1.0), 400);
  assert_eq!(touch.cancel(), [Input::Leave]);
  assert_eq!(touch.cancel(), []);
  assert_eq!(touch.up(4, 500), []);
}

/// A 200 by 30 window with one 20-pixel button at the left that logs its
/// clicks and scroll steps.
fn window(log: &Rc<RefCell<Vec<String>>>) -> Window {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let clicks = Rc::clone(log);
  let right_clicks = Rc::clone(log);
  let scrolls = Rc::clone(log);
  let (backs, forwards) = (Rc::clone(log), Rc::clone(log));
  let button = Button::new(Icon::new("x").size(20))
    .on_click(move |_| clicks.borrow_mut().push(String::from("click")))
    .on_right_click(move |_| right_clicks.borrow_mut().push(String::from("right")))
    .on_back(move |_| backs.borrow_mut().push(String::from("back")))
    .on_forward(move |_| forwards.borrow_mut().push(String::from("forward")))
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
  send(&window, &[press(BTN_SIDE), release(BTN_SIDE)], over);
  send(&window, &[press(BTN_EXTRA), release(BTN_EXTRA)], over);
  assert_eq!(*log.borrow(), ["click", "right", "back", "forward"]);
  // Away from the button: the default cursor, and clicks go nowhere.
  let away = (150.0, 15.0);
  assert_eq!(
    send(&window, &[PointerEventKind::Motion { time: 0 }], away),
    Some(CursorShape::Default)
  );
  send(&window, &[press(BTN_LEFT), release(BTN_LEFT)], away);
  assert_eq!(log.borrow().len(), 4);
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
fn a_tap_clicks_and_a_cancelled_touch_doesnt() {
  let log = Rc::new(RefCell::new(Vec::new()));
  let window = window(&log);
  let mut touch = Touch::default();
  let id = WindowId(0);
  let tap = [touch.down(1, id, (10.0, 15.0), 100), touch.up(1, 150)];
  for input in tap.concat() {
    apply(&window, input);
  }
  assert_eq!(*log.borrow(), ["click"]);
  // The tap left: nothing is hovered once the finger lifts.
  assert_eq!(window.cursor(), CursorShape::Default);
  let cancelled = [touch.down(2, id, (10.0, 15.0), 300), touch.cancel()];
  for input in cancelled.concat() {
    apply(&window, input);
  }
  assert_eq!(*log.borrow(), ["click"]);
}

#[test]
fn a_layout_that_moves_the_button_away_changes_the_cursor() {
  use crate::pointer::Cursor;

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
