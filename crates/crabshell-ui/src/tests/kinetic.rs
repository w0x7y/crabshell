//! Kinetic scrolling and overshoot: touchpad flings, dragging the child,
//! springing back from past an end, and resting asleep.

use std::time::{Duration, Instant};

use crabshell_reactive::Signal;

use super::{Counter, Harness, capture_logs, lift, pixels, rect_at, wheel};
use crate::{
  Align, Button, Column, Icon, Point, PointerButton, Rect, Row, ScrollSource, ScrollView, Style,
  Wheel, WheelAxis, Window, advance_animations, animating, fire_timers, next_timer,
};

/// A 20 by 50 view of 30 icons of 20 pixels: 600 pixels, scrolling 550.
/// Each icon is a button that counts its clicks.
fn view(
  h: &mut Harness,
  offset: Signal<f32>,
  tune: fn(ScrollView) -> ScrollView,
) -> (Window, Counter) {
  let clicks = Counter::default();
  let column = (0..30).fold(Column::new(), |column, _| {
    let c = clicks.clone();
    column.child(Button::new(Icon::new("x").size(20)).on_click(move |_| c.hit()))
  });
  let view =
    tune(ScrollView::new(column).offset(offset)).style(Style::new().width(20.0).height(50.0));
  let row = Row::new().align(Align::Start).child(view);
  (h.window(h.panel().child(row), 100.0, 100.0), clicks)
}

fn plain(view: ScrollView) -> ScrollView {
  view
}

/// Where the first icon is drawn, which an overshoot moves.
fn first_y(window: &Window) -> f32 {
  rect_at(window, &[0, 0, 0, 0]).y
}

/// Five touchpad moves of `step` pixels, 10 ms apart from `time`, then the
/// fingers lift 5 ms after the last: `step` × 100 pixels a second.
fn swipe(window: &Window, step: f32, time: u32) {
  for n in 0..5 {
    window.pointer_scroll_at(pixels(step), time + n * 10);
  }
  window.pointer_scroll_at(lift(), time + 45);
}

/// Ticks the clock every 16 ms from `start`, a frame after each, until
/// nothing moves; returns how many ticks ran.
fn run_clock(h: &mut Harness, window: &Window, start: Instant) -> u32 {
  let mut ticks = 0;
  while animating() {
    assert!(ticks < 1000, "still moving after {ticks} ticks");
    advance_animations(start + Duration::from_millis(16 * u64::from(ticks)));
    h.frame(window);
    ticks += 1;
  }
  ticks
}

#[test]
fn a_touchpad_fling_slows_down_rests_and_then_wakes_nothing() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let (window, _) = view(&mut h, offset, plain);
  window.pointer_motion(Point::new(10.0, 10.0));
  swipe(&window, 10.0, 0);
  assert_eq!(offset.get(), 50.0);
  assert!(animating(), "it flings at 1000 pixels a second");
  let ticks = run_clock(&mut h, &window, Instant::now());
  // At 1500 a second each second, 1000 a second goes 333 more over 0.67 s.
  let gone = offset.get() - 50.0;
  assert!((gone - 1000.0 * 1000.0 / 3000.0).abs() < 2.0, "{gone}");
  assert!((40..=46).contains(&ticks), "{ticks} ticks");
  assert!(
    !animating() && next_timer().is_none(),
    "nothing left to wake"
  );
  assert!(!window.needs_layout());
}

#[test]
fn a_slow_lift_a_wheel_or_a_view_not_kinetic_doesnt_fling() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let (window, _) = view(&mut h, offset, plain);
  window.pointer_motion(Point::new(10.0, 10.0));
  for n in 0..5 {
    window.pointer_scroll_at(pixels(10.0), n * 10);
  }
  // The fingers rested 200 ms before lifting.
  window.pointer_scroll_at(lift(), 240);
  assert!(!animating());
  window.pointer_scroll_at(wheel(120), 300);
  window.pointer_scroll_at(wheel(120), 310);
  assert_eq!(offset.get(), 146.0);
  assert!(!animating());

  let other = h.cx.signal(0.0);
  let (window, _) = view(&mut h, other, |view| view.kinetic(false));
  window.pointer_motion(Point::new(10.0, 10.0));
  swipe(&window, 10.0, 0);
  assert_eq!(other.get(), 50.0);
  assert!(!animating());
}

