//! Drag and drop inside one window: a button's `drag_data` carried to
//! another's `on_drop`.

use std::cell::RefCell;
use std::rc::Rc;

use crabshell_reactive::Signal;

use super::{Harness, capture_logs};
use crate::{Align, Button, Drag, DragPhase, Icon, Point, PointerButton, Row, Window};

fn square() -> Icon {
  Icon::new("x").size(20)
}

/// What a target was given, in order.
type Drops = Rc<RefCell<Vec<u32>>>;

/// A target that takes `u32`s, writing them to `drops`.
fn target(drops: &Drops, hovered: Signal<bool>) -> Button {
  let drops = Rc::clone(drops);
  Button::new(square())
    .on_drop(move |_cx, value: u32| drops.borrow_mut().push(value))
    .drop_hovered(hovered)
}

/// A row of a source carrying `7` at x 0, a gap, and `target` at x 40,
/// and the source's drags.
fn window(h: &mut Harness, target: Button) -> (Window, Rc<RefCell<Vec<Drag>>>) {
  let drags = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&drags);
  let source = Button::new(square())
    .drag_data(Some(7u32))
    .on_drag(move |_, drag| log.borrow_mut().push(drag));
  let row = Row::new()
    .align(Align::Start)
    .child(source)
    .child(square())
    .child(target);
  (h.window(h.panel().child(row), 200.0, 30.0), drags)
}

