//! `children_keyed`: children follow the items, reused by key.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::{Counter, Harness, capture_logs, node_at, paths};
use crabshell_reactive::{ReadSignal, Signal};

use crate::{
  Button, Content, Cx, Easing, Element, Icon, Row, Spring, Style, Text, Transition, Widget, Window,
  advance_animations, animating, fire_timers, next_timer,
};

/// The text of each child of the window's row.
fn texts(window: &Window) -> Vec<String> {
  let view = window.view();
  node_at(view.root(), &[0])
    .children()
    .map(|child| match child.content() {
      Content::Text(text) => text.content().to_owned(),
      _ => String::from("(not text)"),
    })
    .collect()
}

#[test]
fn children_follow_the_items_and_are_reused_by_key() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2, 3]);
  let built = Counter::default();
  let counted = built.clone();
  let row = Row::new().children_keyed(
    move || items.get(),
    |n| *n,
    move |_cx, n| {
      counted.hit();
      Text::new(move || format!("item {}", n.get()))
    },
  );
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  assert_eq!(texts(&window), ["item 1", "item 2", "item 3"]);
  assert_eq!(built.get(), 3);

  items.set(vec![3, 1, 4]);
  h.frame(&window);
  assert_eq!(texts(&window), ["item 3", "item 1", "item 4"]);
  // Only item 4 is new. A child's path holds the order it was created in
  // among its siblings, so the reused children show that they were not
  // rebuilt, and the new one gets a number no other child had.
  assert_eq!(built.get(), 4);
  let row_paths: Vec<String> = paths(&window).into_iter().skip(2).collect();
  assert_eq!(
    row_paths,
    [
      "PanelWindow[DP-1]/Row/Text[2]",
      "PanelWindow[DP-1]/Row/Text[0]",
      "PanelWindow[DP-1]/Row/Text[3]",
    ]
  );
}

#[test]
fn a_child_updates_only_when_its_own_item_changes() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![(1_usize, "a"), (2, "b")]);
  let runs = [Counter::default(), Counter::default()];
  let counters = runs.clone();
  let row = Row::new().children_keyed(
    move || items.get(),
    |item| item.0,
    move |_cx, item| {
      let runs = counters[item.get_untracked().0 - 1].clone();
      Text::new(move || {
        runs.hit();
        item.get().1.to_owned()
      })
    },
  );
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  assert_eq!([runs[0].get(), runs[1].get()], [1, 1]);

  items.set(vec![(1, "a"), (2, "B")]);
  h.frame(&window);
  assert_eq!(texts(&window), ["a", "B"]);
  assert_eq!([runs[0].get(), runs[1].get()], [1, 2]);

  // Same keys, same values, same order: no layout and nothing to repaint.
  window.take_damage();
  items.set(vec![(1, "a"), (2, "B")]);
  h.runtime.flush();
  assert!(!window.needs_layout());
  assert!(!window.has_damage());
}

#[test]
fn removed_children_free_everything_they_created() {
  let mut h = Harness::new();
  let items = h.cx.signal(Vec::<u32>::new());
  let row = Row::new().children_keyed(
    move || items.get(),
    |n| *n,
    |cx, n| {
      let extra = cx.signal(0);
      Text::new(move || format!("{} {}", n.get(), extra.get()))
    },
  );
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  let empty = h.runtime.node_count();

  items.set(vec![1, 2, 3]);
  h.frame(&window);
  assert!(h.runtime.node_count() > empty);
  let bounds: Vec<_> = {
    let view = window.view();
    node_at(view.root(), &[0])
      .children()
      .map(|child| child.bounds())
      .collect()
  };
  window.take_damage();

  items.set(Vec::new());
  h.frame(&window);
  assert_eq!(h.runtime.node_count(), empty);
  assert!(texts(&window).is_empty());
  let damage = window.take_damage();
  for area in bounds {
    assert!(damage.contains(&area), "{area:?} missing from {damage:?}");
  }
}

