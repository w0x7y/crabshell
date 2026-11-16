//! Text: finding fonts through fontconfig, and the cosmic-text buffer each
//! [`Text`](crate::Text) widget is measured with and drawn from.
//!
//! Scanning every installed font would cost tens of milliseconds at startup,
//! so the font database starts empty. Each family a style names is resolved
//! through fontconfig and only the file it matched is loaded. When a text
//! uses a character none of the loaded fonts has, fontconfig names a font
//! that has it, that file is loaded too, and the text is shaped again.
//!
//! A font a config bundles with [`Cx::add_font`](crate::Cx::add_font) is
//! read from its bytes, and a family it provides is used before fontconfig
//! is asked, so a config whose fonts are all bundled never starts it.

use std::cell::{Cell, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use cosmic_text::fontdb::{self, Database, FaceInfo, ID, Query, Source};
use cosmic_text::{
  Align, Attrs, Buffer, Ellipsize, EllipsizeHeightLimit, Family, FeatureTag, FontFeatures,
  FontSystem, LayoutGlyph, LayoutRun, Metrics, Shaping, Weight, Wrap,
};
use fontconfig::{CharSet, FC_FAMILY, FC_SLANT, FC_SLANT_ITALIC, FC_WEIGHT, Fontconfig, Pattern};
use taffy::AvailableSpace;

use crate::geometry::{Point, Rect};
use crate::rich::Span;
use crate::style::{Font, Style, TextAlign};
use crate::widgets::Elide;

/// The fonts every window measures and draws text with.
///
/// There is one per process, and cloning it gives another handle to the
/// same fonts. The platform passes it to
/// [`Window::layout`](crate::Window::layout), the renderer rasterizes
/// glyphs with its [`font_system`](Self::font_system), and the root [`Cx`]
/// gets it as a service for [`Cx::measure_text`] and [`Cx::add_font`].
///
/// [`Cx`]: crate::Cx
/// [`Cx::measure_text`]: crate::Cx::measure_text
/// [`Cx::add_font`]: crate::Cx::add_font
#[derive(Clone, Default)]
pub struct TextSystem(Rc<RefCell<Fonts>>);

impl TextSystem {
  /// An empty font database. Nothing is loaded until text is laid out.
  pub fn new() -> TextSystem {
    TextSystem::default()
  }

  /// cosmic-text's font system, for rasterizing glyphs. Release it before
  /// laying a window out.
  pub fn font_system(&self) -> RefMut<'_, FontSystem> {
    RefMut::map(self.fonts(), |fonts| &mut fonts.fonts)
  }

  /// The fonts, borrowed.
  pub(crate) fn fonts(&self) -> RefMut<'_, Fonts> {
    self
      .0
      .try_borrow_mut()
      .expect("nothing that holds the fonts runs user code, which may measure text")
  }
}

thread_local! {
  /// How many fonts were added. Text shaped before the last one shapes
  /// again, and windows laid out before it lay out again.
  static FONTS_ADDED: Cell<u64> = const { Cell::new(0) };
}

pub(crate) fn fonts_added() -> u64 {
  FONTS_ADDED.get()
}

/// The size of text shaped with one font, from
/// [`Cx::measure_text`](crate::Cx::measure_text), in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextMetrics {
  /// The advance of the widest line.
  pub width: f32,
  /// The line boxes: one line height per line.
  pub height: f32,
  /// How far the font rises above the baseline at this size.
  pub ascent: f32,
  /// How far the font reaches below the baseline at this size, positive.
  pub descent: f32,
  /// The height of one line, as a `Text` with this font has.
  pub line_height: f32,
}

