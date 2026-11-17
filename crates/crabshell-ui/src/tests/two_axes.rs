//! Scroll views that scroll down and across: the wheel, the touchpad, a
//! drag and a fling on both axes, each with its own overshoot, rest and
//! scroll bar.

use std::time::{Duration, Instant};

use crabshell_reactive::Signal;

use super::{Harness, capture_logs, lift, rect_at, wheel};
use crate::{
  Align, Icon, Point, PointerButton, Row, ScrollSource, ScrollView, Style, Wheel, WheelAxis,
  Window, advance_animations, animating, next_timer,
};

/// A 50 by 50 view of a child 200 wide and 300 tall, scrolling 150 across
/// and 250 down: an icon 200 square beside an empty one 300 tall.
fn view(
  h: &mut Harness,
  (x, y): (Signal<f32>, Signal<f32>),
  tune: impl FnOnce(ScrollView) -> ScrollView,
) -> Window {
  let child = Row::new()
    .align(Align::Start)
    .child(Icon::new("x").size(200))
    .child(Icon::new("x").size(0).style(Style::new().height(300.0)));
  let view = tune(ScrollView::new(child).both(true).offset(y).offset_x(x))
    .style(Style::new().width(50.0).height(50.0));
  let row = Row::new().align(Align::Start).child(view);
  h.window(h.panel().child(row), 100.0, 100.0)
}

/// Where the child is drawn, which the offsets and overshoots move.
fn child_at(window: &Window) -> Point {
  let rect = rect_at(window, &[0, 0, 0]);
  Point::new(rect.x, rect.y)
}

/// Fingers on a touchpad moving `x` across and `y` down.
fn fingers(x: f32, y: f32) -> Wheel {
  let axis = |pixels| WheelAxis {
    pixels,
    ..WheelAxis::default()
  };
  Wheel {
    source: ScrollSource::Finger,
    x: axis(x),
    y: axis(y),
  }
}

/// Ticks the clock every 16 ms until nothing moves.
fn run_clock(h: &mut Harness, window: &Window) {
  let start = Instant::now();
  let mut ticks = 0;
  while animating() {
    assert!(ticks < 1000, "still moving after {ticks} ticks");
    advance_animations(start + Duration::from_millis(16 * ticks));
    h.frame(window);
    ticks += 1;
  }
}

#[test]
fn the_child_is_laid_out_whole_and_the_wheel_scrolls_each_way() {
  let mut h = Harness::new();
  let (x, y) = (h.cx.signal(0.0), h.cx.signal(0.0));
  let window = view(&mut h, (x, y), |view| view);
  let child = rect_at(&window, &[0, 0, 0]);
  assert_eq!((child.width, child.height), (200.0, 300.0), "never shrunk");
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll_at(wheel(120), 0);
  let tilted = Wheel {
    source: ScrollSource::WheelTilt,
    x: WheelAxis {
      pixels: 15.0,
      notches: 1.0,
      stop: false,
    },
    y: WheelAxis::default(),
  };
  window.pointer_scroll_at(tilted, 10);
  h.frame(&window);
  assert_eq!((x.get(), y.get()), (48.0, 48.0));
  assert_eq!(child_at(&window), Point::new(-48.0, -48.0));
  // Far past both ends, each stops at its own.
  x.set(1e9);
  y.set(1e9);
  h.frame(&window);
  assert_eq!(child_at(&window), Point::new(-150.0, -250.0));
}

#[test]
fn a_touchpad_moves_both_axes_and_each_springs_back_or_flings_on_its_own() {
  let mut h = Harness::new();
  let (x, y) = (h.cx.signal(0.0), h.cx.signal(0.0));
  let window = view(&mut h, (x, y), |view| view);
  window.pointer_motion(Point::new(10.0, 10.0));
  // Left past the start while going down: across it overshoots, and down
  // it moves at 500 pixels a second.
  for n in 0..5 {
    window.pointer_scroll_at(fingers(-10.0, 5.0), n * 10);
  }
  h.frame(&window);
  assert_eq!((x.get(), y.get()), (0.0, 25.0));
  assert!(child_at(&window).x > 0.0, "shown past the left end");
  window.pointer_scroll_at(lift(), 45);
  run_clock(&mut h, &window);
  assert_eq!(x.get(), 0.0);
  // At 1500 a second each second, 500 a second goes 83 more.
  assert!((y.get() - 25.0 - 83.3).abs() < 2.0, "{}", y.get());
  assert_eq!(child_at(&window), Point::new(0.0, -y.get()), "both at rest");
  assert!(next_timer().is_none());
}

