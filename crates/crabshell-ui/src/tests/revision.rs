//! `NodeView::revision`: what a renderer's kept layer is checked against.
//! A node's own opacity and transform leave it; anything else it or
//! anything inside it draws moves it, and every revision around it.

use std::time::{Duration, Instant};

use super::{Harness, node_at};
use crate::{Color, Easing, Row, Stack, Style, Text, Transition, Window, advance_animations};

/// The revisions of the window's root, the layered stack under it and
/// the stack's first child.
fn revisions(window: &Window) -> [u64; 3] {
  let view = window.view();
  let root = view.root();
  [&[][..], &[0], &[0, 0]].map(|path| node_at(root, path).revision())
}

#[test]
fn a_nodes_own_opacity_and_transform_leave_its_revision_and_its_childrens() {
  let mut h = Harness::new();
  let opacity = h.cx.signal(0.5_f32);
  let scale = h.cx.signal(1.0_f32);
  let stack = Stack::new()
    .style(move || Style::new().opacity(opacity.get()).scale(scale.get()))
    .child(
      Row::new().style(
        Style::new()
          .width(20.0)
          .height(20.0)
          .background(Color::rgb(9, 9, 9)),
      ),
    );
  let window = h.window(h.panel().child(stack), 100.0, 40.0);
  let [root, own, child] = revisions(&window);

  opacity.set(0.25);
  h.frame(&window);
  let [after_root, after_own, after_child] = revisions(&window);
  assert_ne!(after_root, root, "a change inside the window");
  assert_eq!((after_own, after_child), (own, child));

  scale.set(0.8);
  h.frame(&window);
  let [moved_root, moved_own, moved_child] = revisions(&window);
  assert_ne!(moved_root, after_root);
  assert_eq!((moved_own, moved_child), (own, child));
}

#[test]
fn a_change_inside_moves_every_revision_around_it() {
  let mut h = Harness::new();
  let dark = h.cx.signal(true);
  let wide = h.cx.signal(false);
  let stack = Stack::new()
    .style(Style::new().opacity(0.5))
    .child(Row::new().style(move || {
      let shade = if dark.get() { 9 } else { 200 };
      Style::new()
        .width(if wide.get() { 40.0 } else { 20.0 })
        .height(20.0)
        .background(Color::rgb(shade, shade, shade))
    }));
  let window = h.window(h.panel().child(stack), 100.0, 40.0);
  let before = revisions(&window);

  dark.set(false);
  h.frame(&window);
  let painted = revisions(&window);
  for (after, before) in painted.iter().zip(before) {
    assert_ne!(*after, before, "a new background");
  }

  wide.set(true);
  h.frame(&window);
  for (after, before) in revisions(&window).iter().zip(painted) {
    assert_ne!(*after, before, "a new size");
  }
}

#[test]
fn a_color_text_inherits_from_outside_moves_the_revisions_inside() {
  let mut h = Harness::new();
  let red = h.cx.signal(false);
  let outer = Row::new()
    .style(move || Style::new().color(Color::rgb(if red.get() { 255 } else { 0 }, 0, 0)))
    .child(
      Stack::new()
        .style(Style::new().opacity(0.5))
        .child(Text::new("ink")),
    );
  let window = h.window(h.panel().child(outer), 100.0, 40.0);
  let layered = |window: &Window| {
    let view = window.view();
    let root = view.root();
    (
      node_at(root, &[0, 0]).revision(),
      node_at(root, &[0, 0, 0]).revision(),
    )
  };
  let before = layered(&window);

  red.set(true);
  h.frame(&window);
  let after = layered(&window);
  assert_ne!(after.0, before.0);
  assert_ne!(after.1, before.1);
}

