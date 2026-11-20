//! A widget's `mask_region`: what it hands the renderer, what a change
//! repaints, a panicking binding, and what it frees.

use super::{Counter, Harness, capture_logs, node_at};
use crate::{
  Button, Content, Image, Point, PointerButton, Region, Row, Stack, Style, Text, Window,
};

/// A hole `inset` from each side of a 40 by 20 box.
fn holed(inset: i32) -> Region {
  let side = |length: i32| (length - 2 * inset).max(0) as u32;
  Region::rect(0, 0, 40, 20).subtract(Region::rect(inset, inset, side(40), side(20)).radius(4.0))
}

fn mask_at(window: &Window, path: &[usize]) -> Option<Region> {
  node_at(window.view().root(), path).mask_region().cloned()
}

#[test]
fn a_changed_region_repaints_only_the_widget_box_without_a_layout() {
  let mut h = Harness::new();
  let inset = h.cx.signal(4);
  let row = Row::new().child(Text::new("label")).child(
    Stack::new()
      .style(Style::new().width(40.0).height(20.0))
      .mask_region(move || Some(holed(inset.get()))),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(mask_at(&window, &[0, 1]), Some(holed(4)));
  assert_eq!(mask_at(&window, &[0, 0]), None, "unmasked by default");
  window.take_damage();

  inset.set(6);
  h.runtime.flush();
  assert!(!window.needs_layout(), "a region is a paint change");
  let masked = node_at(window.view().root(), &[0, 1]).bounds();
  assert_eq!(window.take_damage(), [masked]);
  assert_eq!(mask_at(&window, &[0, 1]), Some(holed(6)));

  // The same region again repaints nothing.
  inset.set(6);
  h.frame(&window);
  assert!(window.take_damage().is_empty());
}

#[test]
fn a_region_comes_and_goes_with_its_option() {
  let mut h = Harness::new();
  let on = h.cx.signal(false);
  let image = Image::new("a.png")
    .width(40)
    .height(20)
    .mask_region(move || on.get().then(|| Region::ellipse(0, 0, 40, 20)));
  let window = h.window(h.panel().child(Row::new().child(image)), 100.0, 30.0);
  assert_eq!(mask_at(&window, &[0, 0]), None);
  window.take_damage();
  on.set(true);
  h.frame(&window);
  assert_eq!(
    mask_at(&window, &[0, 0]),
    Some(Region::ellipse(0, 0, 40, 20))
  );
  let image = node_at(window.view().root(), &[0, 0]).bounds();
  assert_eq!(window.take_damage(), [image]);
  on.set(false);
  h.frame(&window);
  assert_eq!(mask_at(&window, &[0, 0]), None);
  assert_eq!(window.take_damage(), [image]);
}

#[test]
fn a_panicking_region_is_logged_with_its_path_and_the_last_mask_stays() {
  let mut h = Harness::new();
  let n = h.cx.signal(0);
  let row = Row::new()
    .child(Text::new(move || format!("n is {}", n.get())))
    .child(
      Image::new("a.png")
        .width(40)
        .height(20)
        .mask_region(move || {
          assert!(n.get() != 1, "the region fails");
          Some(holed(n.get()))
        }),
    );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let logs = capture_logs(|| {
    n.set(1);
    h.frame(&window);
  });
  assert!(
    logs.contains("effect PanelWindow[DP-1]/Row/Image[1].mask_region panicked and is disabled")
      && logs.contains("the region fails"),
    "{logs}"
  );
  assert_eq!(
    mask_at(&window, &[0, 1]),
    Some(holed(0)),
    "the last mask stays"
  );
  n.set(2);
  h.frame(&window);
  let view = window.view();
  let Content::Text(text) = node_at(view.root(), &[0, 0]).content() else {
    panic!("no text");
  };
  assert_eq!(text.content(), "n is 2", "the rest keeps running");
  assert_eq!(mask_at(&window, &[0, 1]), Some(holed(0)), "and it stays");
}

#[test]
fn a_dropped_masked_widget_frees_its_binding() {
  let mut h = Harness::new();
  let n = h.cx.signal(4);
  let before = h.runtime.node_count();
  let stack = Stack::new().mask_region(move || Some(holed(n.get())));
  let window = h.window(h.panel().child(stack), 40.0, 20.0);
  assert!(h.runtime.node_count() > before);
  drop(window);
  assert_eq!(h.runtime.node_count(), before);
}

#[test]
fn the_pointer_still_hits_a_hole() {
  let mut h = Harness::new();
  let clicks = Counter::default();
  let counted = clicks.clone();
  let button = Button::new(Stack::new().style(Style::new().width(40.0).height(20.0)))
    .on_click(move |_| counted.hit())
    .mask_region(Some(holed(4)));
  let window = h.window(h.panel().child(Row::new().child(button)), 100.0, 30.0);
  window.pointer_motion(Point::new(20.0, 10.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(clicks.get(), 1);
}