#[test]
fn a_drag_moves_both_axes_and_flings_each() {
  let mut h = Harness::new();
  let (x, y) = (h.cx.signal(0.0), h.cx.signal(0.0));
  let window = view(&mut h, (x, y), |view| view);
  window.pointer_motion_at(Point::new(45.0, 45.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  for (n, at) in [35.0, 25.0, 15.0, 5.0].into_iter().enumerate() {
    window.pointer_motion_at(Point::new(at, 45.0 - (45.0 - at) / 2.0), 10 + 10 * n as u32);
  }
  assert_eq!((x.get(), y.get()), (40.0, 20.0));
  window.pointer_button_at(PointerButton::Left, false, 45);
  assert!(animating());
  run_clock(&mut h, &window);
  // 1000 pixels a second across has 110 left, and 500 down goes 83 more.
  assert_eq!(x.get(), 150.0);
  assert!((y.get() - 20.0 - 83.3).abs() < 2.0, "{}", y.get());
  assert_eq!(child_at(&window), Point::new(-150.0, -y.get()));
}

#[test]
fn a_bar_shows_along_each_axis_and_drags_its_own() {
  let mut h = Harness::new();
  let (x, y) = (h.cx.signal(0.0), h.cx.signal(0.0));
  let window = view(&mut h, (x, y), |view| view.scroll_bar(Style::new()));
  let bars: Vec<_> = rect_bars(&window);
  // Each track stops short of the other's 6 pixels in the corner, so
  // it is 44 long, and each handle is the shortest, 16.
  assert_eq!(bars.len(), 2, "{bars:?}");
  assert_eq!(bars[0], crate::Rect::new(44.0, 0.0, 6.0, 16.0));
  assert_eq!(bars[1], crate::Rect::new(0.0, 44.0, 16.0, 6.0));
  // Dragged 10 pixels right, the bottom bar's handle scrolls across by
  // 150 over its 28 pixels of room.
  window.pointer_motion(Point::new(5.0, 47.0));
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_motion(Point::new(15.0, 47.0));
  window.pointer_button_at(PointerButton::Left, false, 10);
  assert_eq!(y.get(), 0.0);
  assert!((x.get() - 10.0 * 150.0 / 28.0).abs() < 0.01, "{}", x.get());
}

/// The handles of the view's scroll bars.
fn rect_bars(window: &Window) -> Vec<crate::Rect> {
  super::node_at(window.view().root(), &[0, 0])
    .scroll_bars()
    .map(|(handle, _)| handle)
    .collect()
}

#[test]
fn turned_off_it_scrolls_down_only_and_keeps_the_horizontal_offset() {
  let mut h = Harness::new();
  let (x, y) = (h.cx.signal(30.0), h.cx.signal(0.0));
  let both = h.cx.signal(true);
  let window = view(&mut h, (x, y), |view| {
    view.both(move || both.get()).scroll_bar(Style::new())
  });
  assert_eq!(child_at(&window), Point::new(-30.0, 0.0));
  both.set(false);
  h.frame(&window);
  assert_eq!(rect_at(&window, &[0, 0, 0]).width, 50.0, "the box's width");
  assert_eq!(child_at(&window), Point::new(0.0, 0.0));
  assert_eq!(rect_bars(&window).len(), 1);
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll_at(fingers(20.0, 0.0), 0);
  window.pointer_scroll_at(lift(), 5);
  h.frame(&window);
  assert_eq!((x.get(), y.get()), (30.0, 0.0), "nothing sideways");
  assert!(!animating());
}

#[test]
fn a_panicking_both_binding_is_logged_with_its_path() {
  let mut h = Harness::new();
  let (x, y) = (h.cx.signal(0.0), h.cx.signal(0.0));
  let logs = capture_logs(|| {
    let window = view(&mut h, (x, y), |view| {
      view.both(|| -> bool { panic!("both failure") })
    });
    window.pointer_motion(Point::new(10.0, 10.0));
    window.pointer_scroll_at(wheel(120), 0);
    h.frame(&window);
  });
  assert!(
    logs.contains("PanelWindow[DP-1]/Row/ScrollView[0].both"),
    "{logs}"
  );
  assert_eq!((x.get(), y.get()), (0.0, 48.0), "it still scrolls down");
}

#[test]
fn a_drag_mostly_down_over_a_view_that_cant_go_down_moves_the_view_around_it() {
  let mut h = Harness::new();
  let (outer, x, y) = (h.cx.signal(0.0), h.cx.signal(0.0), h.cx.signal(0.0));
  // Wider than its box but no taller, so it moves only across.
  let wide = Row::new()
    .align(Align::Start)
    .child(Icon::new("x").size(200).style(Style::new().height(50.0)));
  let inner = ScrollView::new(wide)
    .both(true)
    .offset(y)
    .offset_x(x)
    .style(Style::new().width(50.0).height(50.0));
  let content = crate::Column::new()
    .child(inner)
    .child(Icon::new("x").size(200));
  let view = ScrollView::new(content)
    .offset(outer)
    .style(Style::new().width(50.0).height(50.0));
  let row = Row::new().align(Align::Start).child(view);
  let window = h.window(h.panel().child(row), 100.0, 100.0);
  window.pointer_motion_at(Point::new(40.0, 40.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_motion_at(Point::new(35.0, 10.0), 300);
  window.pointer_button_at(PointerButton::Left, false, 600);
  assert_eq!((outer.get(), x.get(), y.get()), (30.0, 0.0, 0.0));
}

#[test]
fn a_write_to_one_axis_stops_only_that_axis() {
  let mut h = Harness::new();
  let (x, y) = (h.cx.signal(0.0), h.cx.signal(0.0));
  let window = view(&mut h, (x, y), |view| view);
  window.pointer_motion(Point::new(10.0, 10.0));
  // Past the left end across, and down at 1000 pixels a second.
  for n in 0..5 {
    window.pointer_scroll_at(fingers(-10.0, 10.0), n * 10);
  }
  window.pointer_scroll_at(lift(), 45);
  let start = Instant::now();
  for tick in 0..2 {
    advance_animations(start + Duration::from_millis(16 * tick));
    h.frame(&window);
  }
  let springing = child_at(&window).x;
  assert!(springing > 0.0, "{springing}");
  y.set(10.0);
  advance_animations(start + Duration::from_millis(32));
  h.frame(&window);
  let now = child_at(&window).x;
  assert!(
    now > 0.0 && now < springing,
    "x springs on from {springing}: {now}"
  );
  run_clock(&mut h, &window);
  assert_eq!((x.get(), y.get()), (0.0, 10.0));
  assert_eq!(child_at(&window), Point::new(0.0, -10.0));
}
