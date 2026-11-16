//! `cx.measure_text` and fonts bundled with `cx.add_font`.

use std::cell::Cell;
use std::rc::Rc;

use cosmic_text::fontdb::Source;

use super::{Harness, capture_logs, node_at, rect_at};
use crate::text::{Fonts, TextNode};
use crate::{Align, Content, Font, Rect, Row, Style, Text, TextSystem, Window};

/// DejaVu Sans's bytes, leaked as `include_bytes!` would keep them, or
/// `None` with a message on a machine that keeps the file elsewhere.
fn dejavu() -> Option<&'static [u8]> {
  font_file("DejaVuSans.ttf")
}

/// The bytes of the font file `name` from a usual directory, leaked, or
/// `None` with a message.
fn font_file(name: &str) -> Option<&'static [u8]> {
  for dir in [
    "/usr/share/fonts/dejavu",
    "/usr/share/fonts/truetype/dejavu",
    "/usr/share/fonts/TTF",
    "/usr/share/fonts/noto-emoji",
  ] {
    if let Ok(bytes) = std::fs::read(format!("{dir}/{name}")) {
      return Some(Box::leak(bytes.into_boxed_slice()));
    }
  }
  eprintln!("skipped: {name} isn't in any of the usual places");
  None
}

/// A window with one `Text` showing `content` in `font`.
fn text_window(h: &mut Harness, content: &str, font: Font) -> Window {
  let row = Row::new()
    .align(Align::Start)
    .child(Text::new(content.to_owned()).style(Style::new().font(font)));
  h.window(h.panel().child(row), 600.0, 60.0)
}

/// Whether every glyph of the text at `path` is drawn from a bundled font.
fn drawn_from_bundled(window: &Window, text: &TextSystem, path: &[usize]) -> bool {
  let view = window.view();
  let Content::Text(shown) = node_at(view.root(), path).content() else {
    panic!("a text node has text content");
  };
  let fonts = text.font_system();
  let mut glyphs = shown.runs().flat_map(|run| run.glyphs.iter()).peekable();
  glyphs.peek().is_some()
    && glyphs.all(|glyph| {
      fonts
        .db()
        .face(glyph.font_id)
        .is_some_and(|face| matches!(face.source, Source::Binary(_)))
    })
}

#[test]
fn measuring_agrees_with_a_text() {
  for font in [
    Font::new("sans-serif", 15.0),
    Font::new("sans-serif", 13.0).weight(700),
    // Not installed: fontconfig's best match, as for a `Text`.
    Font::new("No Such Family 1234", 13.0),
  ] {
    let mut h = Harness::new();
    let window = text_window(&mut h, "clicked 12 times", font);
    let laid_out = rect_at(&window, &[0, 0]);
    let measured = h.cx.measure_text("clicked 12 times", font);
    assert!(measured.width > 20.0, "{measured:?}");
    assert_eq!(measured.width.ceil(), laid_out.width, "{font:?}");
    assert_eq!(measured.height, laid_out.height, "{font:?}");
    assert_eq!(measured.line_height, measured.height);
    // Noto Sans reaches past its line box: these are the font's own.
    assert!(
      measured.ascent > 0.0 && measured.descent > 0.0,
      "{measured:?}"
    );
  }
}

#[test]
fn an_empty_string_is_one_empty_line() {
  let h = Harness::new();
  let measured = h.cx.measure_text("", Font::DEFAULT);
  assert_eq!(measured.width, 0.0);
  // The default font is 13 pixels with a line height of round(13 * 1.2).
  assert_eq!(measured.line_height, 16.0);
  assert_eq!(measured.height, 16.0);
  assert!(measured.ascent > 0.0, "{measured:?}");
}

#[test]
fn each_line_break_adds_a_line() {
  let h = Harness::new();
  let font = Font::DEFAULT;
  let long = h.cx.measure_text("a longer second line", font);
  let both = h.cx.measure_text("short\na longer second line", font);
  assert_eq!(both.width, long.width);
  assert_eq!(both.height, 2.0 * both.line_height);
  let three = h.cx.measure_text("one\ntwo\n", font);
  assert_eq!(three.height, 3.0 * three.line_height);
}

