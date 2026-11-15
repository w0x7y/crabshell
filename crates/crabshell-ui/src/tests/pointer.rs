//! Pointer input added in Phase 15: release, double click, long press,
//! drag, the position signal, both scroll axes, side buttons, cursors,
//! `enabled` and focus from code.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::Signal;

use super::{Counter, Harness, capture_logs, lift, rect_at, wheel};
use crate::{
  Align, Button, Color, CursorShape, Drag, DragPhase, Icon, Key, KeyEvent, Modifiers, Point,
  PointerButton, Row, ScrollSource, Size, Stack, Style, TextInput, Wheel, WheelAxis, Window,
  fire_timers, next_timer,
};

const NORMAL: Style = Style::new().background(Color::rgb(1, 1, 1));
const HOVER: Style = Style::new().background(Color::rgb(2, 2, 2));

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

/// A left press and release at the given times.
fn click_at(window: &Window, time: u32) {
  window.pointer_button_at(PointerButton::Left, true, time);
  window.pointer_button_at(PointerButton::Left, false, time + 50);
}

fn background(window: &Window, path: &[usize]) -> Option<Color> {
  super::node_at(window.view().root(), path)
    .style()
    .background
}

#[test]
fn release_runs_before_click_and_wherever_the_pointer_is() {
  let mut h = Harness::new();
  let log = log();
  let window = one(
    &mut h,
    Button::new(square())
      .on_release(note(&log, "release"))
      .on_click(note(&log, "click")),
  );
  window.pointer_motion(Point::new(10.0, 10.0));
  click_at(&window, 0);
  assert_eq!(*log.borrow(), ["release", "click"]);
  // Released away from the button: no click, but still the release.
  window.pointer_button_at(PointerButton::Left, true, 1000);
  window.pointer_motion(Point::new(100.0, 10.0));
  window.pointer_button_at(PointerButton::Left, false, 1100);
  assert_eq!(*log.borrow(), ["release", "click", "release"]);
  // Other buttons don't release it.
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_button_at(PointerButton::Right, true, 2000);
  window.pointer_button_at(PointerButton::Right, false, 2050);
  assert_eq!(log.borrow().len(), 3);
}

#[test]
fn a_second_click_soon_and_close_is_a_double_click() {
  let mut h = Harness::new();
  let log = log();
  let window = one(
    &mut h,
    Button::new(square())
      .on_click(note(&log, "click"))
      .on_double_click(note(&log, "double")),
  );
  window.pointer_motion(Point::new(10.0, 10.0));
  click_at(&window, 1000);
  // 400 ms after the first click's release, and 4 pixels away.
  window.pointer_motion(Point::new(13.0, 12.0));
  click_at(&window, 1350);
  assert_eq!(*log.borrow(), ["click", "double"]);
  // A third click starts over.
  click_at(&window, 1500);
  assert_eq!(*log.borrow(), ["click", "double", "click"]);
  // Too late.
  click_at(&window, 1951);
  assert_eq!(log.borrow().len(), 4);
  assert_eq!(log.borrow()[3], "click");
  // Too far: 6 pixels.
  window.pointer_motion(Point::new(19.0, 12.0));
  click_at(&window, 2000);
  assert_eq!(log.borrow()[4], "click");
  // Times count on from where they wrap around.
  window.pointer_motion(Point::new(10.0, 10.0));
  click_at(&window, u32::MAX - 100);
  click_at(&window, 100);
  assert_eq!(log.borrow()[6], "double");
}

#[test]
fn without_a_double_click_handler_both_clicks_click() {
  let mut h = Harness::new();
  let clicks = Counter::default();
  let counted = clicks.clone();
  let window = one(
    &mut h,
    Button::new(square()).on_click(move |_| counted.hit()),
  );
  window.pointer_motion(Point::new(10.0, 10.0));
  click_at(&window, 0);
  click_at(&window, 100);
  assert_eq!(clicks.get(), 2);
}

