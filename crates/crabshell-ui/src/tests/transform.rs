//! Translate, scale and rotate: paint fields that move where a widget is
//! drawn and hit, never the layout.

use std::time::{Duration, Instant};

use super::{Harness, node_at, rect_at};
use crate::{
  Align, Button, Color, Easing, Icon, Point, PointerButton, Rect, Row, Style, Transition, Window,
  advance_animations,
};

/// A 20 by 20 box with a background.
const BOX: Style = Style::new()
  .width(20.0)
  .height(20.0)
  .background(Color::rgb(1, 2, 3));

fn close(a: Rect, b: Rect) -> bool {
  [
    (a.x, b.x),
    (a.y, b.y),
    (a.width, b.width),
    (a.height, b.height),
  ]
  .iter()
  .all(|(a, b)| (a - b).abs() < 0.01)
}

/// The bounds of the box at `path`.
fn bounds(window: &Window, path: &[usize]) -> Rect {
  node_at(window.view().root(), path).bounds()
}

/// The style the box at `path` is drawn in.
fn shown(window: &Window, path: &[usize]) -> Style {
  node_at(window.view().root(), path).style()
}

/// A window with one box at (10, 10), styled by `style`.
fn one_box(h: &mut Harness, style: impl Fn() -> Style + 'static) -> Window {
  let row = Row::new()
    .align(Align::Start)
    .padding(10, 10)
    .child(Row::new().style(style));
  h.window(h.panel().child(row), 200.0, 60.0)
}

#[test]
fn transforms_are_const_paint_fields_that_merge_one_by_one() {
  const MOVED: Style = Style::new()
    .translate(3.0, -4.0)
    .scale(1.5)
    .rotate(90.0)
    .origin(0.0, 1.0);
  assert_eq!(MOVED.translate, Some(Point::new(3.0, -4.0)));
  assert_eq!(MOVED.scale, Some(1.5));
  assert_eq!(MOVED.rotate, Some(90.0));
  assert_eq!(MOVED.origin, Some(Point::new(0.0, 1.0)));
  assert!(BOX.same_layout(&BOX.merge(MOVED)), "none of them lays out");
  // A hover that scales keeps the turn under it.
  let hovered = Style::new().rotate(45.0).merge(Style::new().scale(1.1));
  assert_eq!((hovered.rotate, hovered.scale), (Some(45.0), Some(1.1)));
}

#[test]
fn transforms_blend_from_what_an_unset_field_means() {
  let to = Style::new()
    .translate(10.0, 20.0)
    .scale(2.0)
    .rotate(90.0)
    .origin(0.0, 1.0);
  let half = Style::new().lerp(&to, 0.5);
  assert_eq!(half.translate, Some(Point::new(5.0, 10.0)));
  assert_eq!(half.scale, Some(1.5), "unset scale is 1");
  assert_eq!(half.rotate, Some(45.0));
  assert_eq!(
    half.origin,
    Some(Point::new(0.25, 0.75)),
    "unset is the middle"
  );
  assert_eq!(to.lerp(&Style::new(), 0.5).scale, Some(1.5));
  assert_eq!(Style::new().lerp(&Style::new(), 0.5).scale, None);
}

#[test]
fn a_transform_moves_the_painted_area_and_never_the_layout() {
  // Each is grown by a logical pixel, which bilinear filtering may reach.
  let cases = [
    (
      Style::new().translate(5.0, -3.0),
      Rect::new(14.0, 6.0, 22.0, 22.0),
    ),
    // Twice the size around the middle, (20, 20).
    (Style::new().scale(2.0), Rect::new(-2.0, -2.0, 44.0, 44.0)),
    // Around the top-left corner instead.
    (
      Style::new().scale(2.0).origin(0.0, 0.0),
      Rect::new(8.0, 8.0, 44.0, 44.0),
    ),
    // A 22-pixel square turned 45 degrees reaches 11√2 from its middle.
    (
      Style::new().rotate(45.0),
      Rect::new(4.444, 4.444, 31.113, 31.113),
    ),
    // A quarter turn of a square covers the same square.
    (Style::new().rotate(-270.0), Rect::new(9.0, 9.0, 22.0, 22.0)),
  ];
  for (transform, expected) in cases {
    let mut h = Harness::new();
    let window = one_box(&mut h, move || BOX.merge(transform));
    let painted = bounds(&window, &[0, 0]);
    assert_eq!(rect_at(&window, &[0, 0]), Rect::new(10.0, 10.0, 20.0, 20.0));
    assert!(close(painted, expected), "{transform:?}: {painted:?}");
    assert!(
      window
        .take_damage()
        .iter()
        .any(|rect| close(*rect, expected)),
      "{transform:?} damages where it is drawn"
    );
  }
}