/// Presses the source and drags it to `to`.
fn drag_to(window: &Window, to: Point) {
  window.pointer_motion(Point::new(5.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_motion(to);
}

#[test]
fn a_drag_released_over_a_target_drops_its_value_there() {
  let mut h = Harness::new();
  let drops = Drops::default();
  let hovered = h.cx.signal(false);
  let (window, drags) = window(&mut h, target(&drops, hovered));

  drag_to(&window, Point::new(25.0, 5.0));
  assert!(!hovered.get_untracked(), "over the gap");
  window.pointer_motion(Point::new(45.0, 5.0));
  assert!(hovered.get_untracked(), "over the target");
  assert!(
    drops.borrow().is_empty(),
    "nothing drops before the release"
  );

  window.pointer_button(PointerButton::Left, false);
  assert_eq!(*drops.borrow(), [7]);
  assert!(!hovered.get_untracked());
  let end = *drags.borrow().last().expect("the drag ended");
  assert_eq!((end.phase, end.dropped), (DragPhase::End, true));
  assert!(
    drags
      .borrow()
      .iter()
      .rev()
      .skip(1)
      .all(|drag| !drag.dropped)
  );
}

#[test]
fn a_drag_released_anywhere_else_drops_nothing() {
  let mut h = Harness::new();
  let drops = Drops::default();
  let hovered = h.cx.signal(false);
  let (window, drags) = window(&mut h, target(&drops, hovered));
  drag_to(&window, Point::new(45.0, 5.0));
  window.pointer_motion(Point::new(25.0, 5.0));
  assert!(!hovered.get_untracked(), "it left the target");
  window.pointer_button(PointerButton::Left, false);
  assert!(drops.borrow().is_empty());
  assert!(!drags.borrow().last().expect("the drag ended").dropped);
}

#[test]
fn a_target_takes_only_its_own_type() {
  let mut h = Harness::new();
  let words = Rc::new(RefCell::new(Vec::<String>::new()));
  let hovered = h.cx.signal(false);
  let kept = Rc::clone(&words);
  let strings = Button::new(square())
    .on_drop(move |_cx, value: String| kept.borrow_mut().push(value))
    .drop_hovered(hovered);
  let (window, drags) = window(&mut h, strings);
  drag_to(&window, Point::new(45.0, 5.0));
  assert!(!hovered.get_untracked(), "a u32 isn't a String");
  window.pointer_button(PointerButton::Left, false);
  assert!(words.borrow().is_empty());
  assert!(!drags.borrow().last().expect("the drag ended").dropped);
}

#[test]
fn a_drag_without_data_drops_nothing_and_a_binding_is_read_as_it_changes() {
  let mut h = Harness::new();
  let drops = Drops::default();
  let hovered = h.cx.signal(false);
  let carried = h.cx.signal(None::<u32>);
  let source = Button::new(square()).drag_data(move || carried.get());
  let row = Row::new()
    .align(Align::Start)
    .child(source)
    .child(square())
    .child(target(&drops, hovered));
  let window = h.window(h.panel().child(row), 200.0, 30.0);

  drag_to(&window, Point::new(45.0, 5.0));
  assert!(!hovered.get_untracked(), "it carries nothing");
  window.pointer_button(PointerButton::Left, false);
  assert!(drops.borrow().is_empty());

  carried.set(Some(3));
  h.frame(&window);
  drag_to(&window, Point::new(45.0, 5.0));
  assert!(hovered.get_untracked());
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(*drops.borrow(), [3], "a source with only data drags too");
}

#[test]
fn a_target_removed_or_hidden_mid_drag_lets_go() {
  for hide in [false, true] {
    let mut h = Harness::new();
    let drops = Drops::default();
    let hovered = h.cx.signal(false);
    let shown = h.cx.signal(true);
    let items = h.cx.signal(vec![0]);
    let source = Button::new(square()).drag_data(Some(7u32));
    let drops_in = Rc::clone(&drops);
    let row = Row::new()
      .align(Align::Start)
      .child(source)
      .child(square())
      .children_keyed(
        move || items.get(),
        |n| *n,
        move |_cx, _n| target(&drops_in, hovered).visible(move || shown.get()),
      );
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    drag_to(&window, Point::new(45.0, 5.0));
    assert!(hovered.get_untracked());
    if hide {
      shown.set(false);
    } else {
      items.set(Vec::new());
    }
    // At once, not only once a layout looks again.
    h.runtime.flush();
    assert!(!hovered.get_untracked(), "hidden {hide}: the target let go");
    h.frame(&window);
    window.pointer_button(PointerButton::Left, false);
    assert!(drops.borrow().is_empty(), "hidden {hide}: nothing dropped");
  }
}

#[test]
fn a_source_disabled_or_removed_mid_drag_drops_nothing() {
  for remove in [false, true] {
    let mut h = Harness::new();
    let drops = Drops::default();
    let hovered = h.cx.signal(false);
    let enabled = h.cx.signal(true);
    let items = h.cx.signal(vec![0]);
    let drags = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&drags);
    let row = Row::new()
      .align(Align::Start)
      .children_keyed(
        move || items.get(),
        |n| *n,
        move |_cx, _n| {
          let log = Rc::clone(&log);
          Button::new(square())
            .drag_data(Some(7u32))
            .enabled(move || enabled.get())
            .on_drag(move |_, drag| log.borrow_mut().push(drag))
        },
      )
      .child(square())
      .child(target(&drops, hovered));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    drag_to(&window, Point::new(45.0, 5.0));
    assert!(hovered.get_untracked());
    if remove {
      items.set(Vec::new());
    } else {
      enabled.set(false);
    }
    h.runtime.flush();
    assert!(
      !hovered.get_untracked(),
      "removed {remove}: the target let go"
    );
    h.frame(&window);
    window.pointer_button(PointerButton::Left, false);
    assert!(
      drops.borrow().is_empty(),
      "removed {remove}: nothing dropped"
    );
    if !remove {
      let end = *drags.borrow().last().expect("disabling ended the drag");
      assert_eq!((end.phase, end.dropped), (DragPhase::End, false));
    }
  }
}

#[test]
fn a_source_moved_under_the_pointer_still_finds_the_target_below() {
  let mut h = Harness::new();
  let drops = Drops::default();
  let hovered = h.cx.signal(false);
  let at = h.cx.signal(Point::default());
  let source = Button::new(square())
    .style(move || {
      let at = at.get();
      crate::Style::new().translate(at.x, at.y)
    })
    .drag_data(Some(7u32))
    .on_drag(move |_, drag| {
      at.set(Point::new(
        drag.position.x - drag.start.x,
        drag.position.y - drag.start.y,
      ))
    });
  // The source comes last, so it is drawn over the target.
  let row = Row::new()
    .align(Align::Start)
    .child(target(&drops, hovered))
    .child(square())
    .child(source);
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(45.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_motion(Point::new(5.0, 5.0));
  h.frame(&window);
  window.pointer_motion(Point::new(6.0, 5.0));
  assert!(
    hovered.get_untracked(),
    "the source drawn on top is skipped"
  );
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(*drops.borrow(), [7]);
}

#[test]
fn a_panicking_on_drop_is_logged_with_its_path_and_the_shell_runs_on() {
  let mut h = Harness::new();
  let hovered = h.cx.signal(false);
  let clicks = h.cx.signal(0);
  let broken = Button::new(square())
    .on_drop(|_cx, _value: u32| panic!("drop failure"))
    .drop_hovered(hovered)
    .on_click(move |_| clicks.update(|n| *n += 1));
  let (window, drags) = window(&mut h, broken);
  let logs = capture_logs(|| {
    drag_to(&window, Point::new(45.0, 5.0));
    window.pointer_button(PointerButton::Left, false);
  });
  assert!(
    logs.contains("handler PanelWindow[DP-1]/Row/Button[2].on_drop panicked and is disabled"),
    "{logs}"
  );
  let end = *drags
    .borrow()
    .last()
    .expect("the source still hears its end");
  assert_eq!(end.phase, DragPhase::End);
  window.pointer_motion(Point::new(45.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(clicks.get_untracked(), 1, "the target still clicks");
}

#[test]
fn a_panicking_drag_data_binding_is_logged_with_its_path() {
  let mut h = Harness::new();
  let logs = capture_logs(|| {
    let source = Button::new(square()).drag_data(|| -> Option<u32> { panic!("no data") });
    let _window = h.window(h.panel().child(source), 200.0, 30.0);
  });
  assert!(
    logs.contains("PanelWindow[DP-1]/Button.drag_data"),
    "{logs}"
  );
}

#[test]
fn a_turned_off_target_takes_nothing_even_through_a_button_inside_it() {
  let mut h = Harness::new();
  let drops = Drops::default();
  let hovered = h.cx.signal(false);
  let kept = Rc::clone(&drops);
  let off = Button::new(Button::new(square()).on_click(|_| {}))
    .on_drop(move |_cx, value: u32| kept.borrow_mut().push(value))
    .drop_hovered(hovered)
    .enabled(false);
  let (window, drags) = window(&mut h, off);
  drag_to(&window, Point::new(45.0, 5.0));
  assert!(!hovered.get_untracked());
  window.pointer_button(PointerButton::Left, false);
  assert!(drops.borrow().is_empty());
  assert!(!drags.borrow().last().expect("the drag ended").dropped);
}

#[test]
fn leaving_the_window_mid_drag_lets_the_target_go_and_drops_nothing() {
  let mut h = Harness::new();
  let drops = Drops::default();
  let hovered = h.cx.signal(false);
  let (window, drags) = window(&mut h, target(&drops, hovered));
  drag_to(&window, Point::new(45.0, 5.0));
  assert!(hovered.get_untracked());
  window.pointer_leave();
  assert!(!hovered.get_untracked());
  window.pointer_button(PointerButton::Left, false);
  assert!(drops.borrow().is_empty());
  let end = *drags.borrow().last().expect("leaving ended the drag");
  assert_eq!((end.phase, end.dropped), (DragPhase::End, false));
}

#[test]
fn a_drag_that_starts_at_the_press_inside_a_target_is_over_it_at_once() {
  let mut h = Harness::new();
  let drops = Drops::default();
  let hovered = h.cx.signal(false);
  let kept = Rc::clone(&drops);
  let zone = Button::new(Button::new(square()).drag_data(Some(7u32)))
    .on_drop(move |_cx, value: u32| kept.borrow_mut().push(value))
    .drop_hovered(hovered);
  let row = Row::new().align(Align::Start).child(zone);
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(5.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  assert!(
    hovered.get_untracked(),
    "the source is skipped, not the zone it is in"
  );
}

#[test]
fn a_target_whose_on_drop_panicked_reports_no_drop_and_takes_no_more() {
  let mut h = Harness::new();
  let hovered = h.cx.signal(false);
  let broken = Button::new(square())
    .on_drop(|_cx, _value: u32| panic!("drop failure"))
    .drop_hovered(hovered);
  let (window, drags) = window(&mut h, broken);
  capture_logs(|| {
    drag_to(&window, Point::new(45.0, 5.0));
    window.pointer_button(PointerButton::Left, false);
  });
  let end = *drags.borrow().last().expect("the first drag ended");
  assert_eq!(
    (end.phase, end.dropped),
    (DragPhase::End, false),
    "the drop that panicked didn't land"
  );

  drag_to(&window, Point::new(45.0, 5.0));
  assert!(!hovered.get_untracked(), "a disabled on_drop takes nothing");
  window.pointer_button(PointerButton::Left, false);
  let end = *drags.borrow().last().expect("the second drag ended");
  assert_eq!((end.phase, end.dropped), (DragPhase::End, false));
}
