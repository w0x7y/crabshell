//! The elided last line of wrapped text, which cosmic-text 0.19 starts
//! with the space it broke at: crabshell drops it everywhere it reads
//! the lines.

use crate::select::text_hit;
use crate::style::{Font, TextAlign};
use crate::text::{Fonts, TextNode, TextStyle};
use crate::{Elide, Point, Rect};

const WORDS: &str = "one two three four five six seven eight nine ten";

/// `content` over at most two lines in a box 80 wide at the window's
/// corner, ending in "…" when it doesn't fit, aligned as `align` says.
fn elided(text: &mut Fonts, content: &str, align: Option<TextAlign>) -> TextNode {
  let mut node = TextNode::new();
  node.set_content(String::from(content));
  node.set_max_lines(2);
  node.set_elide(Elide::End);
  let mut style = TextStyle::from(Font::new("sans-serif", 13.0));
  style.align = align;
  node.prepare(style, text);
  node.finish_at(
    Rect::new(0.0, 0.0, 80.0, 40.0),
    Some(80.0),
    text.font_system(),
  );
  node
}

/// The text of each glyph on line `line`, the ellipsis as "".
fn glyph_texts(node: &TextNode, line: usize) -> Vec<String> {
  let run = node.runs().nth(line).expect("that line");
  run
    .glyphs
    .iter()
    .map(|glyph| run.text[glyph.start..glyph.end].to_owned())
    .collect()
}

#[test]
fn an_elided_wrapped_line_starts_at_its_first_word() {
  let mut text = Fonts::new();
  let node = elided(&mut text, WORDS, None);
  // cosmic-text's own layout starts the line with the space.
  let raw = node.buffer.layout_runs().nth(1).expect("a second line");
  assert_eq!(&raw.text[raw.glyphs[0].start..raw.glyphs[0].end], " ");

  let glyphs = glyph_texts(&node, 1);
  assert_eq!(glyphs[0], "t", "{glyphs:?}");
  assert_eq!(glyphs.last().map(String::as_str), Some(""), "ends in …");
  let run = node.runs().nth(1).expect("a second line");
  assert_eq!(run.glyphs[0].x, 0.0);
  let last = run.glyphs.last().expect("glyphs");
  assert!(
    (run.line_w - (last.x + last.w)).abs() < 0.01,
    "{}",
    run.line_w
  );
  // The first line was never wrong.
  assert_eq!(glyph_texts(&node, 0)[0], "o");
}

#[test]
fn a_leading_space_of_the_text_itself_stays() {
  let mut text = Fonts::new();
  let mut node = TextNode::new();
  node.set_content(format!(" {WORDS}"));
  node.set_elide(Elide::End);
  node.prepare(Font::new("sans-serif", 13.0), &mut text);
  node.finish_at(
    Rect::new(0.0, 0.0, 80.0, 20.0),
    Some(80.0),
    text.font_system(),
  );
  assert_eq!(glyph_texts(&node, 0)[0], " ");
}

#[test]
fn a_centered_elided_line_centers_what_is_left() {
  let mut text = Fonts::new();
  let node = elided(&mut text, WORDS, Some(TextAlign::Center));
  let run = node.runs().nth(1).expect("a second line");
  assert_ne!(glyph_texts(&node, 1)[0], " ");
  let first = run.glyphs.first().expect("glyphs");
  let last = run.glyphs.last().expect("glyphs");
  let (left, right) = (first.x, 80.0 - (last.x + last.w));
  assert!((left - right).abs() < 0.5, "{left} and {right}");
}

#[test]
fn an_end_aligned_elided_line_keeps_its_right_edge() {
  let mut text = Fonts::new();
  let node = elided(&mut text, WORDS, Some(TextAlign::End));
  let raw = node.buffer.layout_runs().nth(1).expect("a second line");
  let run = node.runs().nth(1).expect("a second line");
  assert_ne!(glyph_texts(&node, 1)[0], " ");
  let end = |glyphs: &[cosmic_text::LayoutGlyph]| glyphs.last().map(|g| g.x + g.w);
  assert_eq!(end(&run.glyphs), end(raw.glyphs));
}

#[test]
fn right_to_left_text_is_left_as_cosmic_text_lays_it_out() {
  let mut text = Fonts::new();
  let node = elided(&mut text, "שלום עולם זה טקסט ארוך מאוד שלא נכנס", None);
  for (run, raw) in node.runs().zip(node.buffer.layout_runs()) {
    assert!(run.rtl);
    let xs = |glyphs: &[cosmic_text::LayoutGlyph]| -> Vec<(usize, f32)> {
      glyphs.iter().map(|g| (g.start, g.x)).collect()
    };
    assert_eq!(xs(&run.glyphs), xs(raw.glyphs));
    assert_eq!(run.line_w, raw.line_w);
  }
}

#[test]
fn a_click_on_an_elided_line_hits_the_glyph_drawn_there() {
  let mut text = Fonts::new();
  let node = elided(&mut text, WORDS, None);
  let run = node.runs().nth(1).expect("a second line");
  let (t, h) = (&run.glyphs[0], &run.glyphs[1]);
  assert_eq!(&WORDS[t.start..t.end], "t");
  let y = run.line_top + run.line_height / 2.0;
  // The left half of the "h" puts the caret before it.
  let at = Point::new(h.x + h.w / 4.0, y);
  assert_eq!(text_hit(&node, at), h.start);
  // Left of the line, the caret goes before its first glyph shown.
  assert_eq!(text_hit(&node, Point::new(-2.0, y)), t.start);
}
