//! Text styles: italic, letter spacing, features, alignment, line height,
//! decorations and eliding, and how each is inherited and invalidated.

use super::{Harness, node_at, rect_at};
use crate::text::TextNode;
use crate::{
  Align, Column, Content, Elide, Font, FontFeature, Row, Style, Text, TextAlign, TextSystem, Window,
};

const LONG: &str = "a sentence much too long for the narrow window it sits in";

/// The text node at `path`'s content.
fn with_text<T>(window: &Window, path: &[usize], f: impl FnOnce(&crate::TextView<'_>) -> T) -> T {
  let view = window.view();
  let Content::Text(text) = node_at(view.root(), path).content() else {
    panic!("a text node has text content");
  };
  f(&text)
}

/// `text` shaped alone in `font`.
fn shaped(text: &mut TextSystem, content: &str, font: Font) -> TextNode {
  let mut node = TextNode::new();
  node.set_content(String::from(content));
  node.prepare(font, text);
  node
}

fn width(node: &TextNode) -> f32 {
  node.runs().map(|run| run.line_w).fold(0.0, f32::max)
}

#[test]
fn the_new_fields_merge_and_build_in_consts() {
  const FONT: Font = Font::new("sans-serif", 13.0)
    .italic()
    .letter_spacing(1.5)
    .features(&[FontFeature::TABULAR_NUMBERS, FontFeature::NO_LIGATURES]);
  const BASE: Style = Style::new()
    .font(FONT)
    .text_align(TextAlign::Center)
    .line_height(1.5)
    .underline(true);
  const OVER: Style = Style::new().strikethrough(true).underline(false);
  let merged = BASE.merge(OVER);
  assert_eq!(merged.font, Some(FONT));
  assert_eq!(merged.text_align, Some(TextAlign::Center));
  assert_eq!(merged.line_height, Some(1.5));
  assert_eq!(merged.underline, Some(false));
  assert_eq!(merged.strikethrough, Some(true));
  const { assert!(FONT.italic) };
  assert_eq!(FONT.features[0], FontFeature::new(*b"tnum", 1));
  assert_ne!(FONT, FONT.features(&[]));
}

#[test]
fn alignment_and_line_height_lay_out_but_decorations_only_paint() {
  let plain = Style::new();
  assert!(!plain.same_layout(&plain.text_align(TextAlign::End)));
  assert!(!plain.same_layout(&plain.line_height(2.0)));
  assert!(!plain.same_layout(&plain.font(Font::DEFAULT.italic())));
  assert!(plain.same_layout(&plain.underline(true)));
  assert!(plain.same_layout(&plain.strikethrough(true)));
}

#[test]
fn a_fade_takes_the_end_styles_text_fields_at_once() {
  let from = Style::new().line_height(1.0).underline(false);
  let to = Style::new()
    .line_height(3.0)
    .underline(true)
    .text_align(TextAlign::Justify);
  let halfway = from.lerp(&to, 0.5);
  assert_eq!(halfway.line_height, Some(3.0));
  assert_eq!(halfway.underline, Some(true));
  assert_eq!(halfway.text_align, Some(TextAlign::Justify));
}

#[test]
fn text_inherits_each_field_from_the_nearest_widget_that_sets_it() {
  let mut h = Harness::new();
  let panel = h
    .panel()
    .style(Style::new().line_height(2.0).underline(true))
    .child(
      Column::new()
        .align(Align::Start)
        .style(Style::new().strikethrough(true))
        .child(Text::new("inherits"))
        .child(Text::new("own").style(Style::new().line_height(1.0).underline(false))),
    );
  let window = h.window(panel, 200.0, 100.0);
  // The default 13 px font: round(13 × 2) and round(13 × 1).
  assert_eq!(rect_at(&window, &[0, 0]).height, 26.0);
  assert_eq!(rect_at(&window, &[0, 1]).height, 13.0);
  let decorations = |path| with_text(&window, path, |t| (t.underline(), t.strikethrough()));
  assert_eq!(decorations(&[0, 0]), (true, true));
  assert_eq!(decorations(&[0, 1]), (false, true));
}

#[test]
fn an_unusable_line_height_counts_as_unset() {
  let mut h = Harness::new();
  let column = Column::new()
    .align(Align::Start)
    .child(Text::new("a").style(Style::new().line_height(0.0)))
    .child(Text::new("b").style(Style::new().line_height(-1.0)))
    .child(Text::new("c").style(Style::new().line_height(f32::NAN)))
    .child(Text::new("d").style(Style::new().line_height(0.01)));
  let window = h.window(h.panel().child(column), 200.0, 100.0);
  for index in 0..3 {
    assert_eq!(rect_at(&window, &[0, index]).height, 16.0, "child {index}");
  }
  // A tiny one still gives each line a pixel.
  assert_eq!(rect_at(&window, &[0, 3]).height, 1.0);
}

#[test]
fn a_decoration_change_repaints_and_a_line_height_change_lays_out() {
  let mut h = Harness::new();
  let underline = h.cx.signal(false);
  let tall = h.cx.signal(false);
  let row = Row::new()
    .align(Align::Start)
    .style(move || Style::new().underline(underline.get()))
    .child(
      Text::new("x").style(move || Style::new().line_height(if tall.get() { 2.0 } else { 1.2 })),
    );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.take_damage();

  underline.set(true);
  h.runtime.flush();
  assert!(!window.needs_layout());
  assert!(window.has_damage());
  assert!(with_text(&window, &[0, 0], |t| t.underline()));
  window.take_damage();

  tall.set(true);
  h.runtime.flush();
  assert!(window.needs_layout());
  h.frame(&window);
  assert_eq!(rect_at(&window, &[0, 0]).height, 26.0);
}

#[test]
fn letter_spacing_adds_its_pixels_after_each_glyph() {
  let mut text = TextSystem::new();
  let plain = width(&shaped(&mut text, "abcd", Font::DEFAULT));
  let spaced = width(&shaped(
    &mut text,
    "abcd",
    Font::DEFAULT.letter_spacing(2.5),
  ));
  assert!((spaced - plain - 10.0).abs() < 0.01, "{plain} {spaced}");
  for odd in [f32::NAN, f32::INFINITY] {
    let same = width(&shaped(
      &mut text,
      "abcd",
      Font::DEFAULT.letter_spacing(odd),
    ));
    assert_eq!(same, plain, "{odd}");
  }
}

#[test]
fn italic_text_is_shaped_from_an_italic_face() {
  // This machine's sans-serif, Noto Sans, has an italic face. Upright
  // text goes first, so both faces are loaded when italic is asked for.
  let mut text = TextSystem::new();
  let face_style = |text: &mut TextSystem, font| {
    let node = shaped(text, "x", font);
    let id = node.runs().next().expect("a line").glyphs[0].font_id;
    text.font_system().db().face(id).map(|face| face.style)
  };
  let upright = face_style(&mut text, Font::DEFAULT);
  assert_eq!(upright, Some(cosmic_text::fontdb::Style::Normal));
  let italic = face_style(&mut text, Font::DEFAULT.italic());
  assert_eq!(italic, Some(cosmic_text::fontdb::Style::Italic));
}

#[test]
fn tabular_digits_hold_a_clocks_width_still() {
  // Noto Sans, this machine's sans-serif, has tabular digits by default,
  // and no installed font has proportional ones by default and `tnum`.
  // So `pnum` turns proportional ones on first, and `tnum` after it must
  // undo that.
  const PROPORTIONAL: [FontFeature; 1] = [FontFeature::new(*b"pnum", 1)];
  const TABULAR: [FontFeature; 2] = [PROPORTIONAL[0], FontFeature::TABULAR_NUMBERS];
  let mut text = TextSystem::new();
  let clock = |text: &mut TextSystem, features| {
    let font = Font::DEFAULT.features(features);
    let ones = width(&shaped(text, "11:11", font));
    let zeros = width(&shaped(text, "00:00", font));
    (ones, zeros)
  };
  let (ones, zeros) = clock(&mut text, &PROPORTIONAL);
  assert!(ones + 1.0 < zeros, "{ones} {zeros}");
  let (ones, zeros) = clock(&mut text, &TABULAR);
  assert_eq!(ones, zeros);
}

#[test]
fn aligned_text_sits_in_the_width_it_is_given() {
  let mut h = Harness::new();
  let sized = |align| Text::new("hi").style(Style::new().width(100.0).text_align(align));
  let column = Column::new()
    .align(Align::Start)
    .child(Text::new("hi").style(Style::new().width(100.0)))
    .child(sized(TextAlign::Start))
    .child(sized(TextAlign::Center))
    .child(sized(TextAlign::End));
  let window = h.window(h.panel().child(column), 200.0, 100.0);
  let first_x = |index| {
    with_text(&window, &[0, index], |t| {
      t.runs().next().expect("a line").glyphs[0].x
    })
  };
  let line = with_text(&window, &[0, 0], |t| {
    t.runs().next().expect("a line").line_w
  });
  assert_eq!(first_x(0), 0.0);
  assert_eq!(first_x(1), 0.0);
  assert!((first_x(2) - (100.0 - line) / 2.0).abs() < 0.01);
  assert!((first_x(3) - (100.0 - line)).abs() < 0.01);
  // Its painted area covers the line where it sits.
  let view = window.view();
  let end = node_at(view.root(), &[0, 3]);
  assert!(end.bounds().right() >= end.rect().right());
}

#[test]
fn justified_lines_reach_both_edges_but_the_last() {
  let mut h = Harness::new();
  let text = Text::new(LONG)
    .max_lines(4)
    .style(Style::new().text_align(TextAlign::Justify));
  let window = h.window(h.panel().child(Column::new().child(text)), 120.0, 200.0);
  let widths: Vec<f32> = with_text(&window, &[0, 0], |t| {
    t.runs().map(|run| run.line_w).collect()
  });
  assert!(widths.len() > 2, "{widths:?}");
  for width in &widths[..widths.len() - 1] {
    assert!((width - 120.0).abs() < 0.5, "{widths:?}");
  }
  assert!(widths[widths.len() - 1] < 110.0, "{widths:?}");
}

/// Where line `line` of the text at `[0, index]` has the font's "…", and
/// how many glyphs it has.
fn ellipsis_at(
  h: &mut Harness,
  window: &Window,
  index: usize,
  line: usize,
) -> Option<(usize, usize)> {
  let dots = shaped(&mut h.text, "\u{2026}", Font::DEFAULT);
  let dots = dots.runs().next().expect("a line").glyphs[0].glyph_id;
  with_text(window, &[0, index], |t| {
    let glyphs: Vec<u16> = t
      .runs()
      .nth(line)
      .expect("a line")
      .glyphs
      .iter()
      .map(|glyph| glyph.glyph_id)
      .collect();
    glyphs
      .iter()
      .position(|&id| id == dots)
      .map(|at| (at, glyphs.len()))
  })
}

#[test]
fn each_elide_mode_cuts_its_own_part() {
  let mut h = Harness::new();
  let column = Column::new()
    .child(Text::new(LONG).elide(Elide::Start))
    .child(Text::new(LONG).elide(Elide::Middle))
    .child(Text::new(LONG).elide(Elide::End))
    .child(Text::new(LONG).elide(Elide::None));
  let window = h.window(h.panel().child(column.align(Align::Start)), 120.0, 100.0);
  let mut at = |index| ellipsis_at(&mut h, &window, index, 0);
  assert_eq!(at(0).map(|(at, _)| at), Some(0));
  let (middle, count) = at(1).expect("a middle ellipsis");
  assert!(middle > 2 && middle < count - 3, "{middle} of {count}");
  assert_eq!(at(2).map(|(at, count)| count - at), Some(1));
  assert_eq!(at(3), None);
  assert!(rect_at(&window, &[0, 3]).width > 120.0);
}

#[test]
fn whichever_of_ellipsis_and_elide_is_called_last_wins() {
  let mut h = Harness::new();
  let column = Column::new()
    .child(Text::new(LONG).ellipsis(true).elide(Elide::None))
    .child(Text::new(LONG).elide(Elide::Start).ellipsis(true))
    .child(Text::new(LONG).elide(Elide::Start).ellipsis(false));
  let window = h.window(h.panel().child(column.align(Align::Start)), 120.0, 100.0);
  assert!(rect_at(&window, &[0, 0]).width > 120.0);
  assert!(rect_at(&window, &[0, 1]).width <= 120.0);
  assert!(rect_at(&window, &[0, 2]).width > 120.0);
  // The ellipsis is at the end.
  let (at, count) = ellipsis_at(&mut h, &window, 1, 0).expect("an ellipsis");
  assert_eq!(at, count - 1);
}

#[test]
fn start_eliding_with_more_lines_cuts_the_last_line() {
  let mut h = Harness::new();
  let text = Text::new(LONG).max_lines(2).elide(Elide::Start);
  let window = h.window(h.panel().child(Column::new().child(text)), 120.0, 100.0);
  assert_eq!(with_text(&window, &[0, 0], |t| t.runs().count()), 2);
  assert_eq!(ellipsis_at(&mut h, &window, 0, 0), None);
  assert_eq!(
    ellipsis_at(&mut h, &window, 0, 1).map(|(at, _)| at),
    Some(0)
  );
}