#[test]
fn holding_runs_the_long_press_and_the_release_doesnt_click() {
  let mut h = Harness::new();
  let log = log();
  let window = one(
    &mut h,
    Button::new(square())
      .on_click(note(&log, "click"))
      .on_long_press(note(&log, "long"))
      .on_release(note(&log, "release")),
  );
  window.pointer_motion(Point::new(10.0, 10.0));
  assert_eq!(next_timer(), None, "nothing is armed before a press");
  window.pointer_button(PointerButton::Left, true);
  let due = next_timer().expect("the long press is armed");
  let wait = due.saturating_duration_since(Instant::now());
  assert!(wait > Duration::from_millis(700) && wait <= Duration::from_millis(800));
  fire_timers(due - Duration::from_millis(1));
  assert!(log.borrow().is_empty());
  fire_timers(due);
  assert_eq!(*log.borrow(), ["long"]);
  assert_eq!(next_timer(), None);
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(*log.borrow(), ["long", "release"]);

  // A release before 800 ms clicks, and stops the timer.
  window.pointer_button(PointerButton::Left, true);
  assert!(next_timer().is_some());
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(next_timer(), None, "nothing is armed after the release");
  fire_timers(Instant::now() + Duration::from_secs(2));
  assert_eq!(*log.borrow(), ["long", "release", "release", "click"]);
}