#[test]
fn a_fling_stops_at_once_for_a_press_a_wheel_or_a_write() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let (window, clicks) = view(&mut h, offset, plain);
  window.pointer_motion(Point::new(10.0, 10.0));
  let start = Instant::now();
  let fling = |h: &mut Harness, time: u32| {
    swipe(&window, 10.0, time);
    for tick in 0..4 {
      advance_animations(start + Duration::from_millis(u64::from(time) + 16 * tick));
      h.frame(&window);
    }
  };
  fling(&mut h, 0);
  let stopped = offset.get();
  window.pointer_button_at(PointerButton::Left, true, 100);
  advance_animations(start + Duration::from_millis(200));
  h.frame(&window);
  assert_eq!(offset.get(), stopped, "the press stopped it");
  assert!(!animating());
  window.pointer_button_at(PointerButton::Left, false, 150);
  assert_eq!(clicks.get(), 1, "a tap that stops a fling still clicks");

  fling(&mut h, 1000);
  let stopped = offset.get();
  window.pointer_scroll_at(wheel(120), 1100);
  advance_animations(start + Duration::from_millis(1200));
  h.frame(&window);
  assert_eq!(
    offset.get(),
    stopped + 48.0,
    "the notch moved it, and nothing else did"
  );
  assert!(!animating());

  fling(&mut h, 2000);
  offset.set(5.0);
  advance_animations(start + Duration::from_millis(2200));
  h.frame(&window);
  assert_eq!(offset.get(), 5.0);
  assert!(!animating());
}

#[test]
fn a_touchpad_past_an_end_overshoots_with_resistance_and_springs_back_without_layouts() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let (window, _) = view(&mut h, offset, plain);
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll_at(pixels(-10.0), 0);
  h.frame(&window);
  assert_eq!(offset.get(), 0.0, "the offset stays within the content");
  // A quarter of the box is the limit, 12.5, and the pull halves a move
  // at first.
  let first = first_y(&window);
  assert!(
    (first - 12.5 * (1.0 - (-0.4_f32).exp())).abs() < 0.01,
    "{first}"
  );
  window.pointer_scroll_at(pixels(-9_990.0), 10);
  h.frame(&window);
  assert!(
    (12.4..=12.5).contains(&first_y(&window)),
    "{}",
    first_y(&window)
  );
  // Back the other way it comes in before the offset moves.
  window.pointer_scroll_at(pixels(10_004.0), 20);
  h.frame(&window);
  assert_eq!((offset.get(), first_y(&window)), (4.0, -4.0));
  window.pointer_scroll_at(pixels(-30.0), 30);
  h.frame(&window);
  assert_eq!(offset.get(), 0.0);
  window.pointer_scroll_at(lift(), 500);
  let start = Instant::now();
  let mut ticks = 0;
  while animating() {
    advance_animations(start + Duration::from_millis(16 * ticks));
    h.runtime.flush();
    assert!(
      !window.tree.borrow().needs_layout,
      "springing back only places the window"
    );
    if window.needs_layout() {
      window.layout(&mut h.text);
    }
    ticks += 1;
    assert!(ticks < 200);
  }
  assert_eq!(first_y(&window), 0.0, "back at the start");
  assert!(next_timer().is_none());
}