#[test]
fn keyed_children_sit_between_the_static_ones() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec!["b", "c"]);
  let row = Row::new()
    .child(Text::new("a"))
    .children_keyed(move || items.get(), |s| *s, |_cx, s| Text::new(s.get()))
    .child(Text::new("d"));
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  assert_eq!(texts(&window), ["a", "b", "c", "d"]);

  items.set(vec!["x"]);
  h.frame(&window);
  assert_eq!(texts(&window), ["a", "x", "d"]);
  // Single and keyed children share one count, so no two paths are equal.
  let all = paths(&window);
  let mut unique = all.clone();
  unique.sort();
  unique.dedup();
  assert_eq!(all.len(), unique.len(), "{all:#?}");
}

#[test]
fn a_duplicate_key_shows_the_first_item_and_warns_once() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![(1, "first"), (1, "second"), (2, "other")]);
  let row = Row::new().children_keyed(
    move || items.get(),
    |item| item.0,
    |_cx, item| Text::new(item.get().1),
  );
  let mut window = None;
  let logs = capture_logs(|| {
    let built = h.window(h.panel().child(row), 400.0, 30.0);
    items.set(vec![(2, "other"), (2, "again")]);
    h.frame(&built);
    window = Some(built);
  });
  let window = window.expect("the window was built");
  assert_eq!(texts(&window), ["other"]);
  assert_eq!(
    logs.matches("two items have the same key").count(),
    1,
    "{logs}"
  );
  assert!(logs.contains("PanelWindow[DP-1]/Row.children"), "{logs}");
}

#[test]
fn a_view_reading_a_signal_does_not_rerun_the_list() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1]);
  let other = h.cx.signal(0);
  let runs = Counter::default();
  let counted = runs.clone();
  let row = Row::new().children_keyed(
    move || {
      counted.hit();
      items.get()
    },
    |n| *n,
    move |_cx, _n| {
      // Read outside any binding: only the list's own effect could see it.
      let size = other.get();
      Icon::new("x").size(size)
    },
  );
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  assert_eq!(runs.get(), 1);
  other.set(5);
  h.frame(&window);
  assert_eq!(runs.get(), 1);
}

/// A widget of the user's own whose description reads a signal.
struct ReadsSignal(Signal<u32>);

impl Widget for ReadsSignal {
  fn into_element(self: Box<Self>) -> Element {
    let size = self.0.get();
    Box::new(Icon::new("x").size(size)).into_element()
  }
}

#[test]
fn a_nested_widget_in_a_view_adds_no_dependency_to_the_list() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1]);
  let other = h.cx.signal(4_u32);
  let runs = Counter::default();
  let counted = runs.clone();
  let row = Row::new().children_keyed(
    move || {
      counted.hit();
      items.get()
    },
    |n| *n,
    move |_cx, _n| Button::new(ReadsSignal(other)),
  );
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  assert_eq!(runs.get(), 1);

  other.set(5);
  h.frame(&window);
  assert_eq!(runs.get(), 1);
}

#[test]
fn a_view_may_change_the_list_while_it_is_built() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1]);
  let row = Row::new().children_keyed(
    move || items.get(),
    |n| *n,
    move |_cx, n| {
      if n.get_untracked() == 1 {
        items.update(|list| {
          if !list.contains(&2) {
            list.push(2);
          }
        });
      }
      Text::new(move || n.get().to_string())
    },
  );
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  assert_eq!(texts(&window), ["1", "2"]);
}

const EXIT: Duration = Duration::from_millis(200);

/// What each item's view saw, by key: its `leaving` signal and a signal of
/// its own state.
type Seen = Rc<RefCell<HashMap<u32, (ReadSignal<bool>, Signal<u32>)>>>;

/// A row of `items` that leave over `EXIT`, each showing its key and its
/// own state.
fn exiting_row(items: Signal<Vec<u32>>, seen: &Seen, built: &Counter) -> Row {
  let seen = Rc::clone(seen);
  let built = built.clone();
  Row::new()
    .children_keyed(
      move || items.get(),
      |n| *n,
      move |cx, n| {
        built.hit();
        let state = cx.signal(0);
        seen
          .borrow_mut()
          .insert(n.get_untracked(), (cx.leaving(), state));
        Text::new(move || format!("{} {}", n.get(), state.get()))
      },
    )
    .exit(EXIT)
}

fn leaving(seen: &Seen, key: u32) -> bool {
  seen.borrow()[&key].0.get_untracked()
}