/// What a [`TextSystem`] holds.
pub(crate) struct Fonts {
  fonts: FontSystem,
  fontconfig: Lookup,
  /// The family name each requested family, weight and slant resolved
  /// to, as the font database knows it. `None` if nothing was found.
  families: HashMap<(&'static str, u16, bool), Option<Rc<str>>>,
  /// The family name of each face asked for, by file and face number.
  pub(crate) loaded: HashMap<(PathBuf, u32), Option<Rc<str>>>,
  /// Files read into the database. Reading one loads all its faces.
  files: HashSet<PathBuf>,
  /// Characters a fallback font was already looked up for, by the family,
  /// weight and slant asked for: fontconfig's answer depends on all three.
  fallback_tried: HashSet<(char, &'static str, u16, bool)>,
  /// The weights each bundled face draws, lowest and highest: one, or a
  /// variable font's whole weight axis.
  bundled: HashMap<ID, (u16, u16)>,
}

/// fontconfig, started on first use because that costs about 15 ms.
enum Lookup {
  NotStarted,
  Ready(Fontconfig),
  Failed,
}

impl Fonts {
  /// An empty font database.
  pub(crate) fn new() -> Fonts {
    // cosmic-text needs a locale. Reading the system's would take another
    // crate, and it only affects which script-specific fallbacks it tries.
    let fonts = FontSystem::new_with_locale_and_db("en-US".to_owned(), Database::new());
    Fonts {
      fonts,
      fontconfig: Lookup::NotStarted,
      families: HashMap::new(),
      loaded: HashMap::new(),
      files: HashSet::new(),
      fallback_tried: HashSet::new(),
      bundled: HashMap::new(),
    }
  }

  /// cosmic-text's font system.
  pub(crate) fn font_system(&mut self) -> &mut FontSystem {
    &mut self.fonts
  }

  /// Whether fontconfig was started.
  #[cfg(test)]
  pub(crate) fn fontconfig_started(&self) -> bool {
    !matches!(self.fontconfig, Lookup::NotStarted)
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

  /// The name to ask cosmic-text for to get `font`'s family, weight and
  /// slant: a bundled family of that name, or else the file fontconfig
  /// matches, loaded the first time. `None`, logged once, if no font
  /// could be loaded.
  pub(crate) fn family(&mut self, font: &Font) -> Option<Rc<str>> {
    let key = (font.family, font.weight, font.italic);
    if let Some(name) = self.families.get(&key) {
      return name.clone();
    }
    if let Some(name) = self.bundled(font) {
      self.families.insert(key, Some(Rc::clone(&name)));
      return Some(name);
    }
    let matched = self
      .fontconfig()
      .and_then(|fontconfig| match_font(fontconfig, font, None));
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

  /// The database's name for `font`'s family if a bundled face of it
  /// draws its weight and slant: italic or oblique for italic text,
  /// upright for the rest.
  fn bundled(&self, font: &Font) -> Option<Rc<str>> {
    let db = self.fonts.db();
    self
      .bundled
      .iter()
      .filter(|(_, (low, high))| (*low..=*high).contains(&font.weight))
      .filter_map(|(&id, _)| db.face(id))
      .filter(|face| (face.style != fontdb::Style::Normal) == font.italic)
      .flat_map(|face| &face.families)
      .find(|(name, _language)| name.eq_ignore_ascii_case(font.family))
      .map(|(name, _language)| Rc::from(name.as_str()))
  }

  /// Reads every face in `bytes` into the database, without copying them.
  /// Families they provide are resolved again, and all text shapes again.
  /// Bytes with no face are logged and ignored.
  pub(crate) fn add(&mut self, bytes: &'static [u8]) {
    // fontdb keeps a font's bytes behind an `Arc`, whatever the thread.
    let source = Source::Binary(Arc::new(bytes));
    // Read on its own first, for the families, styles and weights it has.
    let mut probe = Database::new();
    probe.load_font_source(source.clone());
    let faces: Vec<_> = probe
      .faces()
      .map(|face| (face, weights(&probe, face)))
      .collect();
    if faces.is_empty() {
      tracing::warn!(
        bytes = bytes.len(),
        "a bundled font has no face that could be read, so it is ignored"
      );
      return;
    }
    let names: Vec<&str> = faces
      .iter()
      .flat_map(|(face, _)| face.families.iter().map(|(name, _)| name.as_str()))
      .collect();
    tracing::debug!(?names, "font added");
    let same_family = |a: &FaceInfo, b: &FaceInfo| {
      a.families.iter().any(|(name, _)| {
        b.families
          .iter()
          .any(|(other, _)| name.eq_ignore_ascii_case(other))
      })
    };
    // A face loaded from a file that a bundled one replaces, of the same
    // family, style and weight, gives way: cosmic-text takes the first face
    // of a name, and the bundled ones fill their places.
    let replaced: Vec<_> = self
      .fonts
      .db()
      .faces()
      .filter(|old| {
        faces.iter().any(|(new, (low, high))| {
          new.style == old.style && (*low..=*high).contains(&old.weight.0) && same_family(new, old)
        })
      })
      .filter_map(|old| {
        let family = old
          .families
          .first()
          .map(|(name, _)| Rc::from(name.as_str()));
        Some((
          old.id,
          (source_path(&old.source)?.clone(), old.index),
          family,
        ))
      })
      .collect();
    let db = self.fonts.db_mut();
    for (id, face, family) in replaced {
      db.remove_face(id);
      // Its file stays read, so it isn't read again; asked for, it gives
      // its family's name, which now finds the bundled face.
      self.loaded.entry(face).or_insert(family);
    }
    // fontdb reads the faces in the same order the probe did.
    let ids = db.load_font_source(source);
    for (id, (_, weights)) in ids.into_iter().zip(&faces) {
      self.bundled.insert(id, *weights);
    }
    self
      .families
      .retain(|(family, _, _), _| !names.iter().any(|name| name.eq_ignore_ascii_case(family)));
    // A character a removed face drew is looked up again if it is missing.
    self.fallback_tried.clear();
    FONTS_ADDED.set(FONTS_ADDED.get() + 1);
  }

  /// Shapes `content` as a `Text` with `font` shapes it, every line shown.
  pub(crate) fn measure(&mut self, content: &str, font: Font) -> TextMetrics {
    let mut node = TextNode::new();
    node.set_content(content.to_owned());
    node.set_max_lines(u32::MAX);
    node.prepare(font, self);
    let Metrics {
      font_size,
      line_height,
    } = node.buffer.metrics();
    let weight = Weight(font.weight);
    let face = self.family(&font).and_then(|name| {
      self.fonts.db().query(&Query {
        families: &[Family::Name(&name)],
        weight,
        style: if font.italic {
          fontdb::Style::Italic
        } else {
          fontdb::Style::Normal
        },
        ..Query::default()
      })
    });
    let (ascent, descent) =
      face
        .and_then(|id| self.fonts.get_font(id, weight))
        .map_or((0.0, 0.0), |face| {
          let metrics = face.metrics();
          let scale = font_size / f32::from(metrics.units_per_em);
          (metrics.ascent * scale, -metrics.descent * scale)
        });
    TextMetrics {
      width: node.width(),
      height: node.height(),
      ascent,
      descent,
      line_height,
    }
  }

  /// Loads fontconfig's best font for each character in `missing` that
  /// no loaded font has, preferring `font`'s family. Returns whether the
  /// database gained a face, so the text should be shaped again.
  fn load_fallbacks(&mut self, font: &Font, missing: &[char]) -> bool {
    let faces = self.fonts.db().len();
    for &ch in missing {
      if !self
        .fallback_tried
        .insert((ch, font.family, font.weight, font.italic))
      {
        continue;
      }
      let Some(file) = self
        .fontconfig()
        .and_then(|fontconfig| match_font(fontconfig, font, Some(ch)))
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

impl Default for Fonts {
  fn default() -> Self {
    Self::new()
  }
}

/// The weights a face draws, lowest and highest: its weight axis if it is a
/// variable font, else its one weight.
fn weights(db: &Database, face: &FaceInfo) -> (u16, u16) {
  let wght = u32::from_be_bytes(*b"wght");
  let axis = cosmic_text::Font::new(db, face.id, face.weight).and_then(|font| {
    let axis = font
      .as_swash()
      .variations()
      .find(|axis| axis.tag() == wght)?;
    // Weights are 1 to 1000, so the conversions can't overflow.
    Some((axis.min_value() as u16, axis.max_value() as u16))
  });
  axis.unwrap_or((face.weight.0, face.weight.0))
}

fn source_path(source: &Source) -> Option<&PathBuf> {
  match source {
    Source::File(path) | Source::SharedFile(path, _) => Some(path),
    Source::Binary(_) => None,
  }
}

/// Asks fontconfig for the font file and face index that best match
/// `font`'s family, weight and slant, and that have `covering` if given.
fn match_font(
  fontconfig: &Fontconfig,
  font: &Font,
  covering: Option<char>,
) -> Option<(PathBuf, u32)> {
  let mut pattern = Pattern::new(fontconfig).ok()?;
  pattern
    .add_string(FC_FAMILY, &CString::new(font.family).ok()?)
    .ok()?;
  pattern
    .add_integer(FC_WEIGHT, fc_weight(font.weight))
    .ok()?;
  if font.italic {
    pattern.add_integer(FC_SLANT, FC_SLANT_ITALIC).ok()?;
  }
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

/// What a text takes from its own style and its ancestors': the font,
/// which lays it out, and how its lines sit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TextStyle {
  pub(crate) font: Font,
  pub(crate) align: Option<TextAlign>,
  pub(crate) line_height: Option<f32>,
}

impl TextStyle {
  /// This inherited style with what `style` sets laid over it.
  pub(crate) fn inherit(self, style: &Style) -> TextStyle {
    TextStyle {
      font: style.font.unwrap_or(self.font),
      align: style.text_align.or(self.align),
      line_height: style.line_height.or(self.line_height),
    }
  }
}

impl From<Font> for TextStyle {
  fn from(font: Font) -> TextStyle {
    TextStyle {
      font,
      align: None,
      line_height: None,
    }
  }
}

/// The font a span is shaped in: the text's, bold or italic if it says.
fn span_font(span: &Span, font: Font) -> Font {
  Font {
    weight: if span.bold { 700 } else { font.weight },
    italic: font.italic || span.italic,
    ..font
  }
}

/// What cosmic-text shapes `font` with, in `family` if one was found.
fn attrs<'a>(font: &Font, family: Option<&'a str>) -> Attrs<'a> {
  let mut attrs = Attrs::new().weight(Weight(font.weight));
  if let Some(family) = family {
    attrs = attrs.family(Family::Name(family));
  }
  if font.italic {
    attrs = attrs.style(cosmic_text::Style::Italic);
  }
  // cosmic-text counts letter spacing in ems. Taking back at most half
  // an em keeps glyphs within the em the ink box allows left of the text.
  if font.letter_spacing.is_finite() && font.letter_spacing != 0.0 {
    attrs = attrs.letter_spacing((font.letter_spacing / font_size(font)).max(-0.5));
  }
  if !font.features.is_empty() {
    let mut features = FontFeatures::new();
    for feature in font.features {
      features.set(FeatureTag::new(&feature.tag), feature.value);
    }
    attrs = attrs.font_features(features);
  }
  attrs
}

/// The font size, falling back to the default for a size cosmic-text can't
/// use, and the line height in whole pixels, at least 1: 1.2 times the
/// size unless the style sets a usable multiple.
fn metrics(style: &TextStyle) -> Metrics {
  let size = font_size(&style.font);
  let multiple = style
    .line_height
    .filter(|multiple| multiple.is_finite() && *multiple > 0.0)
    .unwrap_or(1.2);
  Metrics::new(size, (size * multiple).round().max(1.0))
}

fn font_size(font: &Font) -> f32 {
  if font.size.is_finite() && font.size >= 1.0 {
    font.size
  } else {
    Font::DEFAULT.size
  }
}

/// The text of one [`Text`](crate::Text) widget, shaped and laid out by
/// cosmic-text. Layout measures with this buffer and the renderer draws from
/// it, so the measured and drawn sizes are the same.
pub(crate) struct TextNode {
  /// The text shown; a rich text's spans, joined.
  pub(crate) content: String,
  /// A rich text's spans. Each glyph's metadata is its span's index plus
  /// one, and 0 for text outside any span, such as an ellipsis.
  pub(crate) spans: Vec<Span>,
  pub(crate) elide: Elide,
  /// At least 1.
  pub(crate) max_lines: u32,
  /// More than one line wraps at words; off, each line is as long as it
  /// is. A text area's setting.
  pub(crate) wrap: bool,
  pub(crate) buffer: Buffer,
  /// The style the buffer was shaped with; `None` until the first layout.
  style: Option<TextStyle>,
  /// [`fonts_added`] when the buffer was shaped.
  fonts_added: u64,
  /// The content, `elide` or `max_lines` changed since the buffer was
  /// shaped.
  stale: bool,
  /// The top-left corner of the first line, set by layout.
  pub(crate) origin: Point,
  /// Takes the pointer and selects, as `Text::selectable` says.
  pub(crate) selectable: bool,
  /// Where a selection ends and starts, in bytes of `content`, while the
  /// text is selectable.
  pub(crate) cursor: usize,
  pub(crate) anchor: Option<usize>,
}

impl TextNode {
  pub(crate) fn new() -> TextNode {
    TextNode {
      content: String::new(),
      spans: Vec::new(),
      elide: Elide::None,
      max_lines: 1,
      wrap: true,
      buffer: Buffer::new_empty(metrics(&Font::DEFAULT.into())),
      style: None,
      fonts_added: 0,
      stale: true,
      origin: Point::default(),
      selectable: false,
      cursor: 0,
      anchor: None,
    }
  }

  // Each setter returns whether the value changed, which needs a layout.

  pub(crate) fn set_content(&mut self, content: String) -> bool {
    self.set(|text| &mut text.content, content)
  }

  pub(crate) fn set_spans(&mut self, spans: Vec<Span>) -> bool {
    let content = spans.iter().map(|span| span.text.as_str()).collect();
    let changed = self.set(|text| &mut text.spans, spans);
    self.set_content(content) | changed
  }

  /// Drops the selection and puts the caret at the start if `changed`,
  /// since the old one's bytes may not be in the new text; returns it.
  pub(crate) fn forget_selection_if(&mut self, changed: bool) -> bool {
    if changed {
      self.anchor = None;
      self.cursor = 0;
    }
    changed
  }

  pub(crate) fn set_elide(&mut self, elide: Elide) -> bool {
    self.set(|text| &mut text.elide, elide)
  }

  pub(crate) fn set_max_lines(&mut self, max_lines: u32) -> bool {
    self.set(|text| &mut text.max_lines, max_lines.max(1))
  }

  pub(crate) fn set_wrap(&mut self, wrap: bool) -> bool {
    self.set(|text| &mut text.wrap, wrap)
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
  /// is elided. Plain one-line text is always as wide as it is.
  fn fits_width(&self) -> bool {
    self.elide != Elide::None || self.wraps()
  }

  fn wraps(&self) -> bool {
    self.max_lines > 1 && self.wrap
  }

  /// Shapes the text again if its content, settings or style changed,
  /// loading fallback fonts for characters the font lacks, or if a font
  /// was added since. Returns whether it did, so layout must measure it
  /// again.
  pub(crate) fn prepare(&mut self, style: impl Into<TextStyle>, text: &mut Fonts) -> bool {
    let style = style.into();
    if !self.stale && self.style == Some(style) && self.fonts_added == fonts_added() {
      return false;
    }
    // The text's own font, then each span's.
    let fonts: Vec<Font> = std::iter::once(style.font)
      .chain(self.spans.iter().map(|span| span_font(span, style.font)))
      .collect();
    let families: Vec<_> = fonts.iter().map(|font| text.family(font)).collect();
    self.buffer.set_metrics(metrics(&style));
    // Words wrap; one too long for a line breaks inside it.
    self.buffer.set_wrap(if self.wraps() {
      Wrap::WordOrGlyph
    } else {
      Wrap::None
    });
    let lines = EllipsizeHeightLimit::Lines(self.max_lines as usize);
    self.buffer.set_ellipsize(match self.elide {
      Elide::None => Ellipsize::None,
      Elide::Start => Ellipsize::Start(lines),
      Elide::Middle => Ellipsize::Middle(lines),
      Elide::End => Ellipsize::End(lines),
    });
    self.buffer.set_size(None, None);
    self.shape(&fonts, &families, &style, text);
    let missing = self.fallback_chars(&families, text.font_system().db());
    let mut gained = false;
    for (index, font) in fonts.iter().enumerate() {
      let chars: Vec<char> = missing
        .iter()
        .filter(|(at, _)| *at == index)
        .map(|(_, ch)| *ch)
        .collect();
      if !chars.is_empty() {
        gained |= text.load_fallbacks(font, &chars);
      }
    }
    if gained {
      self.shape(&fonts, &families, &style, text);
    }
    self.style = Some(style);
    self.fonts_added = fonts_added();
    self.stale = false;
    true
  }

  /// Shapes the text with `fonts` and `families`: the text's own first,
  /// then each span's.
  fn shape(
    &mut self,
    fonts: &[Font],
    families: &[Option<Rc<str>>],
    style: &TextStyle,
    text: &mut Fonts,
  ) {
    let own = attrs(&fonts[0], families[0].as_deref());
    if self.spans.is_empty() {
      self
        .buffer
        .set_text(&self.content, &own, Shaping::Advanced, None);
    } else {
      let spans = self.spans.iter().enumerate().map(|(index, span)| {
        let font = &fonts[index + 1];
        let mut attrs = attrs(font, families[index + 1].as_deref()).metadata(index + 1);
        if let Some(color) = span.color {
          attrs = attrs.color(cosmic_text::Color::rgba(color.r, color.g, color.b, color.a));
        }
        (span.text.as_str(), attrs)
      });
      self
        .buffer
        .set_rich_text(spans, &own, Shaping::Advanced, None);
    }
    let align = style.align.and_then(|align| match align {
      TextAlign::Start => None,
      TextAlign::Center => Some(Align::Center),
      TextAlign::End => Some(Align::End),
      TextAlign::Justify => Some(Align::Justified),
    });
    for line in &mut self.buffer.lines {
      line.set_align(align);
    }
    self.buffer.shape_until_scroll(text.font_system(), false);
  }

  /// Characters that need a fallback font, with the index in `families`
  /// of the font they were asked in: those that came out as the
  /// missing-glyph box, and those drawn from a face outside that family.
  /// The second kind may have come from a fallback loaded for another
  /// weight or family, so fontconfig is asked again for this one.
  fn fallback_chars(&self, families: &[Option<Rc<str>>], db: &Database) -> Vec<(usize, char)> {
    let in_family = |glyph: &LayoutGlyph| {
      let family = families.get(glyph.metadata).cloned().flatten();
      db.face(glyph.font_id).is_some_and(|face| {
        face
          .families
          .iter()
          .any(|(name, _)| Some(name.as_str()) == family.as_deref())
      })
    };
    let mut missing = Vec::new();
    for run in self.buffer.layout_runs() {
      let fallback = |glyph: &&LayoutGlyph| glyph.glyph_id == 0 || !in_family(glyph);
      for glyph in run.glyphs.iter().filter(fallback) {
        let index = glyph.metadata.min(families.len() - 1);
        for ch in run.text.get(glyph.start..glyph.end).unwrap_or("").chars() {
          if !missing.contains(&(index, ch)) {
            missing.push((index, ch));
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
  /// vertically there. Text with an alignment takes the box's width, so
  /// its lines sit in it.
  pub(crate) fn finish(&mut self, content: Rect, fonts: &mut FontSystem) {
    let aligned = self.style.is_some_and(|style| style.align.is_some());
    let width = (self.fits_width() || aligned).then_some(content.width);
    self.buffer.set_size(width, None);
    self.buffer.shape_until_scroll(fonts, false);
    let top = ((content.height - self.height()) / 2.0).round();
    self.origin = Point::new(content.x, content.y + top);
  }

  /// The area the glyphs may cover: the lines' box, which can stick out
  /// of a widget too small for it, grown by an em left and right and half
  /// an em above and below, plus half of what a line height under the
  /// default took. Ink reaches past the line box: stacked accents rise
  /// above it, and a combining mark at the start hangs left of the text
  /// by nearly the width of the base it lacks. Aligned lines sit across
  /// the width the text was laid out in.
  pub(crate) fn ink_box(&self) -> Rect {
    let metrics = self.buffer.metrics();
    let em = metrics.font_size;
    let squeezed = ((1.2 * em).round() - metrics.line_height).max(0.0) / 2.0;
    let width = self.buffer.size().0.unwrap_or(0.0).max(self.width());
    Rect::new(
      self.origin.x - em,
      self.origin.y - em / 2.0 - squeezed,
      width.ceil() + 2.0 * em,
      self.height() + em + 2.0 * squeezed,
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