#[test]
fn a_binding_measures_while_its_window_lays_out() {
  // The binding reruns when layout writes `geometry`, from the flush
  // inside `Window::layout`: the fonts must not be borrowed then.
  let mut h = Harness::new();
  let geometry = h.cx.signal(Rect::default());
  let seen = Rc::new(Cell::new((0.0, 0.0)));
  let cx = h.cx.clone();
  let record = Rc::clone(&seen);
  let row = Row::new()
    .align(Align::Start)
    .geometry(geometry)
    .child(Text::new(move || {
      let width = cx.measure_text("abc", Font::DEFAULT).width;
      record.set((geometry.get().width, width));
      format!("{width}")
    }));
  h.window(h.panel().child(row), 400.0, 30.0);
  let (row_width, measured) = seen.get();
  assert_eq!(row_width, 400.0, "the binding didn't run after layout");
  assert!(measured > 10.0, "measured {measured}");
}

#[test]
fn a_bundled_family_needs_no_fontconfig() {
  let Some(bytes) = dejavu() else { return };
  let mut fonts = Fonts::new();
  fonts.add(bytes);
  let mut node = TextNode::new();
  node.set_content(String::from("clicked 12 times"));
  node.prepare(Font::new("dejavu sans", 13.0), &mut fonts);
  let measured = fonts.measure("clicked 12 times", Font::new("DejaVu Sans", 13.0));
  assert!(measured.width > 20.0, "{measured:?}");
  assert!(!fonts.fontconfig_started());
  let glyphs: Vec<u16> = node
    .runs()
    .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.glyph_id))
    .collect();
  assert!(glyphs.iter().all(|&id| id != 0), "{glyphs:?}");
  // A character DejaVu Sans lacks still falls back through fontconfig.
  node.set_content(String::from("a\u{4E00}"));
  node.prepare(Font::new("DejaVu Sans", 13.0), &mut fonts);
  assert!(fonts.fontconfig_started());
}

#[test]
fn bytes_that_hold_no_font_are_logged_and_ignored() {
  let h = Harness::new();
  let logs = capture_logs(|| h.cx.add_font(b"not a font"));
  assert!(logs.contains("bundled font"), "{logs}");
  let faces = h.text.font_system().db().len();
  assert_eq!(faces, 0);
}

#[test]
fn a_font_added_later_shapes_laid_out_text_again() {
  let Some(bytes) = dejavu() else { return };
  let mut h = Harness::new();
  let window = text_window(&mut h, "hello", Font::new("DejaVu Sans", 13.0));
  // fontconfig's file first.
  assert!(!drawn_from_bundled(&window, &h.text, &[0, 0]));
  assert!(!window.needs_layout());
  h.cx.add_font(bytes);
  assert!(window.needs_layout());
  h.frame(&window);
  assert!(drawn_from_bundled(&window, &h.text, &[0, 0]));
  assert!(!window.needs_layout());
}

#[test]
fn a_font_added_later_repaints_text_that_kept_its_size() {
  let Some(bold) = font_file("DejaVuSans-Bold.ttf") else {
    return;
  };
  let mut h = Harness::new();
  let style = Style::new()
    .font(Font::new("DejaVu Sans", 13.0))
    .width(200.0);
  let row = Row::new()
    .align(Align::Start)
    .child(Text::new("hello").style(style));
  let window = h.window(h.panel().child(row), 600.0, 60.0);
  window.take_damage();
  let text = rect_at(&window, &[0, 0]);
  h.cx.add_font(bold);
  h.frame(&window);
  // Its box didn't move, but its glyphs may have changed.
  let damage = window.take_damage();
  assert!(
    damage.iter().any(|area| area.union(text) == *area),
    "{damage:?} misses {text:?}"
  );
}

