//! Text: resolve one font through fontconfig, lay out a string with cosmic-text,
//! and blend its grayscale glyphs into a tiny-skia pixmap.

use cosmic_text::{
  Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, fontdb,
};
use fontconfig::Fontconfig;
use tiny_skia::{ColorU8, Pixmap, PixmapMut, PixmapPaint, Transform};
use tracing::info;

/// One laid-out line of text plus everything needed to rasterize it.
///
/// The struct owns its `FontSystem` (fonts), `SwashCache` (rasterized glyphs) and
/// `Buffer` (shaped text). Owning them means nothing else can change them behind
/// our back, and they are freed together when the `Text` is dropped.
pub struct Text {
  fonts: FontSystem,
  glyph_cache: SwashCache,
  layout: Buffer,
}

impl Text {
  /// Resolves `family` through fontconfig, loads only the file it matched, and
  /// lays out `content` with `metrics` (in logical pixels).
  ///
  /// Panics with a message if no font can be loaded: the bar can't draw without one.
  pub fn new(family: &str, content: &str, metrics: Metrics) -> Text {
    let fontconfig = Fontconfig::new().expect("failed to initialise fontconfig");
    let matched = fontconfig
      .find(family, None)
      .expect("fontconfig found no font for the requested family");

    // An empty database plus one file. `FontSystem::new()` would call
    // `load_system_fonts()` and parse every font on the machine instead.
    let mut db = fontdb::Database::new();
    db.load_font_file(&matched.path)
      .expect("failed to read the font file fontconfig matched");

    // fontconfig gives the face index inside the file (it matters for .ttc
    // collections) and fontdb gives the family name exactly as cosmic-text
    // will look it up, so the two always agree.
    let index = matched
      .index
      .and_then(|i| u32::try_from(i).ok())
      .unwrap_or(0);
    let family_name = db
      .faces()
      .find(|face| face.index == index)
      .and_then(|face| face.families.first())
      .map(|(name, _language)| name.clone())
      .expect("the matched font file has no face with a family name");
    info!(
        requested = family,
        family = %family_name,
        path = %matched.path.display(),
        "font resolved through fontconfig"
    );

    let mut fonts = FontSystem::new_with_locale_and_db("en-US".to_string(), db);
    let mut layout = Buffer::new(&mut fonts, metrics);
    // `Attrs` borrows `family_name` only for the duration of this call;
    // cosmic-text copies what it needs. That borrow is the `'a` in `Attrs<'a>`.
    let attrs = Attrs::new().family(Family::Name(&family_name));
    layout.set_text(content, &attrs, Shaping::Advanced, None);
    layout.shape_until_scroll(&mut fonts, false);

    Text {
      fonts,
      glyph_cache: SwashCache::new(),
      layout,
    }
  }

  /// Draws the text into `pixmap` with its left edge `x` logical pixels from the
  /// pixmap's left edge. `scale` turns logical pixels into buffer pixels.
  /// `color` must be opaque; each glyph's coverage becomes its alpha.
  pub fn draw(&mut self, pixmap: &mut PixmapMut<'_>, x: f32, scale: f32, color: ColorU8) {
    // `self.layout`, `self.glyph_cache` and `self.fonts` are separate fields,
    // so Rust lets us read one while mutating the others in the same loop.
    for run in self.layout.layout_runs() {
      // A whole-pixel baseline keeps horizontal stems on pixel edges, so
      // text stays sharp instead of smearing across two rows.
      let baseline = (run.line_y * scale).round();
      for glyph in run.glyphs {
        let physical = glyph.physical((x * scale, baseline), scale);
        let Some(image) = self
          .glyph_cache
          .get_image(&mut self.fonts, physical.cache_key)
        else {
          continue;
        };
        // cosmic-text renders with swash's Format::Alpha: a grayscale
        // coverage mask. Colour glyphs (emoji) are out of scope here.
        if !matches!(image.content, SwashContent::Mask) {
          continue;
        }
        // Spaces have an empty image, and Pixmap::new refuses a zero size.
        let Some(mut tile) = Pixmap::new(image.placement.width, image.placement.height) else {
          continue;
        };
        for (pixel, &coverage) in tile.pixels_mut().iter_mut().zip(&image.data) {
          *pixel =
            ColorU8::from_rgba(color.red(), color.green(), color.blue(), coverage).premultiply();
        }
        // tiny-skia blends the tile with source-over and clips it to the pixmap.
        pixmap.draw_pixmap(
          physical.x + image.placement.left,
          physical.y - image.placement.top,
          tile.as_ref(),
          &PixmapPaint::default(),
          Transform::identity(),
          None,
        );
      }
    }
  }
}