#[test]
fn a_scale_of_0_or_a_number_that_isnt_finite_is_handled() {
  let mut h = Harness::new();
  let gone = one_box(&mut h, || BOX.scale(0.0));
  assert!(bounds(&gone, &[0, 0]).is_empty());
  assert!(!gone.take_damage().iter().any(|rect| rect.x == 10.0));

  for odd in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
    let mut h = Harness::new();
    let window = one_box(&mut h, move || {
      BOX
        .scale(odd)
        .rotate(odd)
        .translate(odd, odd)
        .origin(odd, odd)
    });
    let transform = node_at(window.view().root(), &[0, 0]).transform();
    assert_eq!(transform, None, "{odd} counts as unset");
    assert_eq!(bounds(&window, &[0, 0]), Rect::new(10.0, 10.0, 20.0, 20.0));
  }

  // So large it overflows: drawn nowhere, without a panic.
  let mut h = Harness::new();
  let huge = one_box(&mut h, || BOX.scale(3e38).translate(3e38, 0.0));
  assert!(bounds(&huge, &[0, 0]).is_empty());
  // Around the window's corner, so its left edge stays finite and only
  // its right one overflows.
  let mut h = Harness::new();
  let corner = Row::new().style(Style::new().scale(2e38).origin(0.0, 0.0));
  let past = h.window(h.panel().child(corner), 200.0, 60.0);
  assert!(bounds(&past, &[0]).is_empty(), "{:?}", bounds(&past, &[0]));
}

#[test]
fn a_childs_transform_composes_with_its_parents() {
  let mut h = Harness::new();
  let row = Row::new().align(Align::Start).padding(10, 10).child(
    Row::new()
      .style(Style::new().translate(100.0, 0.0))
      .child(Row::new().style(BOX.scale(2.0))),
  );
  let window = h.window(h.panel().child(row), 300.0, 60.0);
  // The child's bounds are in its parent's coordinates, the layout's.
  assert!(close(
    bounds(&window, &[0, 0, 0]),
    Rect::new(-2.0, -2.0, 44.0, 44.0)
  ));
  // The parent moves them, grown once more.
  assert!(close(
    bounds(&window, &[0, 0]),
    Rect::new(97.0, -3.0, 46.0, 46.0)
  ));
  let damage = window.take_damage();
  assert!(
    damage
      .iter()
      .any(|rect| close(*rect, Rect::new(97.0, -3.0, 46.0, 46.0))),
    "the child is repainted where both transforms put it: {damage:?}"
  );
}

#[test]
fn turning_a_widget_repaints_its_old_and_new_area_without_a_layout() {
  let mut h = Harness::new();
  let degrees = h.cx.signal(0.0_f32);
  let row = Row::new()
    .align(Align::Start)
    .padding(10, 10)
    .child(Row::new().style(move || BOX.rotate(degrees.get())))
    .child(Icon::new("still").size(20));
  let window = h.window(h.panel().child(row), 200.0, 60.0);
  window.take_damage();
  let old = bounds(&window, &[0, 0]);

  degrees.set(30.0);
  h.runtime.flush();
  assert!(window.needs_layout(), "it is placed again");
  assert!(!window.tree.borrow().needs_layout, "but not laid out");
  window.layout(&mut h.text);
  let new = bounds(&window, &[0, 0]);
  assert_ne!(old, new);
  let damage = window.take_damage();
  assert!(damage.contains(&old) && damage.contains(&new), "{damage:?}");
  let both = old.union(new);
  assert!(
    damage
      .iter()
      .all(|rect| both.union(*rect) == both && rect.width < 200.0),
    "nothing else repaints: {damage:?}"
  );
  assert!(!window.needs_layout());
}

