//! Drags that cross a window's edge: a drag going out to the platform,
//! and drags from other windows and programs dropped here.

use std::any::TypeId;
use std::cell::RefCell;
use std::rc::Rc;

use crabshell_reactive::Signal;

use super::{Harness, capture_logs};
use crate::{Align, Button, Drag, DragContent, DragOut, Icon, Point, PointerButton, Row, Window};

fn square() -> Icon {
  Icon::new("x").size(20)
}

/// What happened, in order: a source's drag phases with `dropped`, its
/// release, and drops.
type Events = Rc<RefCell<Vec<String>>>;

fn source(events: &Events, data: impl Fn() -> Option<u32> + 'static) -> Button {
  let (drags, releases) = (Rc::clone(events), Rc::clone(events));
  Button::new(square())
    .drag_data(data)
    .on_drag(move |_, drag: Drag| {
      let line = format!("{:?} {}", drag.phase, drag.dropped);
      drags.borrow_mut().push(line);
    })
    .on_release(move |_| releases.borrow_mut().push("release".into()))
}

fn content_target(events: &Events, hovered: Signal<bool>) -> Button {
  let events = Rc::clone(events);
  Button::new(square())
    .on_drop_content(move |_, content| events.borrow_mut().push(format!("{content:?}")))
    .drop_hovered(hovered)
}

/// A row of `first` at x 0, a gap, and `second` at x 40, 200 by 30.
fn window(h: &mut Harness, first: Button, second: Button) -> Window {
  let row = Row::new()
    .align(Align::Start)
    .child(first)
    .child(square())
    .child(second);
  h.window(h.panel().child(row), 200.0, 30.0)
}