#[test]
fn a_context_outside_an_exiting_item_is_never_leaving() {
  let mut h = Harness::new();
  assert!(!h.cx.leaving().get_untracked());
  let items = h.cx.signal(vec![1]);
  let seen = Seen::default();
  let window = h.window(
    h.panel()
      .child(exiting_row(items, &seen, &Counter::default())),
    400.0,
    30.0,
  );
  assert!(!leaving(&seen, 1), "an item in the list isn't leaving");
  drop(window);
}

#[test]
fn removed_items_stay_while_they_leave_then_go_with_everything_they_made() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1]);
  let seen = Seen::default();
  let window = h.window(
    h.panel()
      .child(exiting_row(items, &seen, &Counter::default())),
    400.0,
    30.0,
  );
  let one = h.runtime.node_count();

  items.set(vec![1, 2, 3]);
  h.frame(&window);
  let bounds: Vec<_> = {
    let view = window.view();
    node_at(view.root(), &[0])
      .children()
      .skip(1)
      .map(|child| child.bounds())
      .collect()
  };
  window.take_damage();

  // Both leave at once and stay where they were, untouched: the list
  // lays nothing out and repaints nothing for them.
  items.set(vec![1]);
  h.frame(&window);
  assert_eq!(texts(&window), ["1 0", "2 0", "3 0"]);
  assert!(leaving(&seen, 2) && leaving(&seen, 3));
  assert!(!leaving(&seen, 1));
  assert!(!window.needs_layout());
  assert!(!window.has_damage());

  let due = next_timer().expect("an exit timer runs");
  fire_timers(due - Duration::from_nanos(1));
  h.frame(&window);
  assert_eq!(texts(&window).len(), 3, "not done yet");

  // Each has its own timer, started a moment apart, so this fires well
  // past both.
  fire_timers(due + EXIT / 2);
  h.frame(&window);
  assert_eq!(texts(&window), ["1 0"]);
  assert_eq!(h.runtime.node_count(), one);
  assert_eq!(next_timer(), None, "nothing left to wake for");
  let damage = window.take_damage();
  for area in bounds {
    assert!(damage.contains(&area), "{area:?} missing from {damage:?}");
  }
}

#[test]
fn a_key_that_comes_back_while_leaving_keeps_its_child_and_state() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2, 3]);
  let seen = Seen::default();
  let built = Counter::default();
  let window = h.window(
    h.panel().child(exiting_row(items, &seen, &built)),
    400.0,
    30.0,
  );
  seen.borrow()[&2].1.set(5);

  items.set(vec![1, 3]);
  h.frame(&window);
  assert!(leaving(&seen, 2));

  // It comes back at its new place, as the same child.
  items.set(vec![2, 1, 3]);
  h.frame(&window);
  assert_eq!(texts(&window), ["2 5", "1 0", "3 0"]);
  assert!(!leaving(&seen, 2));
  assert_eq!(built.get(), 3, "nothing was rebuilt");
  assert_eq!(next_timer(), None, "its exit timer stopped");

  // Leaving again counts the whole exit from then.
  items.set(vec![1, 3]);
  h.frame(&window);
  assert!(leaving(&seen, 2));
  fire_timers(next_timer().expect("its exit timer runs again"));
  h.frame(&window);
  assert_eq!(texts(&window), ["1 0", "3 0"]);
}

#[test]
fn a_leaving_child_stays_right_after_the_child_before_it() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2, 3, 4]);
  let seen = Seen::default();
  let window = h.window(
    h.panel()
      .child(exiting_row(items, &seen, &Counter::default())),
    400.0,
    30.0,
  );

  // 2 follows 1 and 4 follows 3 wherever they move; 5 is new.
  items.set(vec![5, 3, 1]);
  h.frame(&window);
  assert_eq!(texts(&window), ["5 0", "3 0", "4 0", "1 0", "2 0"]);

  // The first child leaving stays first, and one that leaves after a
  // leaving child follows it.
  items.set(vec![3, 1]);
  h.frame(&window);
  assert_eq!(texts(&window), ["5 0", "3 0", "4 0", "1 0", "2 0"]);
  items.set(vec![3]);
  h.frame(&window);
  assert_eq!(texts(&window), ["5 0", "3 0", "4 0", "1 0", "2 0"]);

  fire_timers(Instant::now() + 10 * EXIT);
  h.frame(&window);
  assert_eq!(texts(&window), ["3 0"]);
}