#[test]
fn a_transform_fades_and_repaints_where_it_passes() {
  let mut h = Harness::new();
  let turned = h.cx.signal(false);
  let window = one_box(&mut h, move || {
    let degrees = if turned.get() { 90.0 } else { 0.0 };
    // Wide, so a quarter turn reaches places neither end covers.
    BOX
      .width(40.0)
      .height(4.0)
      .rotate(degrees)
      .transition(Transition::new(100).easing(Easing::Linear))
  });
  window.take_damage();
  turned.set(true);
  h.frame(&window);
  window.take_damage();
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + Duration::from_millis(50));
  assert!(window.needs_layout(), "each tick places it again");
  window.layout(&mut h.text);
  assert_eq!(shown(&window, &[0, 0]).rotate, Some(45.0));
  let halfway = bounds(&window, &[0, 0]);
  assert!(halfway.height > 30.0, "it is diagonal: {halfway:?}");
  assert!(window.take_damage().contains(&halfway));
  advance_animations(start + Duration::from_millis(100));
  window.layout(&mut h.text);
  assert_eq!(shown(&window, &[0, 0]).rotate, Some(90.0));
}

/// A window with one 20 by 20 button at (10, 10), styled by `style`, and
/// a counter of its clicks.
fn one_button(h: &mut Harness, style: Style) -> (Window, crabshell_reactive::Signal<u32>) {
  let clicks = h.cx.signal(0_u32);
  let row = Row::new().align(Align::Start).padding(10, 10).child(
    Button::new(Icon::new("x").size(20))
      .style(style)
      .on_click(move |_| clicks.update(|n| *n += 1)),
  );
  (h.window(h.panel().child(row), 200.0, 60.0), clicks)
}

fn clicked_at(window: &Window, clicks: crabshell_reactive::Signal<u32>, at: Point) -> bool {
  let before = clicks.get();
  window.pointer_motion(at);
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
  clicks.get() > before
}

#[test]
fn the_pointer_finds_a_turned_button_by_its_turned_shape() {
  let mut h = Harness::new();
  let (window, clicks) = one_button(&mut h, Style::new().rotate(45.0));
  // The box's corner, (11, 11), is outside the diamond it turns into.
  assert!(!clicked_at(&window, clicks, Point::new(11.0, 11.0)));
  // Past the box's right edge, but inside the diamond's right point,
  // which reaches 20 + 10√2.
  assert!(clicked_at(&window, clicks, Point::new(33.0, 20.0)));
  assert!(!clicked_at(&window, clicks, Point::new(35.0, 20.0)));
}

#[test]
fn a_scaled_up_button_is_hit_out_to_its_drawn_edge() {
  let mut h = Harness::new();
  let (window, clicks) = one_button(&mut h, Style::new().scale(2.0));
  // Drawn from (0, 0) to (40, 40) around its middle.
  assert!(clicked_at(&window, clicks, Point::new(39.5, 20.0)));
  assert!(clicked_at(&window, clicks, Point::new(0.5, 0.5)));
  assert!(!clicked_at(&window, clicks, Point::new(40.5, 20.0)));
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(10.0, 10.0, 20.0, 20.0));
}

#[test]
fn a_button_scaled_to_0_is_never_hit() {
  let mut h = Harness::new();
  let (window, clicks) = one_button(&mut h, Style::new().scale(0.0));
  assert!(!clicked_at(&window, clicks, Point::new(20.0, 20.0)));
}

#[test]
fn a_hover_scale_moves_the_button_without_a_layout() {
  let mut h = Harness::new();
  let row = Row::new()
    .align(Align::Start)
    .padding(10, 10)
    .child(Button::new(Icon::new("x").size(20)).hover_style(Style::new().scale(0.5)));
  let window = h.window(h.panel().child(row), 200.0, 60.0);
  // Inside the box, outside the half it shrinks to: hover mustn't flicker.
  window.pointer_motion(Point::new(12.0, 12.0));
  let mut layouts = 0;
  while window.needs_layout() && layouts < 10 {
    assert!(!window.tree.borrow().needs_layout);
    window.layout(&mut h.text);
    layouts += 1;
  }
  assert!(layouts <= 2, "still dirty after {layouts} placings");
}

#[test]
fn a_click_in_a_moved_field_puts_the_caret_under_the_pointer() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let row = Row::new()
    .child(crate::TextInput::new(text).style(Style::new().width(100.0).translate(100.0, 0.0)));
  let window = h.window(h.panel().child(row), 300.0, 30.0);
  window.keyboard_enter();
  // At the field's left edge where it is drawn, past its text where it
  // was laid out.
  window.pointer_motion(Point::new(101.0, 10.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
  window.key(&crate::KeyEvent {
    key: crate::Key::Character('!'),
    text: Some(String::from("!")),
    modifiers: crate::Modifiers::default(),
    repeat: false,
  });
  assert_eq!(text.get(), "!abcdef");
}