#[test]
fn a_fling_past_the_end_goes_a_bounded_way_and_springs_back() {
  let mut h = Harness::new();
  let offset = h.cx.signal(300.0);
  let (window, _) = view(&mut h, offset, plain);
  window.pointer_motion(Point::new(10.0, 10.0));
  // 2500 a second would go 2083 pixels; it has 125 left.
  swipe(&window, 25.0, 0);
  assert_eq!(offset.get(), 425.0);
  let start = Instant::now();
  let mut furthest: f32 = 0.0;
  let mut ticks = 0;
  while animating() {
    advance_animations(start + Duration::from_millis(16 * ticks));
    h.frame(&window);
    furthest = furthest.min(first_y(&window) + 550.0);
    ticks += 1;
    assert!(ticks < 300);
  }
  assert_eq!(offset.get(), 550.0);
  assert!((-12.5..-1.0).contains(&furthest), "{furthest}");
  assert_eq!(first_y(&window), -550.0, "back at the end");
}

#[test]
fn without_overshoot_a_view_stops_at_its_ends() {
  let mut h = Harness::new();
  let offset = h.cx.signal(300.0);
  let overshoot = h.cx.signal(false);
  let window = view_with(&mut h, offset, overshoot);
  window.pointer_motion(Point::new(10.0, 10.0));
  swipe(&window, 25.0, 0);
  run_clock(&mut h, &window, Instant::now());
  assert_eq!((offset.get(), first_y(&window)), (550.0, -550.0));
  offset.set(540.0);
  h.frame(&window);
  window.pointer_scroll_at(pixels(30.0), 1000);
  h.frame(&window);
  assert_eq!((offset.get(), first_y(&window)), (550.0, -550.0));
  window.pointer_scroll_at(lift(), 1005);
  // Turned off while springing back, it drops the overshoot at once.
  overshoot.set(true);
  h.frame(&window);
  window.pointer_scroll_at(pixels(30.0), 2000);
  window.pointer_scroll_at(lift(), 2500);
  h.frame(&window);
  assert!(first_y(&window) < -550.0);
  advance_animations(Instant::now());
  overshoot.set(false);
  h.frame(&window);
  advance_animations(Instant::now() + Duration::from_millis(16));
  h.frame(&window);
  assert_eq!(first_y(&window), -550.0);
  assert!(!animating());
}

/// [`view`], without buttons, with its overshoot bound to `overshoot`.
fn view_with(h: &mut Harness, offset: Signal<f32>, overshoot: Signal<bool>) -> Window {
  let column = (0..30).fold(Column::new(), |column, _| {
    column.child(Icon::new("x").size(20))
  });
  let view = ScrollView::new(column)
    .offset(offset)
    .overshoot(move || overshoot.get())
    .style(Style::new().width(20.0).height(50.0));
  let row = Row::new().align(Align::Start).child(view);
  h.window(h.panel().child(row), 100.0, 100.0)
}

