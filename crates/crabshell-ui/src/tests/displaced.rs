//! Displaced transitions: the children a keyed list's change moves slide
//! from where they were drawn to their new place, through a translate.

use std::time::{Duration, Instant};

use crabshell_reactive::Signal;

use super::{Counter, Harness, capture_logs, node_at};
use crate::{
  Button, Color, Easing, Point, PointerButton, Rect, Row, Style, Transition, Window,
  advance_animations, animating, fire_timers, next_timer,
};

/// A 20 by 20 box with a background.
const BOX: Style = Style::new()
  .width(20.0)
  .height(20.0)
  .background(Color::rgb(1, 2, 3));

const SLIDE: Transition = Transition::new(100).easing(Easing::Linear);

/// Where the child of the window's row at `index`, in drawing order, is
/// drawn: its laid-out corner moved by its transform.
fn drawn_x(window: &Window, index: usize) -> f32 {
  let view = window.view();
  let child = node_at(view.root(), &[0, index]);
  let rect = child.rect();
  child.transform().map_or(rect.x, |t| t.sx * rect.x + t.tx)
}

fn laid_x(window: &Window, index: usize) -> f32 {
  node_at(window.view().root(), &[0, index]).rect().x
}

fn count(window: &Window) -> usize {
  node_at(window.view().root(), &[0]).children().count()
}

/// A row of 20-wide boxes, one per item, that slide over `SLIDE`.
fn sliding_row(items: Signal<Vec<u32>>) -> Row {
  Row::new()
    .align(crate::Align::Start)
    .children_keyed(move || items.get(), |n| *n, |_cx, _n| Row::new().style(BOX))
    .displaced(SLIDE)
}

/// Ticks the animation clock at `ms` past `start` and lays out.
fn tick(h: &mut Harness, window: &Window, start: Instant, ms: u64) {
  advance_animations(start + Duration::from_millis(ms));
  h.frame(window);
}

#[test]
fn the_children_after_a_removed_one_slide_into_its_place() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2, 3]);
  let window = h.window(h.panel().child(sliding_row(items)), 200.0, 30.0);
  assert_eq!(drawn_x(&window, 2), 40.0);

  items.set(vec![2, 3]);
  h.frame(&window);
  assert_eq!(laid_x(&window, 0), 0.0, "the layout moves at once");
  assert_eq!(drawn_x(&window, 0), 20.0, "2 is still drawn where it was");
  assert_eq!(drawn_x(&window, 1), 40.0);
  assert!(animating());

  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  assert_eq!(drawn_x(&window, 0), 20.0, "the first tick is time 0");
  tick(&mut h, &window, start, 50);
  assert_eq!(drawn_x(&window, 0), 10.0, "halfway");
  assert_eq!(drawn_x(&window, 1), 30.0);
  tick(&mut h, &window, start, 100);
  assert_eq!(drawn_x(&window, 0), 0.0);
  assert_eq!(node_at(window.view().root(), &[0, 0]).transform(), None);
  assert!(!animating(), "a settled slide stops the clock");
}

#[test]
fn without_displaced_the_children_jump() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2, 3]);
  let row = Row::new().children_keyed(move || items.get(), |n| *n, |_cx, _n| Row::new().style(BOX));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  items.set(vec![2, 3]);
  h.frame(&window);
  assert_eq!(drawn_x(&window, 0), 0.0);
  assert!(!animating());
}

#[test]
fn an_added_child_shows_at_once_and_pushes_the_others_aside() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![2, 3]);
  let window = h.window(h.panel().child(sliding_row(items)), 200.0, 30.0);
  items.set(vec![1, 2, 3]);
  h.frame(&window);
  assert_eq!(
    drawn_x(&window, 0),
    0.0,
    "the new child has nowhere to come from"
  );
  assert_eq!(drawn_x(&window, 1), 0.0, "2 starts where it was");
  assert_eq!(laid_x(&window, 1), 20.0);
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  tick(&mut h, &window, start, 100);
  assert_eq!(drawn_x(&window, 1), 20.0);
}

#[test]
fn a_child_moved_again_mid_slide_starts_from_where_it_is_drawn() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2, 3]);
  let window = h.window(h.panel().child(sliding_row(items)), 200.0, 30.0);
  items.set(vec![2, 3]);
  h.frame(&window);
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  tick(&mut h, &window, start, 50);
  assert_eq!(drawn_x(&window, 1), 30.0, "3 is halfway from 40 to 20");

  // 2 goes too: 3's place is now 0, and it moves on from 30.
  items.set(vec![3]);
  h.frame(&window);
  assert_eq!(drawn_x(&window, 0), 30.0);
  tick(&mut h, &window, start, 50);
  tick(&mut h, &window, start, 100);
  assert_eq!(
    drawn_x(&window, 0),
    15.0,
    "halfway from 30 to 0, on a slide of its own"
  );
  tick(&mut h, &window, start, 150);
  assert_eq!(drawn_x(&window, 0), 0.0);
  tick(&mut h, &window, start, 160);
  assert!(!animating(), "the first slide ended when the second began");
}

