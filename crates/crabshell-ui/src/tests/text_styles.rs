//! Text styles: italic, letter spacing, features and eliding.

use super::{Harness, node_at, rect_at};
use crate::text::TextNode;
use crate::{Align, Column, Content, Elide, Font, FontFeature, Text, TextSystem, Window};

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