#[test]
fn a_hidden_or_reordered_child_moves_its_parents_revision() {
  let mut h = Harness::new();
  let shown = h.cx.signal(true);
  let order = h.cx.signal(vec![1, 2]);
  let boxed = |shade: u8| {
    Row::new().style(
      Style::new()
        .width(20.0)
        .height(20.0)
        .background(Color::rgb(shade, shade, shade)),
    )
  };
  let stack = Stack::new()
    .style(Style::new().opacity(0.5))
    .child(boxed(9).visible(move || shown.get()))
    .children_keyed(
      move || order.get(),
      |n| *n,
      move |_, n| boxed(n.get_untracked() as u8 * 50),
    );
  let window = h.window(h.panel().child(stack), 100.0, 40.0);
  let stack_revision = |window: &Window| node_at(window.view().root(), &[0]).revision();
  let before = stack_revision(&window);

  shown.set(false);
  h.frame(&window);
  let hidden = stack_revision(&window);
  assert_ne!(hidden, before);

  // Both stacked in one place, so nothing moves; they swap in z.
  order.set(vec![2, 1]);
  h.frame(&window);
  assert_ne!(stack_revision(&window), hidden);
}

#[test]
fn a_fading_opacity_leaves_the_revision_and_a_fading_color_moves_it() {
  let mut h = Harness::new();
  let faded = h.cx.signal(false);
  let stack = Stack::new()
    .style(move || {
      let (opacity, shade) = if faded.get() { (0.2, 200) } else { (0.9, 9) };
      Style::new()
        .opacity(opacity)
        .background(Color::rgb(shade, shade, shade))
        .width(20.0)
        .height(20.0)
        .transition(Transition::new(100).easing(Easing::Linear))
    })
    .child(Row::new().style(Style::new().width(10.0).height(10.0)));
  let window = h.window(h.panel().child(stack), 100.0, 40.0);
  let own = |window: &Window| node_at(window.view().root(), &[0]).revision();
  let before = own(&window);

  faded.set(true);
  h.frame(&window);
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + Duration::from_millis(50));
  h.frame(&window);
  assert_ne!(own(&window), before, "the background fades too");
}

#[test]
fn a_fade_of_opacity_alone_leaves_the_revision() {
  let mut h = Harness::new();
  let faded = h.cx.signal(false);
  let stack = Stack::new()
    .style(move || {
      Style::new()
        .opacity(if faded.get() { 0.2 } else { 0.9 })
        .background(Color::rgb(9, 9, 9))
        .width(20.0)
        .height(20.0)
        .transition(Transition::new(100).easing(Easing::Linear))
    })
    .child(Row::new().style(Style::new().width(10.0).height(10.0)));
  let window = h.window(h.panel().child(stack), 100.0, 40.0);
  let own = |window: &Window| node_at(window.view().root(), &[0]).revision();
  let before = own(&window);

  faded.set(true);
  h.frame(&window);
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + Duration::from_millis(50));
  h.frame(&window);
  let opacity = node_at(window.view().root(), &[0]).style().opacity;
  assert!(
    opacity.is_some_and(|opacity| (opacity - 0.55).abs() < 1e-4),
    "{opacity:?}"
  );
  assert_eq!(own(&window), before);
}

#[test]
fn a_layered_node_moved_by_its_sibling_gets_a_new_revision() {
  let mut h = Harness::new();
  let wide = h.cx.signal(false);
  let row = Row::new()
    .child(Row::new().style(move || {
      Style::new()
        .width(if wide.get() { 40.0 } else { 20.0 })
        .height(20.0)
    }))
    .child(
      Stack::new()
        .style(Style::new().opacity(0.5))
        .child(Row::new().style(Style::new().width(20.0).height(20.0))),
    );
  let window = h.window(h.panel().child(row), 100.0, 40.0);
  let layered = |window: &Window| {
    let view = window.view();
    let root = view.root();
    (
      node_at(root, &[0, 1]).revision(),
      node_at(root, &[0, 1, 0]).revision(),
    )
  };
  let before = layered(&window);

  wide.set(true);
  h.frame(&window);
  let after = layered(&window);
  assert_ne!(after.0, before.0, "its layer would be drawn where it was");
  assert_ne!(after.1, before.1);
}
