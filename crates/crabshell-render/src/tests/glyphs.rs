//! Color glyphs: vector (COLR) glyphs arrive premultiplied, bitmap emoji
//! don't. Both are checked against the glyph data, drawn over nothing.
//! They use this machine's Noto Znamenny Musical Notation, which has COLR
//! glyphs, and Noto Color Emoji, which has bitmaps.

use cosmic_text::{SwashCache, SwashContent};
use crabshell_ui::{Content, Font, Row, Style, Text};

use super::{Buffer, Scene};

/// `channel * alpha / 255`, rounded.
fn mul(channel: u8, alpha: u8) -> u8 {
  ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8
}

/// Draws `text` in `family` at 48 pixels over a transparent window, and
/// returns the buffer with the first color glyph's image and where it was
/// placed.
fn draw_color_glyph(family: &'static str, text: &str) -> (Buffer, Vec<u8>, i32, i32, u32, u32) {
  let mut scene = Scene::new();
  // Padding keeps a combining mark, which hangs left of its origin, in
  // the window.
  let panel = scene.panel().child(
    Row::new()
      .padding(20, 40)
      .child(Text::new(text).style(Style::new().font(Font::new(family, 48.0)))),
  );
  let window = scene.window(panel, 160.0, 100.0);
  let buffer = scene.full(&window, 1.0);
  let view = window.view();
  let Content::Text(text) = super::node_at(view.root(), &[0, 0]).content() else {
    panic!("a text node has text content");
  };
  let origin = text.origin();
  let mut cache = SwashCache::new();
  for run in text.runs() {
    let baseline = (origin.y + run.line_y).round();
    for glyph in run.glyphs {
      let physical = glyph.physical((origin.x, baseline), 1.0);
      let Some(image) = cache.get_image_uncached(&mut scene.text.font_system(), physical.cache_key)
      else {
        continue;
      };
      if image.content == SwashContent::Color {
        let left = physical.x + image.placement.left;
        let top = physical.y - image.placement.top;
        return (
          buffer,
          image.data,
          left,
          top,
          image.placement.width,
          image.placement.height,
        );
      }
    }
  }
  panic!("no color glyph in {family}; is the font installed?");
}

/// Checks every partly transparent pixel of a color glyph against what
/// `expected` makes of the glyph's own pixel.
fn check(family: &'static str, text: &str, expected: impl Fn([u8; 4]) -> [u8; 4]) {
  let (buffer, data, left, top, width, height) = draw_color_glyph(family, text);
  let mut checked = 0;
  for y in 0..height {
    for x in 0..width {
      let index = (y * width + x) as usize * 4;
      let mut pixel = [0; 4];
      pixel.copy_from_slice(&data[index..index + 4]);
      if pixel[3] == 0 || pixel[3] == 255 {
        continue;
      }
      let (bx, by) = (left + x as i32, top + y as i32);
      if bx < 0 || by < 0 || bx as u32 >= buffer.width || by as u32 >= buffer.height {
        continue;
      }
      let drawn = buffer.pixel(bx as u32, by as u32);
      let want = expected(pixel);
      assert!(
        drawn.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 1),
        "{family}: glyph pixel {pixel:?} drawn as {drawn:?}, expected {want:?}"
      );
      checked += 1;
    }
  }
  assert!(checked > 10, "{family}: only {checked} edge pixels checked");
}

#[test]
fn colr_glyphs_are_already_premultiplied() {
  // A Znamenny combining mark, drawn as a COLR glyph.
  check("Noto Znamenny Musical Notation", "\u{1CF01}", |pixel| pixel);
}

#[test]
fn bitmap_emoji_are_premultiplied_once() {
  check("Noto Color Emoji", "\u{1F600}", |[r, g, b, a]| {
    [mul(r, a), mul(g, a), mul(b, a), a]
  });
}

#[test]
fn the_glyph_cache_is_cleared_once_it_grows_large() {
  use crabshell_ui::Column;

  let mut scene = Scene::new();
  // About 155 characters at 13 sizes: some 2,500 glyph images, past the
  // limit of 2,048.
  let text: String = (0x21_u32..=0x7E)
    .chain(0xC0..=0x11F)
    .filter_map(char::from_u32)
    .collect();
  let column = (8..21).fold(Column::new(), |column, size| {
    column
      .child(Text::new(text.clone()).style(Style::new().font(Font::new("sans-serif", size as f32))))
  });
  let window = scene.window(scene.panel().child(column), 4000.0, 400.0);
  scene.full(&window, 1.0);
  assert!(
    scene.renderer.cached_glyphs() <= 1024,
    "{}",
    scene.renderer.cached_glyphs()
  );
  // A small window afterward fills it again only as far as it needs.
  let small = scene.window(scene.panel().child(Text::new("abc")), 60.0, 20.0);
  scene.full(&small, 1.0);
  assert!((1..=2048).contains(&scene.renderer.cached_glyphs()));
}