#[test]
fn a_leaving_child_keeps_its_space_and_the_rest_slide_once_it_goes() {
  const EXIT: Duration = Duration::from_millis(200);
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2, 3]);
  let row = Row::new()
    .align(crate::Align::Start)
    .children_keyed(move || items.get(), |n| *n, |_cx, _n| Row::new().style(BOX))
    .exit(EXIT)
    .displaced(SLIDE);
  let window = h.window(h.panel().child(row), 200.0, 30.0);

  items.set(vec![2, 3]);
  h.frame(&window);
  assert_eq!(count(&window), 3, "1 is leaving");
  assert!(!animating(), "nothing moved yet");

  fire_timers(next_timer().expect("1's exit runs") + EXIT);
  h.frame(&window);
  assert_eq!(count(&window), 2, "1 went");
  assert_eq!(drawn_x(&window, 0), 20.0, "2 slides from where it was");
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  tick(&mut h, &window, start, 100);
  assert_eq!(drawn_x(&window, 0), 0.0);
}

#[test]
fn a_key_that_comes_back_mid_slide_is_new_and_the_rest_slide_back() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let window = h.window(h.panel().child(sliding_row(items)), 200.0, 30.0);
  items.set(vec![2]);
  h.frame(&window);
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  tick(&mut h, &window, start, 50);
  assert_eq!(drawn_x(&window, 0), 10.0);

  items.set(vec![1, 2]);
  h.frame(&window);
  assert_eq!(drawn_x(&window, 0), 0.0, "1 is built again, in place");
  assert_eq!(drawn_x(&window, 1), 10.0, "2 turns back from where it is");
  tick(&mut h, &window, start, 50);
  tick(&mut h, &window, start, 150);
  assert_eq!(drawn_x(&window, 1), 20.0);
}

#[test]
fn a_slide_repaints_where_it_was_and_where_it_is_without_a_layout() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let window = h.window(h.panel().child(sliding_row(items)), 200.0, 30.0);
  items.set(vec![2]);
  h.frame(&window);
  window.take_damage();
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  assert!(!window.has_damage(), "time 0 moves nothing");

  advance_animations(start + Duration::from_millis(50));
  assert!(window.needs_layout(), "a tick places the window again");
  h.frame(&window);
  let damage = window.take_damage();
  let covers = |area: Rect| {
    damage
      .iter()
      .any(|rect| rect.x <= area.x && rect.right() >= area.right())
  };
  assert!(covers(Rect::new(20.0, 0.0, 20.0, 20.0)), "{damage:?}");
  assert!(covers(Rect::new(10.0, 0.0, 20.0, 20.0)), "{damage:?}");
}

#[test]
fn a_child_mid_slide_is_hit_where_it_is_drawn() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let clicks = Counter::default();
  let counted = clicks.clone();
  let row = Row::new()
    .align(crate::Align::Start)
    .children_keyed(
      move || items.get(),
      |n| *n,
      move |_cx, n| {
        let counted = counted.clone();
        Button::new(Row::new().style(BOX)).on_click(move |_| {
          if n.get_untracked() == 2 {
            counted.hit();
          }
        })
      },
    )
    .displaced(SLIDE);
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  items.set(vec![2]);
  h.frame(&window);
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  tick(&mut h, &window, start, 50);

  // 2 is laid out at 0..20 and drawn at 10..30.
  window.pointer_motion(Point::new(25.0, 10.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(clicks.get(), 1, "hit where it is drawn");
  window.pointer_motion(Point::new(5.0, 10.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(clicks.get(), 1, "not where it is laid out");
}

#[test]
fn a_list_disposed_mid_slide_frees_everything_and_stops_the_clock() {
  let mut h = Harness::new();
  let outer = h.cx.signal(Vec::<u32>::new());
  let items = h.cx.signal(vec![1, 2, 3]);
  let row = Row::new().children_keyed(
    move || outer.get(),
    |n| *n,
    move |_cx, _n| sliding_row(items),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let empty = h.runtime.node_count();
  outer.set(vec![0]);
  h.frame(&window);
  items.set(vec![3]);
  h.frame(&window);
  let start = Instant::now();
  advance_animations(start);
  assert!(animating(), "2 and 3 slide");

  outer.set(Vec::new());
  h.frame(&window);
  assert_eq!(h.runtime.node_count(), empty);
  advance_animations(start + Duration::from_millis(10));
  assert!(
    !animating(),
    "a slide whose child went stops at the next tick"
  );
}

#[test]
fn a_resize_moves_the_children_at_once() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let row = Row::new()
    .justify(crate::Justify::End)
    .children_keyed(move || items.get(), |n| *n, |_cx, _n| Row::new().style(BOX))
    .displaced(SLIDE);
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(drawn_x(&window, 0), 160.0);
  window.set_size(crate::Size::new(100.0, 30.0));
  h.frame(&window);
  assert_eq!(drawn_x(&window, 0), 60.0);
  assert!(!animating());
}

#[test]
fn a_displaced_binding_is_read_at_each_change_and_zero_jumps() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2, 3]);
  let reduced = h.cx.signal(false);
  let reads = Counter::default();
  let counted = reads.clone();
  let row = Row::new()
    .align(crate::Align::Start)
    .children_keyed(move || items.get(), |n| *n, |_cx, _n| Row::new().style(BOX))
    .displaced(move || {
      counted.hit();
      if reduced.get() {
        Transition::new(0)
      } else {
        SLIDE
      }
    });
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  // A change at the end moves nothing, but reads the binding.
  items.set(vec![1, 2, 3, 4]);
  h.frame(&window);
  let before = reads.get();
  assert!(before > 0);

  reduced.set(true);
  h.frame(&window);
  assert_eq!(reads.get(), before, "the binding is read untracked");
  items.set(vec![2, 3, 4]);
  h.frame(&window);
  assert_eq!(drawn_x(&window, 0), 0.0, "a transition of 0 jumps");
  assert!(!animating());

  reduced.set(false);
  items.set(vec![3, 4]);
  h.frame(&window);
  assert_eq!(drawn_x(&window, 0), 20.0, "read again at this change");
}