#[test]
fn disposing_the_list_while_items_leave_frees_them_all_at_once() {
  let mut h = Harness::new();
  let outer = h.cx.signal(Vec::<u32>::new());
  let items = h.cx.signal(vec![1, 2, 3]);
  let seen = Seen::default();
  let row = Row::new().children_keyed(
    move || outer.get(),
    |n| *n,
    move |_cx, _n| exiting_row(items, &seen, &Counter::default()),
  );
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  let empty = h.runtime.node_count();

  outer.set(vec![0]);
  h.frame(&window);
  items.set(Vec::new());
  h.frame(&window);
  assert!(next_timer().is_some(), "the items are leaving");

  outer.set(Vec::new());
  h.frame(&window);
  assert_eq!(h.runtime.node_count(), empty);
  assert_eq!(next_timer(), None, "no exit timer is left");
}

#[test]
fn nested_contexts_of_a_leaving_item_read_the_same_signal() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1]);
  let inner = Rc::new(RefCell::new(None));
  let kept = Rc::clone(&inner);
  let row = Row::new()
    .children_keyed(
      move || items.get(),
      |n| *n,
      move |cx, _n| {
        *kept.borrow_mut() = Some(cx.child().child().leaving());
        Text::new("x")
      },
    )
    .exit(EXIT);
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  let inner = inner.borrow().expect("the view ran");
  assert!(!inner.get_untracked());
  items.set(Vec::new());
  h.frame(&window);
  assert!(inner.get_untracked());
}

#[test]
fn a_list_without_an_exit_removes_at_once_and_arms_no_timer() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let seen: Seen = Seen::default();
  let kept = Rc::clone(&seen);
  let row = Row::new().children_keyed(
    move || items.get(),
    |n| *n,
    move |cx, n| {
      let state = cx.signal(0);
      kept
        .borrow_mut()
        .insert(n.get_untracked(), (cx.leaving(), state));
      Text::new(move || format!("{} {}", n.get(), state.get()))
    },
  );
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  assert!(!leaving(&seen, 2));
  items.set(vec![1]);
  h.frame(&window);
  assert_eq!(texts(&window), ["1 0"]);
  assert_eq!(next_timer(), None);
}

#[test]
fn a_panicking_view_in_an_exiting_list_shows_empty_and_still_leaves() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let row = Row::new()
    .children_keyed(
      move || items.get(),
      |n| *n,
      |_cx, n| {
        if n.get_untracked() == 2 {
          panic!("no view for 2");
        }
        Text::new("1")
      },
    )
    .exit(EXIT);
  let mut shown = Vec::new();
  let logs = capture_logs(|| {
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    shown.push(texts(&window));
    items.set(vec![1]);
    h.frame(&window);
    shown.push(texts(&window));
    fire_timers(Instant::now() + 10 * EXIT);
    h.frame(&window);
    shown.push(texts(&window));
  });
  assert_eq!(
    shown,
    [vec!["1", "(not text)"], vec!["1", "(not text)"], vec!["1"]]
  );
  assert!(logs.contains("building an item panicked"), "{logs}");
}

#[test]
fn an_exit_before_any_keyed_list_warns_and_does_nothing() {
  let mut h = Harness::new();
  let mut window = None;
  let logs = capture_logs(|| {
    let row = Row::new().exit(EXIT).child(Text::new("a"));
    window = Some(h.window(h.panel().child(row), 400.0, 30.0));
  });
  assert_eq!(texts(&window.expect("the window was built")), ["a"]);
  assert!(logs.contains("`exit` was called before any"), "{logs}");
}

