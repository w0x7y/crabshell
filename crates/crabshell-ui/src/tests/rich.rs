//! Rich text: spans shaped, measured and handed to the renderer.

use super::{Harness, node_at, rect_at};
use crate::text::{Fonts, TextNode};
use crate::{Align, Color, Content, Font, Row, Span, Text, Window};

const FONT: Font = Font::new("sans-serif", 13.0);

fn rich(fonts: &mut Fonts, spans: Vec<Span>, font: Font) -> TextNode {
  let mut node = TextNode::new();
  node.set_spans(spans);
  node.prepare(font, fonts);
  node
}

fn width(node: &TextNode) -> f32 {
  node.runs().map(|run| run.line_w).fold(0.0, f32::max)
}

#[test]
fn a_bold_span_is_wider_than_the_same_text_regular() {
  let mut fonts = Fonts::new();
  let regular = rich(&mut fonts, vec![Span::new("Hello world")], FONT);
  let bold = rich(&mut fonts, vec![Span::new("Hello world").bold()], FONT);
  assert!(
    width(&bold) > width(&regular),
    "bold {} regular {}",
    width(&bold),
    width(&regular)
  );
  // The same text as plain text shapes as the regular span does.
  let mut plain = TextNode::new();
  plain.set_content(String::from("Hello world"));
  plain.prepare(FONT, &mut fonts);
  assert_eq!(width(&plain), width(&regular));
}

#[test]
fn spans_take_their_own_face_and_otherwise_the_texts() {
  let mut fonts = Fonts::new();
  let spans = vec![
    Span::new("a"),
    Span::new("b").bold(),
    Span::new("c").italic(),
  ];
  // A bold text keeps its weight in a span that only sets italic.
  let node = rich(&mut fonts, spans, FONT.weight(600));
  let db = fonts.font_system().db();
  let faces: Vec<_> = node
    .runs()
    .flat_map(|run| run.glyphs.iter())
    .map(|glyph| {
      let face = db.face(glyph.font_id).expect("a loaded face");
      (
        glyph.font_weight.0,
        face.style != cosmic_text::fontdb::Style::Normal,
      )
    })
    .collect();
  assert_eq!(faces, vec![(600, false), (700, false), (600, true)]);
}

#[test]
fn glyphs_carry_their_spans_color_and_index() {
  let mut fonts = Fonts::new();
  let red = Color::rgb(0xff, 0, 0);
  let node = rich(
    &mut fonts,
    vec![
      Span::new("ab"),
      Span::new("cd").color(red).underline(),
      Span::new("ef").strikethrough(),
    ],
    FONT,
  );
  // The renderer draws each span's lines, and cosmic-text none.
  let run = node.runs().next().expect("one line");
  let glyphs: Vec<_> = run
    .glyphs
    .iter()
    .map(|glyph| (glyph.metadata, glyph.color_opt.map(|c| c.as_rgba())))
    .collect();
  let red = Some([0xff, 0, 0, 0xff]);
  assert_eq!(
    glyphs,
    vec![
      (1, None),
      (1, None),
      (2, red),
      (2, red),
      (3, None),
      (3, None)
    ]
  );
  assert!(run.decorations.is_empty());
}

#[test]
fn a_span_character_the_font_lacks_gets_a_fallback_in_its_weight() {
  // Noto Sans has no snowman; DejaVu Sans Bold has one.
  let mut fonts = Fonts::new();
  let node = rich(
    &mut fonts,
    vec![Span::new("a"), Span::new("\u{2603}").bold()],
    FONT,
  );
  let db = fonts.font_system().db();
  let snowman = node
    .runs()
    .flat_map(|run| run.glyphs.iter())
    .nth(1)
    .expect("two glyphs");
  assert_ne!(snowman.glyph_id, 0);
  let weight = db.face(snowman.font_id).map(|face| face.weight.0);
  assert!(weight >= Some(700), "bold snowman from a {weight:?} face");
}

/// The text node at `path`'s content.
fn text_at<T>(window: &Window, path: &[usize], f: impl FnOnce(&crate::TextView<'_>) -> T) -> T {
  let view = window.view();
  let Content::Text(text) = node_at(view.root(), path).content() else {
    panic!("a text node has text content");
  };
  f(&text)
}

#[test]
fn a_rich_text_is_measured_from_the_buffer_it_draws() {
  let mut h = Harness::new();
  let spans = vec![
    Span::new("one "),
    Span::new("two ").bold(),
    Span::new("three").italic(),
  ];
  let row = Row::new()
    .align(Align::Start)
    .child(Text::rich(spans.clone()))
    .child(
      Text::rich(spans)
        .max_lines(2)
        .style(crate::Style::new().width(60.0)),
    );
  let window = h.window(h.panel().child(row), 400.0, 60.0);
  let drawn = text_at(&window, &[0, 0], |text| {
    assert_eq!(text.content(), "one two three");
    text.runs().map(|run| run.line_w).fold(0.0, f32::max)
  });
  assert_eq!(rect_at(&window, &[0, 0]).width, drawn.ceil());
  // Too narrow for one line, it wraps onto two.
  let lines = text_at(&window, &[0, 1], |text| text.runs().count());
  assert_eq!(lines, 2);
  assert_eq!(rect_at(&window, &[0, 1]).height, 32.0);
}

#[test]
fn new_spans_lay_out_and_equal_ones_do_nothing() {
  let mut h = Harness::new();
  let bold = h.cx.signal(false);
  let text = Text::rich(move || {
    let span = Span::new("label");
    vec![if bold.get() { span.bold() } else { span }]
  });
  let window = h.window(
    h.panel().child(Row::new().align(Align::Start).child(text)),
    200.0,
    30.0,
  );
  let before = rect_at(&window, &[0, 0]).width;

  bold.set(false);
  h.runtime.flush();
  assert!(!window.needs_layout());

  bold.set(true);
  h.frame(&window);
  assert!(rect_at(&window, &[0, 0]).width > before);
}

#[test]
fn many_spans_needing_fallbacks_shape_quickly() {
  let time = |body: &str| {
    let spans = crate::parse_markup(&body.repeat(5000));
    assert_eq!(spans.len(), 10000);
    let mut fonts = Fonts::new();
    // Warm: the faces and fallbacks load once.
    rich(&mut fonts, spans[..2].to_vec(), FONT);
    let start = std::time::Instant::now();
    let node = rich(&mut fonts, spans, FONT);
    let elapsed = start.elapsed();
    let glyphs: Vec<_> = node.runs().flat_map(|run| run.glyphs.iter()).collect();
    assert!(
      glyphs.iter().all(|glyph| glyph.glyph_id != 0),
      "a glyph is missing"
    );
    elapsed
  };
  // Noto Sans has no snowman, so every span asks DejaVu Sans, unlike "a".
  let plain = time("<b>a</b>a");
  let fallback = time("<b>☃</b>☃");
  // A fallback costs some shaping, about three times the plain body here,
  // but matching each missing character against every span cost seven.
  assert!(fallback < plain * 5, "{fallback:?} against {plain:?}");
}
