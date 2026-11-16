//! Invalidation: paint changes repaint one widget, layout changes repaint
//! what moved, and nothing changing means nothing to do.

use super::{Harness, rect_at};
use crate::{Align, Button, Color, Icon, Rect, Row, Size, Spacer, Stack, Style, Text};

#[test]
fn the_first_layout_damages_every_node_and_then_nothing_is_dirty() {
  let mut h = Harness::new();
  let row = Row::new().child(Icon::new("a").size(10));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let damage = window.take_damage();
  assert!(damage.contains(&Rect::new(0.0, 0.0, 200.0, 30.0)));
  assert!(damage.contains(&rect_at(&window, &[0, 0])));

  h.frame(&window);
  assert!(!window.needs_layout());
  assert!(!window.has_damage());
}

#[test]
fn a_paint_change_repaints_only_that_widget() {
  let mut h = Harness::new();
  let dark = h.cx.signal(true);
  let row = Row::new()
    .padding(4, 8)
    .child(Icon::new("a").size(10))
    .child(Button::new(Icon::new("b").size(10)).style(move || {
      let background = if dark.get() { 0x11 } else { 0xee };
      Style::new().background(Color::rgb(background, background, background))
    }));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.take_damage();

  dark.set(false);
  h.runtime.flush();
  assert!(!window.needs_layout());
  assert_eq!(window.take_damage(), vec![rect_at(&window, &[0, 1])]);
}