#[test]
fn an_exit_binding_is_read_untracked_when_a_key_starts_leaving() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2, 3]);
  let reduced = h.cx.signal(false);
  let runs = Counter::default();
  let counted = runs.clone();
  let row = Row::new()
    .children_keyed(
      move || {
        counted.hit();
        items.get()
      },
      |n| *n,
      |_cx, n| Text::new(move || n.get().to_string()),
    )
    .exit(move || if reduced.get() { Duration::ZERO } else { EXIT });
  let window = h.window(h.panel().child(row), 400.0, 30.0);

  items.set(vec![1, 3]);
  h.frame(&window);
  assert_eq!(texts(&window), ["1", "2", "3"], "2 leaves over the exit");
  let due = next_timer().expect("2's exit runs");

  // The binding is read when a key leaves, and reading it reruns nothing.
  let before = runs.get();
  reduced.set(true);
  h.frame(&window);
  assert_eq!(runs.get(), before);
  items.set(vec![1]);
  h.frame(&window);
  assert_eq!(
    texts(&window),
    ["1", "2"],
    "3 goes at once with an exit of 0"
  );
  assert_eq!(next_timer(), Some(due), "and has no timer of its own");
}

#[test]
fn an_exit_of_zero_removes_at_once_like_no_exit() {
  let mut h = Harness::new();
  let items = h.cx.signal(Vec::<u32>::new());
  let seen = Seen::default();
  let kept = Rc::clone(&seen);
  let row = Row::new()
    .children_keyed(
      move || items.get(),
      |n| *n,
      move |cx, n| {
        kept
          .borrow_mut()
          .insert(n.get_untracked(), (cx.leaving(), cx.signal(0)));
        Text::new(move || n.get().to_string())
      },
    )
    .exit(Duration::ZERO);
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  let empty = h.runtime.node_count();
  items.set(vec![1]);
  h.frame(&window);
  let one = seen.borrow()[&1].0;
  items.set(Vec::new());
  h.frame(&window);
  assert!(texts(&window).is_empty());
  assert_eq!(next_timer(), None);
  assert_eq!(h.runtime.node_count(), empty);
  assert_eq!(one.try_get(), None, "it never left: its owner is gone");
}

#[test]
fn a_key_that_leaves_and_comes_back_often_holds_one_timer() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1]);
  let seen = Seen::default();
  let window = h.window(
    h.panel()
      .child(exiting_row(items, &seen, &Counter::default())),
    400.0,
    30.0,
  );
  items.set(Vec::new());
  h.frame(&window);
  items.set(vec![1]);
  h.frame(&window);
  let settled = h.runtime.node_count();
  for _ in 0..10 {
    items.set(Vec::new());
    h.frame(&window);
    items.set(vec![1]);
    h.frame(&window);
  }
  assert_eq!(h.runtime.node_count(), settled);
  assert_eq!(next_timer(), None);
}

#[test]
fn reading_leaving_in_a_binding_outside_an_exiting_item_makes_nothing() {
  let h = Harness::new();
  let tick = h.cx.signal(0);
  let cx = h.cx.clone();
  h.cx.effect(move || {
    tick.get();
    assert!(!cx.leaving().get());
  });
  h.runtime.flush();
  let before = h.runtime.node_count();
  for n in 1..=10 {
    tick.set(n);
    h.runtime.flush();
  }
  assert_eq!(h.runtime.node_count(), before);
}

#[test]
fn an_exit_binding_that_panics_is_logged_and_the_child_goes_at_once() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let row = Row::new()
    .children_keyed(
      move || items.get(),
      |n| *n,
      |_cx, n| Text::new(move || n.get().to_string()),
    )
    .exit(|| -> Duration { panic!("no exit") });
  let mut shown = Vec::new();
  let logs = capture_logs(|| {
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    items.set(vec![1]);
    h.frame(&window);
    shown.push(texts(&window));
    // The list goes on.
    items.set(vec![1, 3]);
    h.frame(&window);
    shown.push(texts(&window));
  });
  assert_eq!(shown, [vec!["1"], vec!["1", "3"]]);
  assert_eq!(next_timer(), None);
  assert!(
    logs.contains("PanelWindow[DP-1]/Row.children: its exit panicked; the child goes at once"),
    "{logs}"
  );
}

const SLIDE: Transition = Transition::new(100).easing(Easing::Linear);

/// A row of `items` that leave over at most `exit`, each sliding 50
/// pixels over `slide` as it leaves.
fn sliding_row(items: Signal<Vec<u32>>, exit: Duration, slide: Transition) -> Row {
  Row::new()
    .children_keyed(
      move || items.get(),
      |n| *n,
      move |cx, n| {
        let leaving = cx.leaving();
        let x = cx.animate(move || if leaving.get() { 50.0 } else { 0.0 }, slide);
        Text::new(move || n.get().to_string()).style(move || Style::new().translate(x.get(), 0.0))
      },
    )
    .exit(exit)
}