#[test]
fn dragging_the_child_follows_the_pointer_cancels_the_click_and_flings() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let (window, clicks) = view(&mut h, offset, plain);
  window.pointer_motion_at(Point::new(10.0, 45.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_motion_at(Point::new(10.0, 38.0), 5);
  assert_eq!(offset.get(), 0.0, "within the threshold");
  // Past it, the child moves with the pointer from the press.
  for (n, y) in [30.0, 20.0, 10.0, 0.0].into_iter().enumerate() {
    window.pointer_motion_at(Point::new(10.0, y), 10 + 10 * n as u32);
  }
  assert_eq!(offset.get(), 45.0);
  window.pointer_button_at(PointerButton::Left, false, 45);
  assert_eq!(clicks.get(), 0, "a drag isn't a click");
  assert!(animating(), "it flings at 1000 pixels a second");
  run_clock(&mut h, &window, Instant::now());
  assert!(
    (offset.get() - 45.0 - 333.3).abs() < 2.0,
    "{}",
    offset.get()
  );
  // A drag that comes back over the button it began on doesn't click it.
  let at = offset.get();
  window.pointer_motion_at(Point::new(10.0, 45.0), 1000);
  window.pointer_button_at(PointerButton::Left, true, 1000);
  window.pointer_motion_at(Point::new(10.0, 25.0), 1100);
  window.pointer_motion_at(Point::new(10.0, 45.0), 1200);
  h.frame(&window);
  window.pointer_button_at(PointerButton::Left, false, 1500);
  assert_eq!((offset.get(), clicks.get()), (at, 0));
  // A tap still clicks.
  window.pointer_button_at(PointerButton::Left, true, 2000);
  window.pointer_button_at(PointerButton::Left, false, 2050);
  assert_eq!(clicks.get(), 1);
}

#[test]
fn a_drag_past_the_start_overshoots_and_springs_back_once_let_go() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let (window, _) = view(&mut h, offset, plain);
  window.pointer_motion_at(Point::new(10.0, 5.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_motion_at(Point::new(10.0, 45.0), 500);
  h.frame(&window);
  assert_eq!(offset.get(), 0.0);
  assert!(first_y(&window) > 5.0, "{}", first_y(&window));
  window.pointer_button_at(PointerButton::Left, false, 1000);
  run_clock(&mut h, &window, Instant::now());
  assert_eq!(first_y(&window), 0.0);
}

#[test]
fn a_button_that_drags_keeps_its_drag_and_the_innermost_view_along_a_drag_takes_it() {
  let mut h = Harness::new();
  let (outer, inner) = (h.cx.signal(0.0), h.cx.signal(0.0));
  let drags = Counter::default();
  let d = drags.clone();
  let across = (0..10).fold(Row::new(), |row, _| row.child(Icon::new("x").size(20)));
  let content = Column::new()
    .child(Button::new(Icon::new("x").size(20)).on_drag(move |_, _| d.hit()))
    .child(
      ScrollView::new(across)
        .horizontal(true)
        .offset(inner)
        .style(Style::new().height(20.0)),
    )
    .child(Icon::new("x").size(200));
  let view = ScrollView::new(content)
    .offset(outer)
    .style(Style::new().width(50.0).height(50.0));
  let row = Row::new().align(Align::Start).child(view);
  let window = h.window(h.panel().child(row), 100.0, 100.0);
  let drag = |from: Point, to: Point, time: u32| {
    window.pointer_motion_at(from, time);
    window.pointer_button_at(PointerButton::Left, true, time);
    window.pointer_motion_at(to, time + 300);
    window.pointer_button_at(PointerButton::Left, false, time + 600);
  };
  drag(Point::new(10.0, 10.0), Point::new(10.0, -20.0), 0);
  assert!(drags.get() > 0);
  assert_eq!(outer.get(), 0.0, "the button's own drag");
  drag(Point::new(40.0, 30.0), Point::new(10.0, 30.0), 1000);
  assert_eq!(
    (inner.get(), outer.get()),
    (30.0, 0.0),
    "sideways, the inner view"
  );
  // Mostly up, a little sideways: up, so the outer one.
  drag(Point::new(40.0, 30.0), Point::new(35.0, 10.0), 2000);
  assert_eq!(
    (inner.get(), outer.get()),
    (30.0, 20.0),
    "up and down, the outer one"
  );
  assert!(!animating());
}

#[test]
fn a_horizontal_touchpad_fling_moves_sideways() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let across = (0..30).fold(Row::new(), |row, _| row.child(Icon::new("x").size(20)));
  let view = ScrollView::new(across)
    .horizontal(true)
    .offset(offset)
    .style(Style::new().width(50.0).height(20.0));
  let row = Row::new().align(Align::Start).child(view);
  let window = h.window(h.panel().child(row), 100.0, 100.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  let side = |pixels: f32, stop: bool| Wheel {
    source: ScrollSource::Finger,
    x: WheelAxis {
      pixels,
      notches: 0.0,
      stop,
    },
    y: WheelAxis::default(),
  };
  for n in 0..5 {
    window.pointer_scroll_at(side(10.0, false), n * 10);
  }
  window.pointer_scroll_at(side(0.0, true), 45);
  run_clock(&mut h, &window, Instant::now());
  assert!((offset.get() - 383.3).abs() < 2.0, "{}", offset.get());
}

#[test]
fn a_view_removed_or_a_window_dropped_mid_fling_stops_and_frees_it() {
  let mut h = Harness::new();
  let before = h.runtime.node_count();
  let offset = h.cx.signal(0.0);
  let shown = h.cx.signal(true);
  let list = Column::new().children_keyed(
    move || if shown.get() { vec![()] } else { Vec::new() },
    |_| (),
    move |_, _| {
      let column = (0..30).fold(Column::new(), |column, _| {
        column.child(Icon::new("x").size(20))
      });
      ScrollView::new(column)
        .offset(offset)
        .style(Style::new().width(20.0).height(50.0))
    },
  );
  let window = h.window(h.panel().child(list), 100.0, 100.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  swipe(&window, 10.0, 0);
  let start = Instant::now();
  assert!(advance_animations(start));
  shown.set(false);
  h.frame(&window);
  assert!(
    !advance_animations(start + Duration::from_millis(16)),
    "its view went"
  );

  shown.set(true);
  h.frame(&window);
  window.pointer_motion(Point::new(10.0, 10.0));
  swipe(&window, 10.0, 1000);
  assert!(advance_animations(start + Duration::from_millis(32)));
  drop(window);
  assert!(
    !advance_animations(start + Duration::from_millis(48)),
    "its window went"
  );
  assert_eq!(
    h.runtime.node_count(),
    before + 2,
    "only the test's signals stay"
  );
}

#[test]
fn a_panicking_kinetic_or_overshoot_binding_is_logged_with_its_path() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let logs = capture_logs(|| {
    let (window, _) = view(&mut h, offset, |view| {
      view
        .kinetic(|| -> bool { panic!("kinetic failure") })
        .overshoot(|| -> bool { panic!("overshoot failure") })
        .mouse_drag(|| -> bool { panic!("mouse_drag failure") })
    });
    window.pointer_motion(Point::new(10.0, 10.0));
    swipe(&window, 10.0, 0);
    run_clock(&mut h, &window, Instant::now());
  });
  assert!(
    logs.contains("PanelWindow[DP-1]/Row/ScrollView[0].kinetic"),
    "{logs}"
  );
  assert!(
    logs.contains("PanelWindow[DP-1]/Row/ScrollView[0].overshoot"),
    "{logs}"
  );
  assert!(
    logs.contains("PanelWindow[DP-1]/Row/ScrollView[0].mouse_drag"),
    "{logs}"
  );
  assert!(offset.get() > 50.0, "the defaults hold and it still flings");
}

#[test]
fn leaving_mid_gesture_or_mid_drag_springs_back() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let (window, _) = view(&mut h, offset, plain);
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll_at(pixels(-30.0), 0);
  window.pointer_leave();
  assert!(animating(), "the touchpad's stop went elsewhere");
  run_clock(&mut h, &window, Instant::now());
  assert_eq!(first_y(&window), 0.0);

  window.pointer_motion_at(Point::new(10.0, 5.0), 1000);
  window.pointer_button_at(PointerButton::Left, true, 1000);
  window.pointer_motion_at(Point::new(10.0, 45.0), 1100);
  window.pointer_leave();
  assert!(animating());
  run_clock(&mut h, &window, Instant::now());
  assert_eq!(first_y(&window), 0.0);
}

#[test]
fn a_list_flings_and_passes_kinetic_and_overshoot_on() {
  let mut h = Harness::new();
  let flings = h.cx.signal(true);
  let (a, b) = (h.cx.signal(0.0), h.cx.signal(0.0));
  let list = |offset: Signal<f32>, on: bool| {
    crate::ListView::new(
      || (0..100).collect::<Vec<u32>>(),
      |n| *n,
      |_, _| Icon::new("x").size(20),
    )
    .row_height(20)
    .height(50)
    .offset(offset)
    .kinetic(move || on && flings.get())
    .overshoot(on)
  };
  let row = Row::new()
    .align(Align::Start)
    .child(list(a, true))
    .child(list(b, false));
  let window = h.window(h.panel().child(row), 400.0, 100.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  swipe(&window, 10.0, 0);
  run_clock(&mut h, &window, Instant::now());
  assert!((a.get() - 383.3).abs() < 2.0, "{}", a.get());
  window.pointer_motion(Point::new(30.0, 10.0));
  swipe(&window, 10.0, 1000);
  assert!(!animating());
  assert_eq!(b.get(), 50.0);
  window.pointer_scroll_at(pixels(-1000.0), 2000);
  h.frame(&window);
  assert_eq!(b.get(), 0.0, "no overshoot to come back from");
  window.pointer_scroll_at(lift(), 2005);
  assert!(!animating());
}

#[test]
fn a_button_carrying_drag_data_drags_it_instead_of_the_view() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let dropped = h.cx.signal(Vec::<u32>::new());
  let content = Column::new()
    .child(Button::new(Icon::new("x").size(20)).drag_data(Some(7_u32)))
    .child(
      Button::new(Icon::new("x").size(20))
        .on_drop(move |_, value: u32| dropped.update(|all| all.push(value))),
    )
    .child(Icon::new("x").size(200));
  let view = ScrollView::new(content)
    .offset(offset)
    .style(Style::new().width(20.0).height(50.0));
  let row = Row::new().align(Align::Start).child(view);
  let window = h.window(h.panel().child(row), 100.0, 100.0);
  window.pointer_motion_at(Point::new(10.0, 5.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_motion_at(Point::new(10.0, 30.0), 100);
  h.frame(&window);
  assert_eq!(
    rect_at(&window, &[0, 0, 0, 1]).y,
    20.0,
    "the view stays put"
  );
  window.pointer_button_at(PointerButton::Left, false, 200);
  assert_eq!((offset.get(), dropped.get()), (0.0, vec![7]));
}

#[test]
fn a_button_that_takes_the_touchpad_stop_still_lets_the_gesture_go() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let wheels = Counter::default();
  let w = wheels.clone();
  // 60 pixels in a 50-pixel view: it scrolls 10, then past the end.
  let content = Column::new()
    .child(Icon::new("x").size(40))
    .child(Button::new(Icon::new("x").size(20)).on_wheel(move |_, _| w.hit()));
  let view = ScrollView::new(content)
    .offset(offset)
    .style(Style::new().width(20.0).height(50.0));
  let row = Row::new().align(Align::Start).child(view);
  let window = h.window(h.panel().child(row), 100.0, 100.0);
  window.pointer_motion(Point::new(10.0, 35.0));
  window.pointer_scroll_at(pixels(30.0), 0);
  h.frame(&window);
  assert_eq!(offset.get(), 10.0);
  let button = || rect_at(&window, &[0, 0, 0, 1]).y;
  assert!(button() < 35.0 - 10.0, "past the end, under the pointer");
  // The content moved the button under the still pointer, so it takes
  // the stop; the view still springs back.
  window.pointer_scroll_at(lift(), 300);
  assert_eq!(wheels.get(), 1);
  run_clock(&mut h, &window, Instant::now());
  assert_eq!(button(), 30.0);
}

#[test]
fn a_press_on_the_bar_stops_a_fling_and_its_drag_starts_from_there() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let (window, _) = view(&mut h, offset, |view| view.scroll_bar(Style::new()));
  window.pointer_motion(Point::new(10.0, 10.0));
  swipe(&window, 10.0, 0);
  let start = Instant::now();
  for tick in 0..4 {
    advance_animations(start + Duration::from_millis(16 * tick));
    h.frame(&window);
  }
  let stopped = offset.get();
  // The 20 by 50 view of 600 has a 16-pixel handle, at 50 + 34 × the
  // offset's fraction of 550.
  let handle = 34.0 * stopped / 550.0 + 8.0;
  window.pointer_motion(Point::new(17.0, handle));
  window.pointer_button_at(PointerButton::Left, true, 100);
  advance_animations(start + Duration::from_millis(200));
  h.frame(&window);
  assert_eq!(offset.get(), stopped, "the press on the handle stopped it");
  assert!(!animating());
  window.pointer_motion(Point::new(17.0, handle + 1.0));
  let moved = offset.get() - stopped;
  assert!((moved - 550.0 / 34.0).abs() < 0.01, "{moved}");
  window.pointer_button_at(PointerButton::Left, false, 300);
  // Held past the end, the bar's release lets it spring back.
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll_at(pixels(10_000.0), 400);
  h.frame(&window);
  assert!(first_y(&window) < -550.0);
  window.pointer_motion(Point::new(17.0, 42.0));
  window.pointer_button_at(PointerButton::Left, true, 500);
  window.pointer_button_at(PointerButton::Left, false, 550);
  run_clock(&mut h, &window, Instant::now());
  assert_eq!(first_y(&window), -550.0);
}

#[test]
fn a_notch_or_a_fling_only_places_the_window_and_keeps_geometry_and_hover_right() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let geometry = h.cx.signal(Rect::default());
  let hovered = h.cx.signal(false);
  let column = (0..30).fold(Column::new(), |column, n| {
    let icon = Icon::new("x").size(20);
    let icon = if n == 5 {
      icon.geometry(geometry)
    } else {
      icon
    };
    let button = Button::new(icon);
    column.child(if n == 3 {
      button.hovered(hovered)
    } else {
      button
    })
  });
  let view = ScrollView::new(column)
    .offset(offset)
    .style(Style::new().width(20.0).height(50.0));
  let row = Row::new().align(Align::Start).child(view);
  let window = h.window(h.panel().child(row), 100.0, 100.0);
  assert_eq!(geometry.get().y, 100.0);
  window.pointer_motion(Point::new(10.0, 20.0));
  // 48 pixels up, the fourth button, from 60 to 80, is under the pointer.
  window.pointer_scroll_at(wheel(120), 0);
  h.runtime.flush();
  assert!(!window.tree.borrow().needs_layout, "a notch only places it");
  h.frame(&window);
  assert_eq!(geometry.get().y, 52.0);
  assert!(hovered.get(), "the button scrolled under the pointer");
  swipe(&window, 10.0, 100);
  let start = Instant::now();
  let mut ticks = 0;
  while animating() {
    advance_animations(start + Duration::from_millis(16 * ticks));
    h.runtime.flush();
    assert!(!window.tree.borrow().needs_layout, "a fling only places it");
    h.frame(&window);
    let y = geometry.get().y;
    assert!((y - (100.0 - offset.get())).abs() < 1e-3, "{y}");
    ticks += 1;
    assert!(ticks < 200);
  }
  assert!(offset.get() > 400.0, "{}", offset.get());
  assert!(!hovered.get(), "it moved on from under the pointer");
}

