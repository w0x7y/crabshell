//! Font loading through fontconfig, and fallback fonts.

use crate::text::{TextNode, face_number, fc_weight};
use crate::{Font, TextSystem};

#[test]
fn css_weights_map_onto_fontconfigs_scale() {
  assert_eq!(fc_weight(100), 0);
  assert_eq!(fc_weight(400), 80);
  assert_eq!(fc_weight(700), 200);
  assert_eq!(fc_weight(1000), 215);
  // Between two points, interpolated; outside the range, clamped.
  assert_eq!(fc_weight(450), 90);
  assert_eq!(fc_weight(0), 0);
  assert_eq!(fc_weight(u16::MAX), 215);
}

#[test]
fn a_generic_family_resolves_to_one_loaded_font() {
  let mut text = TextSystem::new();
  let name = text.family(&Font::new("sans-serif", 13.0));
  assert!(name.is_some_and(|name| !name.is_empty()));
  // Only the matched file is in the database, not every installed font.
  let files = text.font_system().db().faces().count();
  assert!((1..10).contains(&files), "{files} faces loaded");
  // A second request is answered from the cache.
  text.family(&Font::new("sans-serif", 20.0));
  assert_eq!(text.font_system().db().faces().count(), files);
}

#[test]
fn a_character_the_font_lacks_loads_a_fallback() {
  // Noto Sans, this machine's sans-serif, has no snowman; fontconfig names
  // DejaVu Sans for it. On a machine whose sans-serif has one, the second
  // face check below would fail and the test needs another character.
  let mut text = TextSystem::new();
  let mut node = TextNode::new();
  node.set_content(String::from("a\u{2603}b"));
  assert!(node.prepare(Font::new("sans-serif", 13.0), &mut text));
  let glyphs: Vec<_> = node
    .runs()
    .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.glyph_id))
    .collect();
  assert_eq!(glyphs.len(), 3);
  assert!(
    glyphs.iter().all(|&id| id != 0),
    "a glyph is missing: {glyphs:?}"
  );
  let faces = text.font_system().db().faces().count();
  assert!(faces >= 2, "no fallback font was loaded: {faces} faces");

  // Shaping again with nothing changed does nothing.
  assert!(!node.prepare(Font::new("sans-serif", 13.0), &mut text));
}

#[test]
fn bold_text_gets_a_bold_fallback_face() {
  // DejaVu Sans Bold has the snowman too, and regular text asks first.
  let mut text = TextSystem::new();
  let mut regular = TextNode::new();
  regular.set_content(String::from("\u{2603}"));
  regular.prepare(Font::new("sans-serif", 13.0), &mut text);
  let mut bold = TextNode::new();
  bold.set_content(String::from("\u{2603}"));
  bold.prepare(Font::new("sans-serif", 13.0).weight(700), &mut text);
  let font_id = bold
    .runs()
    .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.font_id))
    .next()
    .expect("one glyph");
  let weight = text
    .font_system()
    .db()
    .face(font_id)
    .map(|face| face.weight.0);
  assert!(
    weight >= Some(700),
    "bold snowman drawn from a {weight:?} face"
  );
}

#[test]
fn an_unusable_font_size_falls_back_to_the_default() {
  let mut text = TextSystem::new();
  let mut node = TextNode::new();
  node.set_content(String::from("x"));
  for size in [0.0, -3.0, f32::NAN, f32::INFINITY] {
    node.prepare(Font::new("sans-serif", size), &mut text);
    let height: f32 = node.runs().map(|run| run.line_height).sum();
    assert_eq!(height, 16.0, "size {size}");
  }
}

#[test]
fn an_unknown_family_still_gets_fontconfigs_best_match() {
  let mut text = TextSystem::new();
  let name = text.family(&Font::new("No Such Family 1234", 13.0));
  assert!(name.is_some());
}

#[test]
fn a_fontconfig_face_index_drops_the_named_instance() {
  // fontconfig keeps a variable font's named instance in the upper 16
  // bits; fontdb numbers only the faces in the file.
  assert_eq!(face_number(0), 0);
  assert_eq!(face_number(2), 2);
  assert_eq!(face_number(0x0003_0000), 0);
  assert_eq!(face_number(0x0003_0002), 2);
}

#[test]
fn loading_another_face_of_a_loaded_file_adds_no_faces() {
  let mut text = TextSystem::new();
  text.family(&Font::new("sans-serif", 13.0));
  let faces = text.font_system().db().faces().count();
  let path = text
    .font_system()
    .db()
    .faces()
    .find_map(|face| match &face.source {
      cosmic_text::fontdb::Source::File(path)
      | cosmic_text::fontdb::Source::SharedFile(path, _) => Some(path.clone()),
      cosmic_text::fontdb::Source::Binary(_) => None,
    })
    .expect("the font came from a file");
  // A face the file may not have: either way the file is not read again.
  text.load((path, 1));
  assert_eq!(text.font_system().db().faces().count(), faces);
}

// The next three need a fontconfig setup whose only fonts are
// NotoSans-Regular.ttf and NotoEmoji-VariableFont_wght.ttf, a variable font
// with no static faces. Put both in a directory and run:
//
//   printf '<fontconfig><dir>DIR</dir><cachedir>DIR/cache</cachedir></fontconfig>' > DIR/fonts.conf
//   FONTCONFIG_FILE=DIR/fonts.conf cargo test -p crabshell-ui --lib -- --ignored variable

#[test]
#[ignore = "needs FONTCONFIG_FILE with only a variable font; see the comment above"]
fn a_variable_font_named_instance_loads() {
  let mut text = TextSystem::new();
  let name = text.family(&Font::new("Noto Emoji", 13.0).weight(700));
  assert_eq!(name.as_deref(), Some("Noto Emoji"));
}

#[test]
#[ignore = "needs FONTCONFIG_FILE with only a variable font; see the comment above"]
fn a_variable_font_fallback_is_shaped_again() {
  let mut text = TextSystem::new();
  let mut node = TextNode::new();
  node.set_content(String::from("a\u{1FAE0}"));
  node.prepare(Font::new("sans-serif", 13.0).weight(700), &mut text);
  let glyphs: Vec<u16> = node
    .runs()
    .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.glyph_id))
    .collect();
  assert!(
    glyphs.iter().all(|&id| id != 0),
    "a glyph is missing: {glyphs:?}"
  );
}

#[test]
#[ignore = "needs FONTCONFIG_FILE with only a variable font; see the comment above"]
fn a_variable_font_draws_heavier_at_a_heavier_weight() {
  let mut text = TextSystem::new();
  let mut ink = |weight: u16| -> u32 {
    let mut node = TextNode::new();
    node.set_content(String::from("\u{1F600}"));
    node.prepare(Font::new("Noto Emoji", 40.0).weight(weight), &mut text);
    let glyph = node
      .runs()
      .flat_map(|run| run.glyphs.iter())
      .next()
      .expect("one glyph")
      .physical((0.0, 0.0), 1.0);
    let image = cosmic_text::SwashCache::new()
      .get_image_uncached(text.font_system(), glyph.cache_key)
      .expect("the glyph rasterizes");
    image.data.iter().map(|&coverage| u32::from(coverage)).sum()
  };
  let light = ink(300);
  let bold = ink(700);
  assert!(
    bold > light + light / 10,
    "weight 300 ink {light}, weight 700 ink {bold}"
  );
}