/// Ticks the clock at `ms` past `start`, then runs a loop iteration.
fn tick(h: &mut Harness, window: &Window, start: Instant, ms: u64) -> bool {
  let still = advance_animations(start + Duration::from_millis(ms));
  h.frame(window);
  still
}

#[test]
fn a_leaving_child_goes_once_its_animations_end() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1]);
  let window = h.window(
    h.panel().child(sliding_row(items, 10 * EXIT, SLIDE)),
    400.0,
    30.0,
  );
  let one = h.runtime.node_count();
  items.set(vec![1, 2]);
  h.frame(&window);
  items.set(vec![1]);
  h.frame(&window);
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  tick(&mut h, &window, start, 50);
  assert_eq!(texts(&window), ["1", "2"], "still sliding");
  tick(&mut h, &window, start, 100);
  assert_eq!(texts(&window), ["1", "2"], "judged at the next tick");
  assert!(!tick(&mut h, &window, start, 116), "the clock stops");
  assert_eq!(texts(&window), ["1"]);
  assert_eq!(h.runtime.node_count(), one);
  assert_eq!(next_timer(), None, "its exit timer went with it");
  assert!(!animating());
}

#[test]
fn an_exit_shorter_than_the_animation_cuts_it_short() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let window = h.window(
    h.panel()
      .child(sliding_row(items, EXIT, Transition::new(10_000))),
    400.0,
    30.0,
  );
  items.set(vec![1]);
  h.frame(&window);
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  fire_timers(next_timer().expect("its exit timer runs"));
  h.frame(&window);
  assert_eq!(texts(&window), ["1"]);
  assert!(
    !tick(&mut h, &window, start, 16),
    "its animation went with it"
  );
}

#[test]
fn a_leaving_child_with_nothing_animating_goes_at_the_first_tick() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let seen = Seen::default();
  let window = h.window(
    h.panel()
      .child(exiting_row(items, &seen, &Counter::default())),
    400.0,
    30.0,
  );
  items.set(vec![1]);
  h.frame(&window);
  assert!(leaving(&seen, 2));
  assert!(animating(), "the clock ticks once to judge it");
  assert!(!tick(&mut h, &window, Instant::now(), 0));
  assert_eq!(texts(&window), ["1 0"]);
  assert_eq!(next_timer(), None);
}

#[test]
fn an_animation_that_starts_a_frame_late_still_holds_the_child() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let row = Row::new()
    .children_keyed(
      move || items.get(),
      |n| *n,
      |cx, n| {
        let leaving = cx.leaving();
        let slide = cx.animation(move || if leaving.get() { 50.0 } else { 0.0 }, SLIDE);
        // Moves once the slide has ended, in the flush after that tick.
        let fade = cx.animate(
          move || {
            if leaving.get() && !slide.running().get() {
              0.0
            } else {
              1.0
            }
          },
          SLIDE,
        );
        Text::new(move || n.get().to_string()).style(move || {
          Style::new()
            .translate(slide.value().get(), 0.0)
            .opacity(fade.get())
        })
      },
    )
    .exit(10 * EXIT);
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  items.set(vec![1]);
  h.frame(&window);
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  tick(&mut h, &window, start, 100);
  assert!(
    tick(&mut h, &window, start, 116),
    "the fade keeps the clock"
  );
  assert_eq!(texts(&window), ["1", "2"], "the fade holds it");
  tick(&mut h, &window, start, 200);
  assert_eq!(texts(&window), ["1", "2"], "the fade just ended");
  assert!(!tick(&mut h, &window, start, 216));
  assert_eq!(texts(&window), ["1"]);
}