#[test]
fn setting_an_equal_value_invalidates_nothing() {
  let mut h = Harness::new();
  let label = h.cx.signal(String::from("same"));
  let style = h.cx.signal(Style::new().background(Color::rgb(1, 2, 3)));
  let row = Row::new()
    .style(move || style.get())
    .child(Text::new(move || label.get()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.take_damage();

  label.set(String::from("same"));
  style.set(Style::new().background(Color::rgb(1, 2, 3)));
  h.runtime.flush();
  assert!(!window.needs_layout());
  assert!(!window.has_damage());
}

#[test]
fn a_layout_change_repaints_what_moved_at_both_positions() {
  let mut h = Harness::new();
  let size = h.cx.signal(10);
  let row = Row::new()
    .align(Align::Start)
    .child(Icon::new("before").size(8))
    .child(Icon::new("grows").size(move || size.get()))
    .child(Icon::new("after").size(8));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.take_damage();
  let before = rect_at(&window, &[0, 0]);
  let old_grows = rect_at(&window, &[0, 1]);
  let old_after = rect_at(&window, &[0, 2]);

  size.set(20);
  h.runtime.flush();
  assert!(window.needs_layout());
  assert!(!window.has_damage());
  window.layout(&mut h.text);

  let new_grows = rect_at(&window, &[0, 1]);
  let new_after = rect_at(&window, &[0, 2]);
  assert_eq!(new_after.x, old_after.x + 10.0);
  let damage = window.take_damage();
  for rect in [old_grows, new_grows, old_after, new_after] {
    assert!(damage.contains(&rect), "{rect:?} missing from {damage:?}");
  }
  assert!(!damage.contains(&before), "{damage:?}");
  assert!(!window.needs_layout());
}

#[test]
fn new_text_is_shaped_and_measured_again() {
  let mut h = Harness::new();
  let label = h.cx.signal(String::from("a"));
  let row = Row::new()
    .align(Align::Start)
    .child(Text::new(move || label.get()))
    .child(Icon::new("after").size(8));
  let window = h.window(h.panel().child(row), 400.0, 30.0);
  let short = rect_at(&window, &[0, 0]);

  label.set(String::from("a much longer label"));
  h.frame(&window);
  let long = rect_at(&window, &[0, 0]);
  assert!(long.width > short.width + 50.0, "{short:?} {long:?}");
  assert_eq!(rect_at(&window, &[0, 1]).x, long.right());
  let view = window.view();
  let crate::Content::Text(text) = super::node_at(view.root(), &[0, 0]).content() else {
    panic!("a text node has text content");
  };
  let drawn: Vec<&str> = text.runs().map(|run| run.text).collect();
  assert_eq!(drawn, ["a much longer label"]);
}

#[test]
fn a_layout_change_that_moves_nothing_still_repaints_its_widget() {
  let mut h = Harness::new();
  let label = h.cx.signal(String::from("ab"));
  let row = Row::new()
    .align(Align::Start)
    .child(Text::new(move || label.get()))
    .child(Icon::new("after").size(8));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.take_damage();
  let text = rect_at(&window, &[0, 0]);

  // The same glyphs in another order: the same width.
  label.set(String::from("ba"));
  h.frame(&window);
  assert_eq!(rect_at(&window, &[0, 0]), text);
  // Its bounds: the text's rectangle and the ink allowance around it.
  let bounds = super::node_at(window.view().root(), &[0, 0]).bounds();
  assert!(
    bounds.x < text.x && bounds.right() > text.right(),
    "{bounds:?}"
  );
  assert_eq!(window.take_damage(), vec![bounds]);
}

#[test]
fn a_style_change_to_padding_needs_a_layout() {
  let mut h = Harness::new();
  let wide = h.cx.signal(false);
  let button = Button::new(Icon::new("a").size(10)).style(move || {
    Style::new()
      .background(Color::rgb(1, 1, 1))
      .padding(0, if wide.get() { 8 } else { 2 })
  });
  let window = h.window(h.panel().child(Row::new().child(button)), 200.0, 30.0);
  window.take_damage();
  assert_eq!(rect_at(&window, &[0, 0]).width, 14.0);

  wide.set(true);
  h.runtime.flush();
  assert!(window.needs_layout());
  window.layout(&mut h.text);
  assert_eq!(rect_at(&window, &[0, 0]).width, 26.0);
  assert!(window.has_damage());
}

#[test]
fn a_change_to_one_sides_padding_lays_out_and_repaints_both_places() {
  let mut h = Harness::new();
  let wide = h.cx.signal(false);
  let button = Button::new(Icon::new("a").size(10)).style(move || {
    Style::new()
      .background(Color::rgb(1, 1, 1))
      .padding(0, 2)
      .padding_left(if wide.get() { 8 } else { 2 })
  });
  let window = h.window(h.panel().child(Row::new().child(button)), 200.0, 30.0);
  window.take_damage();
  assert_eq!(rect_at(&window, &[0, 0, 0]).x, 2.0);

  wide.set(true);
  h.runtime.flush();
  assert!(window.needs_layout());
  window.layout(&mut h.text);
  assert_eq!(rect_at(&window, &[0, 0]).width, 20.0);
  assert_eq!(rect_at(&window, &[0, 0, 0]).x, 8.0);
  let damage = window.take_damage();
  let reach = damage
    .iter()
    .fold(Rect::default(), |all, rect| all.union(*rect));
  assert_eq!(reach, Rect::new(0.0, 0.0, 20.0, 30.0), "{damage:?}");
}

#[test]
fn a_font_change_on_an_ancestor_measures_its_text_again() {
  use crate::Font;

  let mut h = Harness::new();
  let big = h.cx.signal(false);
  let row = Row::new()
    .align(Align::Start)
    .style(move || Style::new().font(Font::new("sans-serif", if big.get() { 20.0 } else { 10.0 })))
    .child(Text::new("hello"));
  let window = h.window(h.panel().child(row), 400.0, 40.0);
  let small = rect_at(&window, &[0, 0]);
  assert_eq!(small.height, 12.0);

  big.set(true);
  h.frame(&window);
  let large = rect_at(&window, &[0, 0]);
  assert_eq!(large.height, 24.0);
  assert!(large.width > small.width, "{small:?} {large:?}");
}

#[test]
fn a_new_size_lays_out_again_and_repaints_old_and_new_area() {
  let mut h = Harness::new();
  let window = h.window(h.panel().child(Row::new()), 200.0, 30.0);
  window.take_damage();

  window.set_size(Size::new(200.0, 30.0));
  assert!(!window.needs_layout());

  window.set_size(Size::new(300.0, 30.0));
  assert!(window.needs_layout());
  window.layout(&mut h.text);
  let damage = window.take_damage();
  assert!(damage.contains(&Rect::new(0.0, 0.0, 200.0, 30.0)));
  assert!(damage.contains(&Rect::new(0.0, 0.0, 300.0, 30.0)));
}

#[test]
fn a_color_change_repaints_the_descendants_that_inherit_it() {
  let mut h = Harness::new();
  let red = h.cx.signal(false);
  // The row fills the 30-pixel window; its text overflows it.
  let row = Row::new()
    .style(move || Style::new().color(Color::rgb(if red.get() { 255 } else { 0 }, 0, 0)))
    .child(Button::new(Text::new("overflowing text")));
  let window = h.window(h.panel().child(row), 30.0, 30.0);
  window.take_damage();
  let text = rect_at(&window, &[0, 0, 0]);
  assert_eq!(rect_at(&window, &[0]).width, 30.0);
  assert!(text.right() > 30.0, "{text:?}");

  red.set(true);
  h.runtime.flush();
  let damage = window.take_damage();
  assert_eq!(damage.len(), 1);
  assert!(damage[0].right() >= text.right(), "{damage:?} {text:?}");
}

#[test]
fn empty_and_repeated_areas_are_left_out() {
  let mut h = Harness::new();
  let row = Row::new()
    .child(Spacer::new())
    .child(Stack::new().child(Icon::new("a").size(10)));
  let window = h.window(h.panel().child(row), 10.0, 10.0);
  // The spacer is 0 wide, and the stack and its icon share a rectangle.
  let damage = window.take_damage();
  assert_eq!(damage, vec![Rect::new(0.0, 0.0, 10.0, 10.0)]);
}

/// The top and bottom of the lines `path`'s text draws.
fn line_box(window: &crate::Window, path: &[usize]) -> (f32, f32) {
  let view = window.view();
  let crate::Content::Text(text) = super::node_at(view.root(), path).content() else {
    panic!("a text node has text content");
  };
  let top = text.origin().y;
  (
    top,
    top + text.runs().map(|run| run.line_height).sum::<f32>(),
  )
}

/// The smallest rectangle covering all of `damage`.
fn covered(damage: &[Rect]) -> Rect {
  damage
    .iter()
    .fold(Rect::default(), |all, rect| all.union(*rect))
}

#[test]
fn text_taller_than_its_box_is_repainted_where_it_is_drawn() {
  let mut h = Harness::new();
  let label = h.cx.signal(String::from("gy"));
  // A 20-pixel bar with 4 pixels of padding leaves 12 pixels for a
  // 16-pixel line, which overflows by 2 pixels above and below.
  let row = Row::new()
    .padding(4, 8)
    .child(Text::new(move || label.get()));
  let window = h.window(h.panel().child(row), 200.0, 20.0);
  window.take_damage();
  let (top, bottom) = line_box(&window, &[0, 0]);
  assert!(top < rect_at(&window, &[0, 0]).y, "the line overflows");

  label.set(String::from("yg"));
  h.frame(&window);
  let damage = covered(&window.take_damage());
  assert!(damage.y <= top && damage.bottom() >= bottom, "{damage:?}");
}

#[test]
fn a_paint_change_covers_the_texts_overflow() {
  let mut h = Harness::new();
  let red = h.cx.signal(false);
  let row = Row::new().padding(4, 8).child(
    Text::new("gy")
      .style(move || Style::new().color(Color::rgb(if red.get() { 255 } else { 0 }, 0, 0))),
  );
  let window = h.window(h.panel().child(row), 200.0, 20.0);
  window.take_damage();
  let (top, bottom) = line_box(&window, &[0, 0]);

  red.set(true);
  h.runtime.flush();
  let damage = covered(&window.take_damage());
  assert!(damage.y <= top && damage.bottom() >= bottom, "{damage:?}");
}

#[test]
fn the_damage_covers_every_glyph_pixel() {
  use cosmic_text::SwashCache;

  let mut h = Harness::new();
  // A capital with a ring and an accent above, and letters with descenders.
  let label = h.cx.signal(String::from("\u{1FA}gjy"));
  let row = Row::new()
    .align(Align::Center)
    .child(Text::new(move || label.get()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.take_damage();

  // The same glyphs in another order, so the rectangle stays put.
  label.set(String::from("yjg\u{1FA}"));
  h.frame(&window);
  let damage = covered(&window.take_damage());

  let mut cache = SwashCache::new();
  let view = window.view();
  let crate::Content::Text(text) = super::node_at(view.root(), &[0, 0]).content() else {
    panic!("a text node has text content");
  };
  let (mut top, mut bottom) = (f32::MAX, f32::MIN);
  for run in text.runs() {
    let baseline = (text.origin().y + run.line_y).round();
    for glyph in run.glyphs {
      let key = glyph.physical((0.0, 0.0), 1.0).cache_key;
      if let Some(image) = cache.get_image_uncached(&mut h.text.font_system(), key) {
        let glyph_top = baseline - image.placement.top as f32;
        top = top.min(glyph_top);
        bottom = bottom.max(glyph_top + image.placement.height as f32);
      }
    }
  }
  assert!(top < bottom, "some glyph was drawn");
  assert!(
    damage.y <= top && damage.bottom() >= bottom,
    "glyphs {top}..{bottom}, {damage:?}"
  );
}

#[test]
fn an_icon_bigger_than_its_box_is_repainted_whole() {
  let mut h = Harness::new();
  let first = h.cx.signal(true);
  // 36 pixels with 10 of padding above and below leave a 16-pixel box for
  // a 24-pixel icon, centered: it spans 6 to 30.
  let row = Row::new().padding(10, 4).child(
    Icon::new(move || crate::IconSource::Name(if first.get() { "one" } else { "two" }.to_owned()))
      .size(24),
  );
  let window = h.window(h.panel().child(row), 60.0, 36.0);
  window.take_damage();
  first.set(false);
  h.runtime.flush();
  let damage = covered(&window.take_damage());
  assert!(damage.y <= 6.0 && damage.bottom() >= 30.0, "{damage:?}");
  assert!(damage.x <= 4.0 && damage.right() >= 28.0, "{damage:?}");
}

/// The logical area the glyphs of `path`'s text cover at `scale`, placed
/// as the renderer places them.
fn ink(window: &crate::Window, text: &mut crate::TextSystem, path: &[usize], scale: f32) -> Rect {
  use cosmic_text::SwashCache;

  let view = window.view();
  let crate::Content::Text(view_text) = super::node_at(view.root(), path).content() else {
    panic!("a text node has text content");
  };
  let origin = view_text.origin();
  let mut cache = SwashCache::new();
  let mut ink = Rect::default();
  for run in view_text.runs() {
    let baseline = ((origin.y + run.line_y) * scale).round();
    for glyph in run.glyphs {
      let physical = glyph.physical((origin.x * scale, baseline), scale);
      let Some(image) = cache.get_image_uncached(&mut text.font_system(), physical.cache_key)
      else {
        continue;
      };
      let left = (physical.x + image.placement.left) as f32;
      let top = (physical.y - image.placement.top) as f32;
      let (width, height) = (image.placement.width as f32, image.placement.height as f32);
      if width > 0.0 && height > 0.0 {
        ink = ink.union(Rect::new(
          left / scale,
          top / scale,
          width / scale,
          height / scale,
        ));
      }
    }
  }
  ink
}

fn covers(outer: Rect, inner: Rect) -> bool {
  outer.x <= inner.x
    && outer.y <= inner.y
    && outer.right() >= inner.right()
    && outer.bottom() >= inner.bottom()
}

#[test]
fn glyph_ink_outside_the_line_box_is_repainted() {
  // Stacked accents rise above the line; a leading combining mark hangs
  // left of the text's origin.
  for (from, to) in [("\u{1ED6}", "O"), ("\u{1EB6}", "A"), ("\u{303}x", "x")] {
    for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
      let mut h = Harness::new();
      let label = h.cx.signal(String::from(from));
      let row = Row::new()
        .padding(8, 8)
        .align(Align::Center)
        .child(Text::new(move || label.get()));
      let window = h.window(h.panel().child(row), 60.0, 40.0);
      window.take_damage();
      let old = ink(&window, &mut h.text, &[0, 0], scale);
      label.set(String::from(to));
      h.frame(&window);
      let new = ink(&window, &mut h.text, &[0, 0], scale);
      let damage = covered(&window.take_damage());
      assert!(
        covers(damage, old) && covers(damage, new),
        "{from:?} to {to:?} at {scale}: ink {old:?} and {new:?}, damage {damage:?}"
      );
    }
  }
}

/// The bounds of the node at `path`: what it and its descendants painted.
fn bounds_at(window: &crate::Window, path: &[usize]) -> Rect {
  super::node_at(window.view().root(), path).bounds()
}

#[test]
fn a_spacer_that_resizes_adds_no_damage() {
  let mut h = Harness::new();
  let label = h.cx.signal(String::from("12:34"));
  let row = Row::new()
    .child(Spacer::new())
    .child(Text::new(move || label.get()));
  let window = h.window(h.panel().child(row), 300.0, 30.0);
  window.take_damage();
  let old_spacer = rect_at(&window, &[0, 0]);
  let old_label = bounds_at(&window, &[0, 1]);

  // A clock going from 12:34 to 1:05 gets narrower, and the spacer wider.
  label.set(String::from("1:05"));
  h.frame(&window);
  assert_ne!(rect_at(&window, &[0, 0]), old_spacer, "the spacer resized");
  let new_label = bounds_at(&window, &[0, 1]);
  let mut damage = window.take_damage();
  damage.sort_by(|a, b| a.x.total_cmp(&b.x));
  let mut expected = vec![old_label, new_label];
  expected.sort_by(|a, b| a.x.total_cmp(&b.x));
  // The spacer paints nothing, so only the label is repainted.
  assert_eq!(damage, expected);
}

#[test]
fn a_background_added_without_a_layout_is_repainted_when_the_widget_moves() {
  let mut h = Harness::new();
  let label = h.cx.signal(String::from("ab"));
  let shaded = h.cx.signal(false);
  let row = Row::new()
    .align(Align::Start)
    .child(Text::new(move || label.get()))
    .child(
      Row::new()
        .style(move || {
          let style = Style::new();
          if shaded.get() {
            style.background(Color::rgb(9, 9, 9))
          } else {
            style
          }
        })
        .padding(6, 6)
        .child(Icon::new("x").size(8)),
    );
  let window = h.window(h.panel().child(row), 300.0, 30.0);
  window.take_damage();

  // A paint change: the row now draws a background, with no layout.
  shaded.set(true);
  h.runtime.flush();
  assert!(!window.needs_layout());
  let old = rect_at(&window, &[0, 1]);
  assert!(window.take_damage().contains(&old));

  // Then it moves: where its background was must be repainted.
  label.set(String::from("a much longer label"));
  h.frame(&window);
  let new = rect_at(&window, &[0, 1]);
  assert!(new.x > old.x + 20.0, "{old:?} {new:?}");
  let damage = window.take_damage();
  for rect in [old, new] {
    assert!(damage.contains(&rect), "{rect:?} missing from {damage:?}");
  }
}

#[test]
fn a_widget_that_stops_drawing_is_repainted_where_it_was() {
  let mut h = Harness::new();
  let plain = h.cx.signal(false);
  let row = Row::new().align(Align::Start).child(
    Row::new()
      .style(move || {
        if plain.get() {
          // No background, and padding, so a layout follows.
          Style::new().padding(2, 2)
        } else {
          Style::new().background(Color::rgb(9, 9, 9)).padding(6, 6)
        }
      })
      .child(Icon::new("x").size(8)),
  );
  let window = h.window(h.panel().child(row), 300.0, 30.0);
  window.take_damage();
  let old = rect_at(&window, &[0, 0]);

  plain.set(true);
  h.frame(&window);
  let damage = window.take_damage();
  assert!(damage.contains(&old), "{old:?} missing from {damage:?}");
  // It draws nothing now, so its new, smaller rectangle isn't repainted;
  // the icon inside reports its own.
  assert!(!damage.contains(&rect_at(&window, &[0, 0])), "{damage:?}");
}

#[test]
fn a_widget_with_a_border_gradient_or_shadow_that_moves_is_repainted_at_both_places() {
  let black = Color::rgb(9, 9, 9);
  for style in [
    Style::new().border(1.0, black),
    Style::new().gradient(crate::Gradient::radial(black, black)),
    Style::new().shadow(crate::Shadow::new(0.0, 0.0, 0.0, black)),
  ] {
    let mut h = Harness::new();
    let label = h.cx.signal(String::from("ab"));
    let row = Row::new()
      .align(Align::Start)
      .child(Text::new(move || label.get()))
      .child(
        Row::new()
          .style(style)
          .padding(6, 6)
          .child(Icon::new("x").size(8)),
      );
    let window = h.window(h.panel().child(row), 300.0, 30.0);
    window.take_damage();
    let old = rect_at(&window, &[0, 1]);

    label.set(String::from("a much longer label"));
    h.frame(&window);
    let damage = window.take_damage();
    for rect in [old, rect_at(&window, &[0, 1])] {
      assert!(
        damage.contains(&rect),
        "{style:?}: {rect:?} missing from {damage:?}"
      );
    }
  }
}

#[test]
fn a_shadow_widens_the_painted_area_and_a_new_one_is_laid_out() {
  use crate::Shadow;

  let mut h = Harness::new();
  let lifted = h.cx.signal(false);
  let row = Row::new().padding(20, 20).align(Align::Start).child(
    Row::new()
      .style(move || {
        let style = Style::new().background(Color::rgb(9, 9, 9));
        if lifted.get() {
          style.shadow(Shadow::new(0.0, 4.0, 4.0, Color::rgb(0, 0, 0)))
        } else {
          style
        }
      })
      .padding(6, 6),
  );
  let window = h.window(h.panel().child(row), 300.0, 60.0);
  window.take_damage();
  let rect = rect_at(&window, &[0, 0]);

  lifted.set(true);
  h.runtime.flush();
  assert!(window.needs_layout(), "the painted area grows");
  h.frame(&window);
  // Down 4, and 6 past each side for the blur.
  let reach = Rect::new(
    rect.x - 6.0,
    rect.y - 2.0,
    rect.width + 12.0,
    rect.height + 12.0,
  );
  let bounds = super::node_at(window.view().root(), &[0, 0]).bounds();
  assert_eq!(bounds, rect.union(reach));
  let damage = window.take_damage();
  assert!(
    damage.iter().any(|area| area.union(reach) == *area),
    "{reach:?} missing from {damage:?}"
  );
}

#[test]
fn a_content_shadow_reaches_past_what_casts_it_and_a_child_repaints_its_shadow_too() {
  use crate::Shadow;

  let mut h = Harness::new();
  let shadow = Shadow::new(3.0, 4.0, 4.0, Color::rgb(0, 0, 0)).follow_content();
  let dark = h.cx.signal(true);
  let row = Row::new().padding(20, 20).align(Align::Start).child(
    Row::new()
      .style(Style::new().shadow(shadow))
      .padding(10, 10)
      .child(
        Row::new()
          .style(move || {
            let gray = if dark.get() { 9 } else { 200 };
            Style::new().background(Color::rgb(gray, gray, gray))
          })
          .padding(4, 4),
      ),
  );
  let window = h.window(h.panel().child(row), 300.0, 80.0);
  let parent = rect_at(&window, &[0, 0]);
  let child = rect_at(&window, &[0, 0, 0]);
  let window_view = window.view();
  let view = super::node_at(window_view.root(), &[0, 0]);
  assert_eq!(view.bounds(), parent.union(shadow.reach(parent)));
  assert_eq!(view.inner_bounds(), view.bounds());
  drop(window_view);
  window.take_damage();

  dark.set(false);
  h.runtime.flush();
  assert!(!window.needs_layout());
  let damage = window.take_damage();
  let cast = child.union(shadow.reach(child));
  assert!(
    damage.iter().any(|area| area.union(cast) == *area),
    "{cast:?} missing from {damage:?}"
  );
}

#[test]
fn a_content_shadow_turns_with_its_widget() {
  use crate::Shadow;

  let mut h = Harness::new();
  let shadow = Shadow::new(0.0, 4.0, 2.0, Color::rgb(0, 0, 0)).follow_content();
  let row = Row::new().padding(20, 20).align(Align::Start).child(
    Row::new()
      .style(
        Style::new()
          .background(Color::rgb(9, 9, 9))
          .shadow(shadow)
          .translate(10.0, 0.0),
      )
      .padding(6, 6),
  );
  let window = h.window(h.panel().child(row), 300.0, 80.0);
  let rect = rect_at(&window, &[0, 0]);
  let window_view = window.view();
  let view = super::node_at(window_view.root(), &[0, 0]);
  let inner = rect.union(shadow.reach(rect));
  assert_eq!(view.inner_bounds(), inner);
  let moved = inner.outset(1.0);
  assert_eq!(
    view.bounds(),
    Rect::new(moved.x + 10.0, moved.y, moved.width, moved.height)
  );
}

#[test]
fn a_mask_that_comes_or_goes_repaints_everything_inside_without_a_layout() {
  use crate::Mask;

  let mut h = Harness::new();
  let masked = h.cx.signal(false);
  let row = Row::new().padding(20, 20).align(Align::Start).child(
    Row::new()
      .style(move || {
        let style = Style::new().width(20.0).height(20.0);
        if masked.get() {
          style.mask(Mask::ellipse())
        } else {
          style
        }
      })
      .child(Text::new("text wider than its box")),
  );
  let window = h.window(h.panel().child(row), 300.0, 80.0);
  window.take_damage();
  let bounds = super::node_at(window.view().root(), &[0, 0]).bounds();
  assert!(bounds.width > 20.0, "the text overflows");
  for on in [true, false] {
    masked.set(on);
    h.runtime.flush();
    assert!(!window.needs_layout());
    assert_eq!(window.take_damage(), vec![bounds], "masked: {on}");
  }
}

#[test]
fn a_content_shadow_reaching_past_64_pixels_is_left_out_and_logged_once() {
  use crate::Shadow;

  let mut h = Harness::new();
  let far = h.cx.signal(true);
  let logs = super::capture_logs(|| {
    let row = Row::new().padding(20, 20).align(Align::Start).child(
      Row::new()
        .style(move || {
          // 1.5 blurs and the spread's size: 66 past, then 63.
          let blur = if far.get() { 40.0 } else { 38.0 };
          let shadow = Shadow::new(0.0, 0.0, blur, Color::rgb(0, 0, 0)).spread(-6.0);
          Style::new()
            .background(Color::rgb(9, 9, 9))
            .shadow(shadow.follow_content())
            .width(20.0 + blur)
        })
        .padding(6, 6),
    );
    let window = h.window(h.panel().child(row), 300.0, 80.0);
    let rect = rect_at(&window, &[0, 0]);
    let bounds = super::node_at(window.view().root(), &[0, 0]).bounds();
    assert_eq!(bounds, rect, "no reach past the box");
    // Laid out again, still too far: no second line.
    far.set(false);
    h.frame(&window);
    far.set(true);
    h.frame(&window);
    far.set(false);
    h.frame(&window);
    let rect = rect_at(&window, &[0, 0]);
    let bounds = super::node_at(window.view().root(), &[0, 0]).bounds();
    assert!(bounds.width > rect.width, "63 is drawn");
  });
  assert_eq!(
    logs.matches("PanelWindow[DP-1]/Row/Row").count(),
    1,
    "{logs}"
  );
}

#[test]
fn an_instant_change_of_a_content_shadow_repaints_what_every_child_casts() {
  use crate::{Point, Shadow};

  let mut h = Harness::new();
  let shadow = |color| Shadow::new(2.0, 2.0, 2.0, color).follow_content();
  // The child is drawn twice its size, past the button's box.
  let child = Row::new()
    .style(Style::new().background(Color::rgb(9, 9, 9)).scale(2.0))
    .padding(6, 6);
  let button = Button::new(child)
    .style(Style::new().shadow(shadow(Color::rgb(0, 0, 0))))
    .hover_style(Style::new().shadow(shadow(Color::rgb(255, 0, 0))));
  let row = Row::new().padding(30, 40).align(Align::Start).child(button);
  let window = h.window(h.panel().child(row), 300.0, 100.0);
  window.take_damage();
  let rect = rect_at(&window, &[0, 0]);

  window.pointer_motion(Point::new(rect.x + 2.0, rect.y + 2.0));
  h.frame(&window);
  let bounds = super::node_at(window.view().root(), &[0, 0]).bounds();
  assert!(bounds.width > rect.width + 12.0, "the child overflows");
  let damage = window.take_damage();
  assert!(
    damage.iter().any(|area| area.union(bounds) == *area),
    "{bounds:?} missing from {damage:?}"
  );
}
