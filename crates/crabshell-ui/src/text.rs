//! Text: finding fonts through fontconfig, and the cosmic-text buffer each
//! [`Text`](crate::Text) widget is measured with and drawn from.
//!
//! Scanning every installed font would cost tens of milliseconds at startup,
//! so the font database starts empty. Each family a style names is resolved
//! through fontconfig and only the file it matched is loaded. When a text
//! uses a character none of the loaded fonts has, fontconfig names a font
//! that has it, that file is loaded too, and the text is shaped again.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::path::PathBuf;
use std::rc::Rc;

use cosmic_text::fontdb::{Database, Source};
use cosmic_text::{
  Attrs, Buffer, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, LayoutGlyph, LayoutRun,
  Metrics, Shaping, Weight, Wrap,
};
use fontconfig::{CharSet, FC_FAMILY, FC_WEIGHT, Fontconfig, Pattern};
use taffy::AvailableSpace;

use crate::geometry::{Point, Rect};
use crate::style::Font;

/// The fonts every window measures and draws text with.
///
/// There is one per process. The platform passes it to
/// [`Window::layout`](crate::Window::layout), and the renderer rasterizes
/// glyphs with its [`font_system`](Self::font_system).
pub struct TextSystem {
  fonts: FontSystem,
  fontconfig: Lookup,
  /// The family name each requested family and weight resolved to, as the
  /// font database knows it. `None` if nothing was found.
  families: HashMap<(&'static str, u16), Option<Rc<str>>>,
  /// The family name of each face asked for, by file and face number.
  loaded: HashMap<(PathBuf, u32), Option<Rc<str>>>,
  /// Files read into the database. Reading one loads all its faces.
  files: HashSet<PathBuf>,
  /// Characters a fallback font was already looked up for, by the family
  /// and weight asked for: fontconfig's answer depends on both.
  fallback_tried: HashSet<(char, &'static str, u16)>,
}

/// fontconfig, started on first use because that costs about 15 ms.
enum Lookup {
  NotStarted,
  Ready(Fontconfig),
  Failed,
}

impl TextSystem {
  /// An empty font database. Nothing is loaded until text is laid out.
  pub fn new() -> TextSystem {
    // cosmic-text needs a locale. Reading the system's would take another
    // crate, and it only affects which script-specific fallbacks it tries.
    let fonts = FontSystem::new_with_locale_and_db("en-US".to_owned(), Database::new());
    TextSystem {
      fonts,
      fontconfig: Lookup::NotStarted,
      families: HashMap::new(),
      loaded: HashMap::new(),
      files: HashSet::new(),
      fallback_tried: HashSet::new(),
    }
  }

  /// cosmic-text's font system, for rasterizing glyphs.
  pub fn font_system(&mut self) -> &mut FontSystem {
    &mut self.fonts
  }

  fn fontconfig(&mut self) -> Option<&Fontconfig> {
    if let Lookup::NotStarted = self.fontconfig {
      self.fontconfig = match Fontconfig::new() {
        Some(fontconfig) => Lookup::Ready(fontconfig),
        None => {
          tracing::error!("fontconfig failed to start, so no fonts can be loaded");
          Lookup::Failed
        }
      };
    }
    match &self.fontconfig {
      Lookup::Ready(fontconfig) => Some(fontconfig),
      _ => None,
    }
  }

  /// The name to ask cosmic-text for to get `font`'s family and weight,
  /// loading the file fontconfig matches the first time. `None`, logged
  /// once, if no font could be loaded.
  pub(crate) fn family(&mut self, font: &Font) -> Option<Rc<str>> {
    let key = (font.family, font.weight);
    if let Some(name) = self.families.get(&key) {
      return name.clone();
    }
    let matched = self
      .fontconfig()
      .and_then(|fontconfig| match_font(fontconfig, font.family, font.weight, None));
    let name = matched.and_then(|file| self.load(file));
    match &name {
      Some(name) => tracing::debug!(requested = font.family, family = %name, "font loaded"),
      None => tracing::warn!(
        requested = font.family,
        "no font found for this family, so its text shows nothing"
      ),
    }
    self.families.insert(key, name.clone());
    name
  }

  /// Loads fontconfig's best font for each character in `missing` that
  /// no loaded font has, preferring `font`'s family. Returns whether the
  /// database gained a face, so the text should be shaped again.
  fn load_fallbacks(&mut self, font: &Font, missing: &[char]) -> bool {
    let faces = self.fonts.db().len();
    for &ch in missing {
      if !self.fallback_tried.insert((ch, font.family, font.weight)) {
        continue;
      }
      let Some(file) = self
        .fontconfig()
        .and_then(|fontconfig| match_font(fontconfig, font.family, font.weight, Some(ch)))
      else {
        continue;
      };
      self.load(file);
    }
    self.fonts.db().len() > faces
  }

  /// Reads a font file into the database once, and returns the family name
  /// of its face `index`.
  pub(crate) fn load(&mut self, (path, index): (PathBuf, u32)) -> Option<Rc<str>> {
    let key = (path, index);
    if let Some(name) = self.loaded.get(&key) {
      return name.clone();
    }
    let (path, index) = &key;
    if !self.files.contains(path) {
      self.files.insert(path.clone());
      if let Err(error) = self.fonts.db_mut().load_font_file(path) {
        tracing::warn!(path = %path.display(), %error, "couldn't read a font file");
      }
    }
    let name = self
      .fonts
      .db()
      .faces()
      .find(|face| face.index == *index && source_path(&face.source) == Some(path))
      .and_then(|face| face.families.first())
      .map(|(name, _language)| Rc::from(name.as_str()));
    self.loaded.insert(key, name.clone());
    name
  }
}

impl Default for TextSystem {
  fn default() -> Self {
    Self::new()
  }
}

fn source_path(source: &Source) -> Option<&PathBuf> {
  match source {
    Source::File(path) | Source::SharedFile(path, _) => Some(path),
    Source::Binary(_) => None,
  }
}

/// Asks fontconfig for the font file and face index that best match
/// `family` and `weight`, and that have `covering` if given.
fn match_font(
  fontconfig: &Fontconfig,
  family: &str,
  weight: u16,
  covering: Option<char>,
) -> Option<(PathBuf, u32)> {
  let mut pattern = Pattern::new(fontconfig).ok()?;
  pattern
    .add_string(FC_FAMILY, &CString::new(family).ok()?)
    .ok()?;
  pattern.add_integer(FC_WEIGHT, fc_weight(weight)).ok()?;
  if let Some(ch) = covering {
    let mut chars = CharSet::new(fontconfig).ok()?;
    chars.add_char(ch).ok()?;
    pattern.add_charset(chars).ok()?;
  }
  let matched = pattern.font_match().ok()?;
  let path = PathBuf::from(matched.filename().ok()?);
  let index = matched.face_index().ok().map_or(0, face_number);
  Some((path, index))
}

/// The face number fontdb uses for a fontconfig face index. fontconfig puts
/// a variable font's named instance, such as Bold, in the upper 16 bits;
/// fontdb numbers only the faces in the file, and cosmic-text sets the
/// weight axis itself.
pub(crate) fn face_number(index: i32) -> u32 {
  // The mask leaves 0 to 65535, so the conversion can't fail.
  u32::try_from(index & 0xFFFF).unwrap_or(0)
}

/// Converts a CSS weight (100 to 1000) to fontconfig's scale, which puts
/// regular at 80 and bold at 200. The points are fontconfig's own
/// OpenType table; weights between them are interpolated.
pub(crate) fn fc_weight(weight: u16) -> i32 {
  const POINTS: [(f32, f32); 12] = [
    (100.0, 0.0),
    (200.0, 40.0),
    (300.0, 50.0),
    (350.0, 55.0),
    (380.0, 75.0),
    (400.0, 80.0),
    (500.0, 100.0),
    (600.0, 180.0),
    (700.0, 200.0),
    (800.0, 205.0),
    (900.0, 210.0),
    (1000.0, 215.0),
  ];
  let weight = f32::from(weight).clamp(100.0, 1000.0);
  let upper = POINTS
    .iter()
    .position(|&(css, _)| css >= weight)
    .unwrap_or(POINTS.len() - 1)
    .max(1);
  let (css_low, fc_low) = POINTS[upper - 1];
  let (css_high, fc_high) = POINTS[upper];
  let fc = fc_low + (weight - css_low) / (css_high - css_low) * (fc_high - fc_low);
  fc.round() as i32
}

/// The font size, falling back to the default for a size cosmic-text can't
/// use, and a line height of 1.2 times it in whole pixels.
fn metrics(font: &Font) -> Metrics {
  let size = if font.size.is_finite() && font.size >= 1.0 {
    font.size
  } else {
    Font::DEFAULT.size
  };
  Metrics::new(size, (size * 1.2).round())
}

/// The text of one [`Text`](crate::Text) widget, shaped and laid out by
/// cosmic-text. Layout measures with this buffer and the renderer draws from
/// it, so the measured and drawn sizes are the same.
pub(crate) struct TextNode {
  pub(crate) content: String,
  pub(crate) ellipsis: bool,
  /// At least 1.
  pub(crate) max_lines: u32,
  pub(crate) buffer: Buffer,
  /// The font the buffer was shaped with; `None` until the first layout.
  font: Option<Font>,
  /// The content, `ellipsis` or `max_lines` changed since the buffer was
  /// shaped.
  stale: bool,
  /// The top-left corner of the first line, set by layout.
  pub(crate) origin: Point,
}

impl TextNode {
  pub(crate) fn new() -> TextNode {
    TextNode {
      content: String::new(),
      ellipsis: false,
      max_lines: 1,
      buffer: Buffer::new_empty(metrics(&Font::DEFAULT)),
      font: None,
      stale: true,
      origin: Point::default(),
    }
  }

  // Each setter returns whether the value changed, which needs a layout.

  pub(crate) fn set_content(&mut self, content: String) -> bool {
    self.set(|text| &mut text.content, content)
  }

  pub(crate) fn set_ellipsis(&mut self, ellipsis: bool) -> bool {
    self.set(|text| &mut text.ellipsis, ellipsis)
  }

  pub(crate) fn set_max_lines(&mut self, max_lines: u32) -> bool {
    self.set(|text| &mut text.max_lines, max_lines.max(1))
  }

  fn set<T: PartialEq>(&mut self, field: impl FnOnce(&mut TextNode) -> &mut T, value: T) -> bool {
    let slot = field(self);
    if *slot == value {
      return false;
    }
    *slot = value;
    self.stale = true;
    true
  }

  /// Whether the text's width depends on the space it gets: it wraps or
  /// ends in an ellipsis. Plain one-line text is always as wide as it is.
  fn fits_width(&self) -> bool {
    self.ellipsis || self.max_lines > 1
  }

  /// Shapes the text again if its content, settings or `font` changed,
  /// loading fallback fonts for characters the font lacks. Returns whether
  /// it did, so layout must measure it again.
  pub(crate) fn prepare(&mut self, font: Font, text: &mut TextSystem) -> bool {
    if !self.stale && self.font == Some(font) {
      return false;
    }
    let family = text.family(&font);
    self.buffer.set_metrics(metrics(&font));
    // Words wrap; one too long for a line breaks inside it.
    self.buffer.set_wrap(if self.max_lines > 1 {
      Wrap::WordOrGlyph
    } else {
      Wrap::None
    });
    self.buffer.set_ellipsize(if self.ellipsis {
      Ellipsize::End(EllipsizeHeightLimit::Lines(self.max_lines as usize))
    } else {
      Ellipsize::None
    });
    self.buffer.set_size(None, None);
    self.shape(family.as_deref(), &font, text);
    let missing = self.fallback_chars(family.as_deref(), text.font_system().db());
    if !missing.is_empty() && text.load_fallbacks(&font, &missing) {
      self.shape(family.as_deref(), &font, text);
    }
    self.font = Some(font);
    self.stale = false;
    true
  }

  fn shape(&mut self, family: Option<&str>, font: &Font, text: &mut TextSystem) {
    let attrs = Attrs::new().weight(Weight(font.weight));
    let attrs = match family {
      Some(family) => attrs.family(Family::Name(family)),
      None => attrs,
    };
    self
      .buffer
      .set_text(&self.content, &attrs, Shaping::Advanced, None);
    self.buffer.shape_until_scroll(text.font_system(), false);
  }

  /// Characters that need a fallback font: those that came out as the
  /// missing-glyph box, and those drawn from a face outside `family`.
  /// The second kind may have come from a fallback loaded for another
  /// weight or family, so fontconfig is asked again for this one.
  fn fallback_chars(&self, family: Option<&str>, db: &Database) -> Vec<char> {
    let in_family = |id| {
      db.face(id).is_some_and(|face| {
        face
          .families
          .iter()
          .any(|(name, _)| Some(name.as_str()) == family)
      })
    };
    let mut missing = Vec::new();
    for run in self.buffer.layout_runs() {
      let fallback = |glyph: &&LayoutGlyph| glyph.glyph_id == 0 || !in_family(glyph.font_id);
      for glyph in run.glyphs.iter().filter(fallback) {
        for ch in run.text.get(glyph.start..glyph.end).unwrap_or("").chars() {
          if !missing.contains(&ch) {
            missing.push(ch);
          }
        }
      }
    }
    missing
  }

  /// The size taffy asks for: as wide as the text, or as `available`
  /// allows for text that wraps or ends in an ellipsis, and one line
  /// height per line shown. Sizes are rounded up to whole pixels so that
  /// rounding the layout never cuts the text short.
  pub(crate) fn measure(
    &mut self,
    fonts: &mut FontSystem,
    known: taffy::Size<Option<f32>>,
    available: taffy::Size<AvailableSpace>,
  ) -> taffy::Size<f32> {
    let width = if self.fits_width() {
      known.width.or(match available.width {
        AvailableSpace::Definite(width) => Some(width),
        AvailableSpace::MinContent => Some(0.0),
        AvailableSpace::MaxContent => None,
      })
    } else {
      None
    };
    self.buffer.set_size(width, None);
    self.buffer.shape_until_scroll(fonts, false);
    taffy::Size {
      width: known.width.unwrap_or_else(|| self.width().ceil()),
      height: known.height.unwrap_or_else(|| self.height().ceil()),
    }
  }

  /// Lays the text out in its final `content` box and centers it
  /// vertically there.
  pub(crate) fn finish(&mut self, content: Rect, fonts: &mut FontSystem) {
    let width = self.fits_width().then_some(content.width);
    self.buffer.set_size(width, None);
    self.buffer.shape_until_scroll(fonts, false);
    let top = ((content.height - self.height()) / 2.0).round();
    self.origin = Point::new(content.x, content.y + top);
  }

  /// The area the glyphs may cover: the lines' box, which can stick out
  /// of a widget too small for it, grown by an em left and right and half
  /// an em above and below. Ink reaches past the line box: stacked accents
  /// rise above it, and a combining mark at the start hangs left of the
  /// text by nearly the width of the base it lacks.
  pub(crate) fn ink_box(&self) -> Rect {
    let em = self.buffer.metrics().font_size;
    Rect::new(
      self.origin.x - em,
      self.origin.y - em / 2.0,
      self.width().ceil() + 2.0 * em,
      self.height() + em,
    )
  }

  /// The lines shown, at most `max_lines`.
  pub(crate) fn runs(&self) -> impl Iterator<Item = LayoutRun<'_>> {
    self.buffer.layout_runs().take(self.max_lines as usize)
  }

  fn width(&self) -> f32 {
    self.runs().map(|run| run.line_w).fold(0.0, f32::max)
  }

  fn height(&self) -> f32 {
    self.runs().map(|run| run.line_height).sum()
  }
}