#[test]
fn without_mouse_drag_a_mouse_moved_over_a_button_clicks_and_a_finger_still_drags() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let (window, clicks) = view(&mut h, offset, |view| view.mouse_drag(false));
  // 12 pixels up, past the threshold, still over the first button.
  window.pointer_motion_at(Point::new(10.0, 15.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_motion_at(Point::new(10.0, 3.0), 100);
  window.pointer_button_at(PointerButton::Left, false, 200);
  assert_eq!((offset.get(), clicks.get()), (0.0, 1));
  window.pointer_motion_at(Point::new(10.0, 45.0), 1000);
  window.touch_at(true, 1000);
  window.pointer_motion_at(Point::new(10.0, 25.0), 1100);
  window.touch_at(false, 1400);
  assert_eq!((offset.get(), clicks.get()), (20.0, 1), "a finger drags");
  assert!(!animating());
}

#[test]
fn a_list_or_a_grid_without_mouse_drag_clicks_a_row_the_mouse_moved_along() {
  let mut h = Harness::new();
  let clicks = Counter::default();
  let (a, b) = (h.cx.signal(0.0), h.cx.signal(0.0));
  let row = |clicks: &Counter| {
    let c = clicks.clone();
    Button::new(Icon::new("x").size(20)).on_click(move |_| c.hit())
  };
  let (one, two) = (clicks.clone(), clicks.clone());
  let list = crate::ListView::new(
    || (0..100).collect::<Vec<u32>>(),
    |n| *n,
    move |_, _| row(&one),
  )
  .row_height(20)
  .height(50)
  .offset(a)
  .mouse_drag(false);
  let grid = crate::GridView::new(
    || (0..100).collect::<Vec<u32>>(),
    |n| *n,
    move |_, _| row(&two),
  )
  .cell_size(20, 20)
  .height(50)
  .offset(b)
  .mouse_drag(false);
  let lists = Row::new()
    .align(Align::Start)
    .child(Row::new().style(Style::new().width(20.0)).child(list))
    .child(Row::new().style(Style::new().width(20.0)).child(grid));
  let window = h.window(h.panel().child(lists), 100.0, 100.0);
  for (x, time) in [(10.0, 0), (30.0, 1000)] {
    window.pointer_motion_at(Point::new(x, 18.0), time);
    window.pointer_button_at(PointerButton::Left, true, time);
    window.pointer_motion_at(Point::new(x, 3.0), time + 100);
    window.pointer_button_at(PointerButton::Left, false, time + 200);
  }
  assert_eq!((a.get(), b.get(), clicks.get()), (0.0, 0.0, 2));
}

#[test]
fn a_held_track_pages_again_after_300_ms_then_every_50_until_the_handle_reaches_the_pointer() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let (window, _) = view(&mut h, offset, |view| view.scroll_bar(Style::new()));
  // The 16-pixel handle starts at 34 × offset / 550: below it, at 45, it
  // reaches the pointer once the offset passes 469.
  window.pointer_motion(Point::new(17.0, 45.0));
  let pressed = Instant::now();
  window.pointer_button_at(PointerButton::Left, true, 0);
  h.frame(&window);
  assert_eq!(offset.get(), 50.0, "the press pages at once");
  let first = next_timer().expect("it waits to page again");
  assert!(first >= pressed + Duration::from_millis(300));
  assert!(first <= Instant::now() + Duration::from_millis(300));
  fire_timers(first);
  h.frame(&window);
  assert_eq!(offset.get(), 100.0);
  let mut pages = Vec::new();
  let mut last: Option<Instant> = None;
  while let Some(due) = next_timer() {
    assert!(pages.len() < 20, "{pages:?}");
    if let Some(last) = last {
      assert_eq!(due - last, Duration::from_millis(50));
    }
    last = Some(due);
    fire_timers(due);
    h.frame(&window);
    pages.push(offset.get());
  }
  let paged: Vec<f32> = (3..=10).map(|n| n as f32 * 50.0).collect();
  assert_eq!(pages[..pages.len() - 1], paged[..], "{pages:?}");
  assert_eq!(pages.last(), Some(&500.0), "and stopped where it reached");
  window.pointer_button_at(PointerButton::Left, false, 1000);

  // Released before it repeats, it wakes nothing.
  window.pointer_motion(Point::new(17.0, 2.0));
  window.pointer_button_at(PointerButton::Left, true, 2000);
  h.frame(&window);
  assert_eq!(offset.get(), 450.0);
  assert!(next_timer().is_some());
  window.pointer_button_at(PointerButton::Left, false, 2100);
  h.frame(&window);
  assert!(next_timer().is_none() && !animating() && !window.needs_layout());
  fire_timers(Instant::now() + Duration::from_secs(1));
  assert_eq!(offset.get(), 450.0);
}

#[test]
fn leaving_mid_repeat_stops_it() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let (window, _) = view(&mut h, offset, |view| view.scroll_bar(Style::new()));
  window.pointer_motion(Point::new(17.0, 45.0));
  window.pointer_button_at(PointerButton::Left, true, 0);
  fire_timers(next_timer().expect("it waits to page again"));
  h.frame(&window);
  assert_eq!(offset.get(), 100.0);
  window.pointer_leave();
  assert!(next_timer().is_none());
}