#[test]
fn a_shorter_slide_that_replaces_a_longer_one_is_not_undone_by_it() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2, 3]);
  let quick = h.cx.signal(false);
  let row = Row::new()
    .align(crate::Align::Start)
    .children_keyed(move || items.get(), |n| *n, |_cx, _n| Row::new().style(BOX))
    .displaced(move || {
      if quick.get() {
        Transition::new(20).easing(Easing::Linear)
      } else {
        SLIDE
      }
    });
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  items.set(vec![2, 3]);
  h.frame(&window);
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  tick(&mut h, &window, start, 50);

  quick.set(true);
  items.set(vec![3]);
  h.frame(&window);
  tick(&mut h, &window, start, 50);
  tick(&mut h, &window, start, 70);
  assert_eq!(drawn_x(&window, 0), 0.0, "the quick slide is over");
  tick(&mut h, &window, start, 80);
  assert_eq!(
    drawn_x(&window, 0),
    0.0,
    "and the slow one it replaced moves nothing"
  );
}

#[test]
fn a_displaced_binding_that_panics_is_logged_and_the_children_jump() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let row = Row::new()
    .children_keyed(move || items.get(), |n| *n, |_cx, _n| Row::new().style(BOX))
    .displaced(|| -> Transition { panic!("no slide") });
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let logs = capture_logs(|| {
    items.set(vec![2]);
    h.frame(&window);
  });
  assert!(
    logs.contains(
      "PanelWindow[DP-1]/Row.children: its displaced transition panicked; the children jump"
    ),
    "{logs}"
  );
  assert_eq!(drawn_x(&window, 0), 0.0);
  assert_eq!(count(&window), 1, "the list still follows its items");
}

#[test]
fn displaced_before_any_keyed_list_warns_and_does_nothing() {
  let logs = capture_logs(|| {
    let _ = Row::new().displaced(SLIDE);
  });
  assert!(logs.contains("`displaced` was called before any"), "{logs}");
}

#[test]
fn the_children_before_a_removed_one_stay_and_arm_nothing() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2, 3]);
  let window = h.window(h.panel().child(sliding_row(items)), 200.0, 30.0);
  items.set(vec![1, 2]);
  h.frame(&window);
  assert!(!animating(), "1 and 2 didn't move");
}

#[test]
fn a_container_that_moves_too_moves_at_once_and_its_children_slide_within_it() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let wide = h.cx.signal(false);
  let row = Row::new()
    .align(crate::Align::Start)
    .child(Row::new().style(move || BOX.width(if wide.get() { 40.0 } else { 20.0 })))
    .child(sliding_row(items));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  // Where item `index` is drawn.
  let inner = |window: &Window, index: usize| {
    let view = window.view();
    let child = node_at(view.root(), &[0, 1, index]);
    let rect = child.rect();
    child.transform().map_or(rect.x, |t| rect.x + t.tx)
  };
  assert_eq!(inner(&window, 1), 40.0, "2 is at 20 in a row at 20");

  wide.set(true);
  items.set(vec![2]);
  h.frame(&window);
  assert_eq!(
    inner(&window, 0),
    60.0,
    "the row moved to 40 at once, 2 still at 20 in it"
  );
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  tick(&mut h, &window, start, 100);
  assert_eq!(inner(&window, 0), 40.0);
}

#[test]
fn a_static_child_after_an_empty_list_slides_when_the_first_item_comes() {
  let mut h = Harness::new();
  let items = h.cx.signal(Vec::<u32>::new());
  let row = sliding_row(items).child(Row::new().style(BOX));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(drawn_x(&window, 0), 0.0);
  items.set(vec![1]);
  h.frame(&window);
  assert_eq!(laid_x(&window, 1), 20.0);
  assert_eq!(
    drawn_x(&window, 1),
    0.0,
    "the box after the list slides from where it was"
  );
}