/// Presses at (5, 5) and drags to `to`.
fn drag_to(window: &Window, to: Point) {
  window.pointer_motion(Point::new(5.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_motion(to);
}

#[test]
fn drag_content_drops_on_on_drop_content_in_its_own_window() {
  let mut h = Harness::new();
  let events = Events::default();
  let hovered = h.cx.signal(false);
  let text = Button::new(square()).drag_content(Some(DragContent::Text("hi".into())));
  let window = window(&mut h, text, content_target(&events, hovered));
  drag_to(&window, Point::new(45.0, 5.0));
  assert!(hovered.get_untracked());
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(*events.borrow(), [r#"Text("hi")"#]);
  assert!(!hovered.get_untracked());
}

#[test]
fn a_drag_past_the_edge_goes_out_once_and_a_leave_doesnt_end_it() {
  let mut h = Harness::new();
  let events = Events::default();
  let hovered = h.cx.signal(false);
  let uris = Some(DragContent::Uris(vec!["file:///a".into()]));
  let first = Button::new(square()).drag_content(uris.clone());
  let window = window(&mut h, first, content_target(&events, hovered));
  drag_to(&window, Point::new(45.0, 5.0));
  assert_eq!(window.take_drag_out(), None, "still inside");
  window.pointer_motion(Point::new(45.0, -3.0));
  assert_eq!(
    window.take_drag_out(),
    Some(DragOut {
      carried: TypeId::of::<DragContent>(),
      content: uris,
    })
  );
  assert_eq!(window.take_drag_out(), None, "only once");
  window.pointer_leave();
  assert!(window.dragging_out());
  assert!(!hovered.get_untracked(), "it is over no button here");
}

#[test]
fn ending_a_drag_out_runs_the_release_the_drop_and_the_end_in_order() {
  let mut h = Harness::new();
  let events = Events::default();
  let hovered = h.cx.signal(false);
  let first = source(&events, || Some(7));
  let window = window(&mut h, first, content_target(&events, hovered));
  drag_to(&window, Point::new(250.0, 5.0));
  let out = window.take_drag_out().expect("it went out");
  assert_eq!((out.carried, out.content), (TypeId::of::<u32>(), None));
  window.pointer_leave();
  let log = Rc::clone(&events);
  window.end_drag_out(move |value| {
    let value = value.downcast_ref::<u32>().copied();
    log.borrow_mut().push(format!("drop {value:?}"));
    true
  });
  assert_eq!(
    *events.borrow(),
    [
      "Start false",
      "Move false",
      "release",
      "drop Some(7)",
      "End true"
    ]
  );
  assert!(!window.dragging_out());
}

#[test]
fn a_drag_with_nothing_to_carry_never_goes_out() {
  let mut h = Harness::new();
  let events = Events::default();
  let hovered = h.cx.signal(false);
  let first = source(&events, || None).on_drag(|_, _| {});
  let window = window(&mut h, first, content_target(&events, hovered));
  drag_to(&window, Point::new(250.0, 5.0));
  assert_eq!(window.take_drag_out(), None);
}

#[test]
fn a_source_removed_while_out_drops_nothing_and_runs_nothing() {
  let mut h = Harness::new();
  let events = Events::default();
  let shown = h.cx.signal(true);
  let first = source(&events, || Some(7)).visible(move || shown.get());
  let hovered = h.cx.signal(false);
  let window = window(&mut h, first, content_target(&events, hovered));
  drag_to(&window, Point::new(250.0, 5.0));
  window.take_drag_out().expect("it went out");
  window.pointer_leave();
  shown.set(false);
  h.frame(&window);
  assert!(!window.dragging_out());
  let log = Rc::clone(&events);
  window.end_drag_out(move |_| {
    log.borrow_mut().push("drop".into());
    true
  });
  assert_eq!(*events.borrow(), ["Start false", "Move false"]);
}

#[test]
fn a_drag_from_elsewhere_lights_a_target_of_its_type_and_drops_there() {
  let mut h = Harness::new();
  let events = Events::default();
  let (typed, content) = (h.cx.signal(false), h.cx.signal(false));
  let log = Rc::clone(&events);
  let first = Button::new(square())
    .on_drop(move |_, value: u32| log.borrow_mut().push(format!("u32 {value}")))
    .drop_hovered(typed);
  let window = window(&mut h, first, content_target(&events, content));

  assert!(window.drag_over(Point::new(5.0, 5.0), TypeId::of::<u32>()));
  assert!(typed.get_untracked());
  assert!(!window.drag_over(Point::new(25.0, 5.0), TypeId::of::<u32>()));
  assert!(!typed.get_untracked(), "over the gap");
  let carried = TypeId::of::<DragContent>();
  assert!(
    !window.drag_over(Point::new(5.0, 5.0), carried),
    "wrong type"
  );
  assert!(window.drag_over(Point::new(45.0, 5.0), carried));
  assert!(content.get_untracked());
  window.drag_leave();
  assert!(!content.get_untracked());
  assert_eq!(window.drag_drop(), None, "it left");

  window.drag_over(Point::new(45.0, 5.0), carried);
  let spot = window.drag_drop().expect("dropped on the target");
  assert!(!content.get_untracked(), "the drop lets it go");
  let text = DragContent::Text("dropped".into());
  assert!(window.drop_into(spot, Rc::new(text)));
  assert!(!window.drop_into(spot, Rc::new(3u32)), "not its type");
  assert_eq!(*events.borrow(), [r#"Text("dropped")"#]);
}

#[test]
fn a_target_removed_mid_drag_lets_go_and_takes_no_late_drop() {
  let mut h = Harness::new();
  let events = Events::default();
  let shown = h.cx.signal(true);
  let hovered = h.cx.signal(false);
  let second = content_target(&events, hovered).visible(move || shown.get());
  let window = window(&mut h, Button::new(square()), second);
  let carried = TypeId::of::<DragContent>();
  window.drag_over(Point::new(45.0, 5.0), carried);
  let spot = window.drag_drop().expect("dropped on the target");

  shown.set(false);
  h.frame(&window);
  assert!(!window.drop_into(spot, Rc::new(DragContent::Text("late".into()))));

  shown.set(true);
  h.frame(&window);
  window.drag_over(Point::new(45.0, 5.0), carried);
  assert!(hovered.get_untracked());
  shown.set(false);
  h.frame(&window);
  assert!(!hovered.get_untracked());
  assert!(events.borrow().is_empty());
}

#[test]
fn a_drag_back_over_its_own_window_skips_its_source() {
  let mut h = Harness::new();
  let events = Events::default();
  let log = Rc::clone(&events);
  let first = source(&events, || Some(7))
    .on_drop(move |_, value: u32| log.borrow_mut().push(format!("own {value}")));
  let window = window(&mut h, first, Button::new(square()));
  drag_to(&window, Point::new(250.0, 5.0));
  window.take_drag_out().expect("it went out");
  window.pointer_leave();
  assert!(!window.drag_over(Point::new(5.0, 5.0), TypeId::of::<u32>()));
}

#[test]
fn a_panicking_on_drop_content_is_logged_with_its_path_and_takes_no_more() {
  let mut h = Harness::new();
  let hovered = h.cx.signal(false);
  let broken = Button::new(square())
    .on_drop_content(|_, _| panic!("drop failure"))
    .drop_hovered(hovered);
  let window = window(&mut h, Button::new(square()), broken);
  let carried = TypeId::of::<DragContent>();
  window.drag_over(Point::new(45.0, 5.0), carried);
  let spot = window.drag_drop().expect("dropped on the target");
  let logs = capture_logs(|| {
    let value = Rc::new(DragContent::Text("x".into()));
    assert!(!window.drop_into(spot, value), "it didn't land");
  });
  assert!(
    logs
      .contains("handler PanelWindow[DP-1]/Row/Button[2].on_drop_content panicked and is disabled"),
    "{logs}"
  );
  assert!(!window.drag_over(Point::new(45.0, 5.0), carried));
  assert!(!hovered.get_untracked());
}

#[test]
fn a_drag_that_went_out_still_moves_back_over_its_own_window() {
  let mut h = Harness::new();
  let events = Events::default();
  let hovered = h.cx.signal(false);
  let first = source(&events, || Some(7));
  let window = window(&mut h, first, content_target(&events, hovered));
  drag_to(&window, Point::new(250.0, 5.0));
  window.take_drag_out().expect("it went out");
  window.pointer_leave();
  window.drag_out_moved(Point::new(60.0, 5.0));
  window.end_drag_out(|_| false);
  assert_eq!(
    *events.borrow(),
    [
      "Start false",
      "Move false",
      "Move false",
      "release",
      "End false"
    ]
  );
}

#[test]
fn a_target_hidden_under_a_still_drag_no_longer_takes_it() {
  let mut h = Harness::new();
  let events = Events::default();
  let (shown, hovered) = (h.cx.signal(true), h.cx.signal(false));
  let second = content_target(&events, hovered).visible(move || shown.get());
  let window = window(&mut h, Button::new(square()), second);
  assert!(!window.drag_takes(), "no drag yet");
  window.drag_over(Point::new(45.0, 5.0), TypeId::of::<DragContent>());
  assert!(window.drag_takes());
  shown.set(false);
  h.frame(&window);
  assert!(!window.drag_takes());
  assert!(!hovered.get_untracked());
}