/// A list of one item, in a list of one item, both leaving over at most
/// `10 * EXIT`. The inner item slides as it leaves; its `leaving` is
/// kept in `seen`.
fn nested_rows(outer: Signal<Vec<u32>>, seen: &Rc<RefCell<Option<ReadSignal<bool>>>>) -> Row {
  let seen = Rc::clone(seen);
  Row::new()
    .children_keyed(
      move || outer.get(),
      |n| *n,
      move |_cx, _n| {
        let seen = Rc::clone(&seen);
        Row::new()
          .children_keyed(
            || vec![10_u32],
            |n| *n,
            move |cx, n| {
              let leaving = cx.leaving();
              *seen.borrow_mut() = Some(leaving);
              let x = cx.animate(move || if leaving.get() { 50.0 } else { 0.0 }, SLIDE);
              Text::new(move || n.get().to_string())
                .style(move || Style::new().translate(x.get(), 0.0))
            },
          )
          .exit(10 * EXIT)
      },
    )
    .exit(10 * EXIT)
}

#[test]
fn an_item_inside_a_leaving_item_is_leaving_too() {
  let mut h = Harness::new();
  let outer = h.cx.signal(vec![1]);
  let seen = Rc::new(RefCell::new(None));
  let window = h.window(h.panel().child(nested_rows(outer, &seen)), 400.0, 30.0);
  let inner = seen.borrow().expect("the inner view ran");
  assert!(!inner.get_untracked());

  outer.set(Vec::new());
  h.frame(&window);
  assert!(inner.get_untracked(), "its outer item leaves");
  outer.set(vec![1]);
  h.frame(&window);
  assert!(!inner.get_untracked(), "and comes back");
}

#[test]
fn a_leaving_item_waits_for_the_animations_of_the_items_inside_it() {
  let mut h = Harness::new();
  let outer = h.cx.signal(Vec::new());
  let seen = Rc::new(RefCell::new(None));
  let window = h.window(h.panel().child(nested_rows(outer, &seen)), 400.0, 30.0);
  let empty = h.runtime.node_count();
  outer.set(vec![1]);
  h.frame(&window);
  outer.set(Vec::new());
  h.frame(&window);
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  tick(&mut h, &window, start, 100);
  assert_eq!(texts(&window).len(), 1, "the inner slide just ended");
  assert!(!tick(&mut h, &window, start, 116));
  assert!(texts(&window).is_empty());
  assert_eq!(h.runtime.node_count(), empty);
  assert_eq!(next_timer(), None);
}

/// A row of `items` that leave over at most `10 * EXIT`, each moved out
/// by what `out` makes in its context.
fn leaving_row(items: Signal<Vec<u32>>, out: fn(&Cx) -> Box<dyn Fn() -> Style>) -> Row {
  Row::new()
    .children_keyed(
      move || items.get(),
      |n| *n,
      move |cx, n| {
        let style = out(cx);
        Text::new(move || n.get().to_string()).style(style)
      },
    )
    .exit(10 * EXIT)
}

#[test]
fn a_style_fade_alone_holds_a_leaving_child() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let row = leaving_row(items, |cx| {
    let leaving = cx.leaving();
    Box::new(move || {
      let opacity = if leaving.get() { 0.0 } else { 1.0 };
      Style::new().opacity(opacity).transition(SLIDE)
    })
  });
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  items.set(vec![1]);
  h.frame(&window);
  let start = Instant::now();
  tick(&mut h, &window, start, 0);
  tick(&mut h, &window, start, 50);
  assert_eq!(texts(&window), ["1", "2"], "still fading");
  tick(&mut h, &window, start, 100);
  assert!(!tick(&mut h, &window, start, 116));
  assert_eq!(texts(&window), ["1"]);
}

#[test]
fn a_spring_holds_a_leaving_child_until_it_rests() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1, 2]);
  let row = leaving_row(items, |cx| {
    let leaving = cx.leaving();
    let x = cx.spring(
      move || if leaving.get() { 50.0 } else { 0.0 },
      Spring::default(),
    );
    Box::new(move || Style::new().translate(x.value().get(), 0.0))
  });
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  items.set(vec![1]);
  h.frame(&window);
  let start = Instant::now();
  let mut ms = 0;
  while tick(&mut h, &window, start, ms) {
    if ms == 160 {
      assert_eq!(texts(&window), ["1", "2"], "still springing");
    }
    ms += 16;
    assert!(ms < 1500, "a default spring rests within 1.5 s");
  }
  assert!(ms > 160);
  assert_eq!(texts(&window), ["1"]);
}