#[test]
fn moving_past_the_threshold_or_leaving_stops_a_long_press() {
  let mut h = Harness::new();
  let longs = Counter::default();
  let counted = longs.clone();
  let window = one(
    &mut h,
    Button::new(square()).on_long_press(move |_| counted.hit()),
  );
  window.pointer_motion(Point::new(5.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  // 10 pixels is still a hold.
  window.pointer_motion(Point::new(11.0, 13.0));
  assert!(next_timer().is_some());
  window.pointer_motion(Point::new(16.0, 5.0));
  assert_eq!(next_timer(), None);
  window.pointer_button(PointerButton::Left, false);

  window.pointer_motion(Point::new(5.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_leave();
  assert_eq!(next_timer(), None);
  fire_timers(Instant::now() + Duration::from_secs(2));
  assert_eq!(longs.get(), 0);
}

#[test]
fn a_button_without_a_long_press_arms_nothing() {
  let mut h = Harness::new();
  let window = one(&mut h, Button::new(square()).on_click(|_| {}));
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_button(PointerButton::Left, true);
  assert_eq!(next_timer(), None);
  window.pointer_button(PointerButton::Left, false);
}

#[test]
fn a_button_removed_while_held_never_runs_its_long_press() {
  let mut h = Harness::new();
  let shown = h.cx.signal(true);
  let longs = Counter::default();
  let counted = longs.clone();
  let row = Row::new().align(Align::Start).children_keyed(
    move || if shown.get() { vec![()] } else { Vec::new() },
    |_| (),
    move |_, _| {
      let counted = counted.clone();
      Button::new(square()).on_long_press(move |_| counted.hit())
    },
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_button(PointerButton::Left, true);
  assert!(next_timer().is_some());
  shown.set(false);
  h.frame(&window);
  assert_eq!(next_timer(), None, "its timer went with it");
  fire_timers(Instant::now() + Duration::from_secs(2));
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(longs.get(), 0);
}

/// A button that keeps every drag it gets.
fn dragged(log: &Rc<RefCell<Vec<Drag>>>) -> impl Fn(&crate::Cx, Drag) + 'static {
  let log = Rc::clone(log);
  move |_, drag| log.borrow_mut().push(drag)
}

#[test]
fn a_held_button_drags_past_the_threshold_and_ends_on_release() {
  let mut h = Harness::new();
  let drags = Rc::new(RefCell::new(Vec::new()));
  let log = log();
  let row = Row::new().align(Align::Start).child(square()).child(
    Button::new(square())
      .style(NORMAL)
      .hover_style(HOVER)
      .cursor(CursorShape::Grab)
      .on_click(note(&log, "click"))
      .on_release(note(&log, "release"))
      .on_drag(dragged(&drags)),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  // The button is at x = 20.
  window.pointer_motion(Point::new(25.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_motion(Point::new(30.0, 5.0));
  assert!(drags.borrow().is_empty(), "within the threshold");
  window.pointer_motion(Point::new(36.0, 5.0));
  let size = Size::new(20.0, 20.0);
  assert_eq!(
    drags.borrow()[0],
    Drag {
      phase: DragPhase::Start,
      start: Point::new(5.0, 5.0),
      position: Point::new(16.0, 5.0),
      size,
    }
  );
  // Out of the button and the window, it still moves and stays hovered.
  assert_eq!(
    window.pointer_motion(Point::new(300.0, -40.0)),
    CursorShape::Grab
  );
  assert_eq!(background(&window, &[0, 1]), HOVER.background);
  assert_eq!(
    drags.borrow()[1],
    Drag {
      phase: DragPhase::Move,
      start: Point::new(5.0, 5.0),
      position: Point::new(280.0, -40.0),
      size,
    }
  );
  // Back over it, the release ends the drag and doesn't click.
  window.pointer_motion(Point::new(25.0, 5.0));
  window.pointer_button(PointerButton::Left, false);
  let last = drags.borrow()[3];
  assert_eq!(
    (last.phase, last.position),
    (DragPhase::End, Point::new(5.0, 5.0))
  );
  assert_eq!(*log.borrow(), ["release"]);
  // After it the hover follows the pointer again.
  window.pointer_motion(Point::new(100.0, 5.0));
  assert_eq!(background(&window, &[0, 1]), NORMAL.background);
  assert_eq!(drags.borrow().len(), 4);
}

#[test]
fn a_button_that_only_drags_starts_at_the_press() {
  let mut h = Harness::new();
  let drags = Rc::new(RefCell::new(Vec::new()));
  let window = one(&mut h, Button::new(square()).on_drag(dragged(&drags)));
  window.pointer_motion(Point::new(4.0, 6.0));
  window.pointer_button(PointerButton::Left, true);
  let first = drags.borrow()[0];
  assert_eq!(first.phase, DragPhase::Start);
  assert_eq!(
    (first.start, first.position),
    (Point::new(4.0, 6.0), Point::new(4.0, 6.0))
  );
  window.pointer_motion(Point::new(5.0, 6.0));
  assert_eq!(drags.borrow()[1].phase, DragPhase::Move);
}

#[test]
fn a_drag_is_in_the_buttons_coordinates_through_its_transform() {
  let mut h = Harness::new();
  let drags = Rc::new(RefCell::new(Vec::new()));
  let button = Button::new(square())
    .style(Style::new().translate(50.0, 0.0).scale(2.0))
    .on_drag(dragged(&drags));
  let window = one(&mut h, button);
  // Scaled 2 around its middle (10, 10) and moved 50 right: its top-left
  // is drawn at (40, -10), so (60, 10) is its middle.
  window.pointer_motion(Point::new(60.0, 10.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_motion(Point::new(80.0, 10.0));
  let drags = drags.borrow();
  assert_eq!(drags[0].start, Point::new(10.0, 10.0));
  assert_eq!(drags[1].position, Point::new(20.0, 10.0));
  assert_eq!(drags[1].size, Size::new(20.0, 20.0));
}

#[test]
fn leaving_ends_a_drag_and_a_dropped_window_runs_nothing() {
  let mut h = Harness::new();
  let drags = Rc::new(RefCell::new(Vec::new()));
  let window = one(&mut h, Button::new(square()).on_drag(dragged(&drags)));
  window.pointer_motion(Point::new(4.0, 4.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_motion(Point::new(8.0, 4.0));
  window.pointer_leave();
  assert_eq!(drags.borrow().last().map(|d| d.phase), Some(DragPhase::End));
  assert_eq!(
    drags.borrow().last().map(|d| d.position),
    Some(Point::new(8.0, 4.0))
  );
  // A release after coming back ends nothing again.
  window.pointer_motion(Point::new(4.0, 4.0));
  let count = drags.borrow().len();
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(drags.borrow().len(), count);

  window.pointer_button(PointerButton::Left, true);
  let count = drags.borrow().len();
  drop(window);
  assert_eq!(drags.borrow().len(), count, "no End from a dropped window");
}

#[test]
fn a_panicking_drag_handler_is_logged_and_the_rest_runs_on() {
  let mut h = Harness::new();
  let clicks = Counter::default();
  let counted = clicks.clone();
  let row = Row::new()
    .align(Align::Start)
    .child(Button::new(square()).on_drag(|_, _| panic!("drag failure")))
    .child(Button::new(square()).on_click(move |_| counted.hit()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let logs = capture_logs(|| {
    window.pointer_motion(Point::new(5.0, 5.0));
    window.pointer_button(PointerButton::Left, true);
    window.pointer_motion(Point::new(7.0, 5.0));
    window.pointer_button(PointerButton::Left, false);
    window.pointer_motion(Point::new(25.0, 5.0));
    window.pointer_button(PointerButton::Left, true);
    window.pointer_button(PointerButton::Left, false);
  });
  assert!(
    logs.contains("handler PanelWindow[DP-1]/Row/Button[0].on_drag panicked and is disabled"),
    "{logs}"
  );
  assert_eq!(logs.matches("drag failure").count(), 1, "{logs}");
  assert_eq!(clicks.get(), 1);
}

#[test]
fn a_panicking_long_press_is_logged_and_the_shell_runs_on() {
  let mut h = Harness::new();
  let clicks = Counter::default();
  let counted = clicks.clone();
  let window = one(
    &mut h,
    Button::new(square())
      .on_long_press(|_| panic!("long failure"))
      .on_click(move |_| counted.hit()),
  );
  let logs = capture_logs(|| {
    window.pointer_motion(Point::new(5.0, 5.0));
    window.pointer_button(PointerButton::Left, true);
    fire_timers(Instant::now() + Duration::from_secs(1));
    window.pointer_button(PointerButton::Left, false);
    window.pointer_button(PointerButton::Left, true);
    window.pointer_button(PointerButton::Left, false);
  });
  assert!(
    logs.contains("handler PanelWindow[DP-1]/Row/Button[0].on_long_press panicked and is disabled"),
    "{logs}"
  );
  assert_eq!(clicks.get(), 1, "the second press clicks");
}

#[test]
fn the_position_signal_follows_the_pointer_over_the_button() {
  let mut h = Harness::new();
  let outer: Signal<Option<Point>> = h.cx.signal(None);
  let inner: Signal<Option<Point>> = h.cx.signal(None);
  let row = Row::new().align(Align::Start).child(
    Button::new(
      Row::new()
        .child(square())
        .child(Button::new(square()).pointer_position(inner)),
    )
    .pointer_position(outer),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(5.0, 6.0));
  assert_eq!(outer.get_untracked(), Some(Point::new(5.0, 6.0)));
  assert_eq!(inner.get_untracked(), None);
  // Over the inner button, both have it, each in its own coordinates.
  window.pointer_motion(Point::new(25.0, 6.0));
  assert_eq!(outer.get_untracked(), Some(Point::new(25.0, 6.0)));
  assert_eq!(inner.get_untracked(), Some(Point::new(5.0, 6.0)));
  window.pointer_motion(Point::new(100.0, 6.0));
  assert_eq!((outer.get_untracked(), inner.get_untracked()), (None, None));

  // Written only on a change.
  let writes = Counter::default();
  let counted = writes.clone();
  h.cx.effect(move || {
    outer.get();
    counted.hit();
  });
  h.runtime.flush();
  window.pointer_motion(Point::new(5.0, 6.0));
  h.runtime.flush();
  window.pointer_motion(Point::new(5.0, 6.0));
  h.runtime.flush();
  window.pointer_leave();
  h.runtime.flush();
  assert_eq!(writes.get(), 3);
  assert_eq!(outer.get_untracked(), None);
}

#[test]
fn the_position_signal_follows_a_drag_out_of_the_button() {
  let mut h = Harness::new();
  let at: Signal<Option<Point>> = h.cx.signal(None);
  let window = one(
    &mut h,
    Button::new(square())
      .on_drag(|_, _| {})
      .pointer_position(at),
  );
  window.pointer_motion(Point::new(5.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_motion(Point::new(150.0, 25.0));
  assert_eq!(at.get_untracked(), Some(Point::new(150.0, 25.0)));
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(at.get_untracked(), None, "not over it once the drag ends");
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

fn key(key: Key) -> KeyEvent {
  KeyEvent {
    key,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  }
}

#[test]
fn a_disabled_button_lets_the_pointer_through_and_runs_nothing() {
  let mut h = Harness::new();
  let enabled = h.cx.signal(false);
  let log = log();
  let hovered = h.cx.signal(false);
  let at = h.cx.signal(None);
  let inner = Button::new(square())
    .enabled(move || enabled.get())
    .style(NORMAL)
    .hover_style(HOVER)
    .cursor(CursorShape::Grab)
    .hovered(hovered)
    .pointer_position(at)
    .on_click(note(&log, "inner"))
    .on_scroll(|_, _| panic!("a disabled button scrolled"));
  let row = Row::new()
    .align(Align::Start)
    .child(Button::new(Row::new().child(square()).child(inner)).on_click(note(&log, "outer")));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.keyboard_enter();
  // The outer button takes the pointer, its cursor and the click.
  assert_eq!(
    window.pointer_motion(Point::new(25.0, 5.0)),
    CursorShape::Pointer
  );
  assert_eq!(background(&window, &[0, 0, 0, 1]), NORMAL.background);
  assert_eq!((hovered.get_untracked(), at.get_untracked()), (false, None));
  click_at(&window, 0);
  window.pointer_scroll(wheel(120));
  assert_eq!(*log.borrow(), ["outer"]);
  // Tab skips it.
  window.key(&key(Key::Tab));
  window.key(&key(Key::Tab));
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Row/Button[0]")
  );

  // Turned on, it takes the pointer under it at once.
  enabled.set(true);
  h.frame(&window);
  assert_eq!(window.cursor(), CursorShape::Grab);
  assert_eq!(background(&window, &[0, 0, 0, 1]), HOVER.background);
  assert!(hovered.get_untracked());
  assert_eq!(at.get_untracked(), Some(Point::new(5.0, 5.0)));
}

#[test]
fn turning_a_button_off_ends_its_drag_press_and_focus() {
  let mut h = Harness::new();
  let enabled = h.cx.signal(true);
  let drags = Rc::new(RefCell::new(Vec::new()));
  let log = log();
  let window = one(
    &mut h,
    Button::new(square())
      .enabled(move || enabled.get())
      .focus(true)
      .on_click(note(&log, "click"))
      .on_release(note(&log, "release"))
      .on_drag(dragged(&drags)),
  );
  window.keyboard_enter();
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Row/Button[0]")
  );
  window.pointer_motion(Point::new(5.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_motion(Point::new(18.0, 5.0));
  enabled.set(false);
  h.frame(&window);
  let phases: Vec<DragPhase> = drags.borrow().iter().map(|drag| drag.phase).collect();
  assert_eq!(phases, [DragPhase::Start, DragPhase::End]);
  assert_eq!(window.focused(), None);
  // Its release and Enter do nothing now.
  window.pointer_button(PointerButton::Left, false);
  window.key(&key(Key::Enter));
  assert!(log.borrow().is_empty());
  assert_eq!(drags.borrow().len(), 2);
}

#[test]
fn focus_from_code_takes_the_focus_unless_disabled() {
  let mut h = Harness::new();
  let first = h.cx.signal(false);
  let second = h.cx.signal(false);
  let clicks = Counter::default();
  let counted = clicks.clone();
  let row = Row::new()
    .align(Align::Start)
    .child(
      Button::new(square())
        .focus(move || first.get())
        .on_click(move |_| counted.hit()),
    )
    .child(
      Button::new(square())
        .enabled(false)
        .focus(move || second.get()),
    );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.keyboard_enter();
  assert_eq!(window.focused(), None);
  first.set(true);
  h.frame(&window);
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Row/Button[0]")
  );
  window.key(&key(Key::Enter));
  assert_eq!(clicks.get(), 1);
  second.set(true);
  h.frame(&window);
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Row/Button[0]")
  );
}

#[test]
fn a_panicking_enabled_or_cursor_binding_is_logged_and_the_button_still_clicks() {
  let mut h = Harness::new();
  let clicks = Counter::default();
  let counted = clicks.clone();
  let row = Row::new().align(Align::Start).child(
    Button::new(square())
      .enabled(|| -> bool { panic!("enabled failure") })
      .cursor(|| -> CursorShape { panic!("cursor failure") })
      .on_click(move |_| counted.hit()),
  );
  let mut window = None;
  let logs = capture_logs(|| {
    let built = h.window(h.panel().child(row), 200.0, 30.0);
    built.pointer_motion(Point::new(5.0, 5.0));
    click_at(&built, 0);
    window = Some(built);
  });
  for name in ["enabled", "cursor"] {
    assert!(
      logs.contains(&format!(
        "effect PanelWindow[DP-1]/Row/Button[0].{name} panicked and is disabled"
      )),
      "{logs}"
    );
  }
  assert_eq!(clicks.get(), 1, "the button stays on, with its own cursor");
  let window = window.expect("built");
  assert_eq!(window.cursor(), CursorShape::Pointer);

  // An End that panics while `enabled` cuts its drag short is logged too.
  let on = h.cx.signal(true);
  let row =
    Row::new()
      .align(Align::Start)
      .child(
        Button::new(square())
          .enabled(move || on.get())
          .on_drag(|_, drag| {
            if drag.phase == DragPhase::End {
              panic!("end failure")
            }
          }),
      );
  let other = h.window(h.panel().child(row), 200.0, 30.0);
  let logs = capture_logs(|| {
    other.pointer_motion(Point::new(5.0, 5.0));
    other.pointer_button(PointerButton::Left, true);
    on.set(false);
    h.frame(&other);
  });
  assert!(
    logs.contains("handler PanelWindow[DP-1]/Row/Button[0].on_drag panicked and is disabled"),
    "{logs}"
  );
  assert!(
    !logs.contains("effect PanelWindow[DP-1]/Row/Button[0].enabled"),
    "{logs}"
  );
}