/// Each glyph's face in `node`: whether it is bundled, and its weight.
fn glyph_faces(node: &TextNode, fonts: &mut Fonts) -> Vec<(bool, u16)> {
  let db = fonts.font_system().db();
  node
    .runs()
    .flat_map(|run| run.glyphs.iter())
    .filter_map(|glyph| db.face(glyph.font_id))
    .map(|face| (matches!(face.source, Source::Binary(_)), face.weight.0))
    .collect()
}

/// `content` shaped in DejaVu Sans at `weight`.
fn dejavu_text(fonts: &mut Fonts, weight: u16) -> TextNode {
  let mut node = TextNode::new();
  node.set_content(String::from("hello"));
  node.prepare(Font::new("DejaVu Sans", 13.0).weight(weight), fonts);
  node
}

#[test]
fn a_bundled_bold_leaves_regular_text_to_fontconfig() {
  let Some(bold) = font_file("DejaVuSans-Bold.ttf") else {
    return;
  };
  let mut fonts = Fonts::new();
  fonts.add(bold);
  let node = dejavu_text(&mut fonts, 700);
  assert!(!fonts.fontconfig_started());
  assert!(
    glyph_faces(&node, &mut fonts)
      .iter()
      .all(|&face| face == (true, 700))
  );
  // No bundled face has weight 400, so fontconfig's file is used.
  let node = dejavu_text(&mut fonts, 400);
  assert!(fonts.fontconfig_started());
  let faces = glyph_faces(&node, &mut fonts);
  assert!(faces.iter().all(|&face| face == (false, 400)), "{faces:?}");
}

#[test]
fn a_bundled_regular_keeps_the_installed_bold() {
  let Some(regular) = dejavu() else { return };
  let mut fonts = Fonts::new();
  dejavu_text(&mut fonts, 400);
  dejavu_text(&mut fonts, 700);
  fonts.add(regular);
  let faces = glyph_faces(&dejavu_text(&mut fonts, 700), &mut fonts);
  assert!(faces.iter().all(|&face| face == (false, 700)), "{faces:?}");
  let faces = glyph_faces(&dejavu_text(&mut fonts, 400), &mut fonts);
  assert!(faces.iter().all(|&face| face == (true, 400)), "{faces:?}");
}

#[test]
fn a_bundled_variable_font_serves_the_weights_it_spans() {
  let Some(emoji) = font_file("NotoEmoji-VariableFont_wght.ttf") else {
    return;
  };
  let mut fonts = Fonts::new();
  fonts.add(emoji);
  // Its weight axis runs from 300 to 700.
  for weight in [300, 600, 700] {
    let font = Font::new("Noto Emoji", 13.0).weight(weight);
    assert_eq!(fonts.family(&font).as_deref(), Some("Noto Emoji"));
  }
  assert!(!fonts.fontconfig_started());
  fonts.family(&Font::new("Noto Emoji", 13.0).weight(900));
  assert!(fonts.fontconfig_started());
}

#[test]
fn a_replaced_face_is_never_read_back() {
  let Some(regular) = dejavu() else { return };
  let mut fonts = Fonts::new();
  dejavu_text(&mut fonts, 400);
  let path = fonts
    .font_system()
    .db()
    .faces()
    .find_map(|face| match &face.source {
      Source::File(path) | Source::SharedFile(path, _) => Some(path.clone()),
      Source::Binary(_) => None,
    })
    .expect("fontconfig loaded a file");
  // As for a collection's face that was read with another one and never
  // asked for by its own number.
  fonts.loaded.clear();
  fonts.add(regular);
  let name = fonts.load((path, 0));
  assert_eq!(name.as_deref(), Some("DejaVu Sans"));
  let files = fonts
    .font_system()
    .db()
    .faces()
    .filter(|face| !matches!(face.source, Source::Binary(_)))
    .count();
  assert_eq!(files, 0, "the replaced face was read again");
}
