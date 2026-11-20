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

use std::borrow::Cow;
use std::cell::{Cell, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use cosmic_text::fontdb::{self, Database, FaceInfo, ID, Query, Source};
use cosmic_text::{
  Align, Attrs, Buffer, CacheKey, CacheKeyFlags, Command, Ellipsize, EllipsizeHeightLimit, Family,
  FeatureTag, FontFeatures, FontSystem, LayoutGlyph, LayoutRun, Metrics, Shaping, SwashCache,
  Weight, Wrap,
};
use fontconfig::{CharSet, FC_FAMILY, FC_SLANT, FC_SLANT_ITALIC, FC_WEIGHT, Fontconfig, Pattern};
use taffy::AvailableSpace;

use crate::geometry::{Point, Rect};
use crate::input::LinkHandler;
use crate::rich::Span;
use crate::style::{Color, Font, Style, TextAlign};
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
  /// The tight box around the ink the glyphs draw, from the text's
  /// top-left, as Qt's `tightBoundingRect`: `x` is negative when a glyph
  /// hangs left of where the text starts, and the box can be wider than
  /// [`width`](Self::width) when ink reaches past the last advance, as a
  /// Nerd Font icon in a monospace cell does. Read from the unhinted
  /// outlines, unrounded; a color bitmap glyph, such as an emoji, gives
  /// its pixels' box. Empty text, or only blanks, gives an empty rect at
  /// 0, 0.
  pub ink: Rect,
}

/// How many glyphs' ink boxes [`Fonts`] keeps, at about 50 bytes each;
/// past it the cache starts again.
pub(crate) const INK_GLYPHS: usize = 4096;

/// Calls `add` with every point where the outline `commands` reach
/// furthest along either axis.
fn outline_extremes(commands: &[Command], mut add: impl FnMut(f32, f32)) {
  let mut last = (0.0, 0.0);
  for command in commands {
    last = match *command {
      Command::MoveTo(p) | Command::LineTo(p) => curve_extremes(&[last, (p.x, p.y)], &mut add),
      Command::QuadTo(c, p) => curve_extremes(&[last, (c.x, c.y), (p.x, p.y)], &mut add),
      Command::CurveTo(c1, c2, p) => {
        curve_extremes(&[last, (c1.x, c1.y), (c2.x, c2.y), (p.x, p.y)], &mut add)
      }
      Command::Close => continue,
    };
  }
}

/// Calls `add` with the end of the line, quadratic or cubic Bézier
/// through `points`, and with each point between its ends where it turns
/// back on an axis. A curve's control points may lie outside it, so they
/// aren't the box. Returns the end.
pub(crate) fn curve_extremes(points: &[(f32, f32)], add: &mut impl FnMut(f32, f32)) -> (f32, f32) {
  let end = points[points.len() - 1];
  add(end.0, end.1);
  let axis = |point: &(f32, f32), y: bool| if y { point.1 } else { point.0 };
  let at = |t: f32, y: bool| -> f32 {
    let u = 1.0 - t;
    match points {
      [a, b, c] => u * u * axis(a, y) + 2.0 * u * t * axis(b, y) + t * t * axis(c, y),
      [a, b, c, d] => {
        u * u * u * axis(a, y)
          + 3.0 * u * u * t * axis(b, y)
          + 3.0 * u * t * t * axis(c, y)
          + t * t * t * axis(d, y)
      }
      _ => axis(&end, y),
    }
  };
  for y in [false, true] {
    let mut values = [0.0; 4];
    for (value, point) in values.iter_mut().zip(points) {
      *value = axis(point, y);
    }
    for t in turns(&values[..points.len()]).into_iter().flatten() {
      add(at(t, false), at(t, true));
    }
  }
  end
}

/// Where between 0 and 1 a quadratic or cubic Bézier with these values
/// on one axis has a zero derivative.
fn turns(values: &[f32]) -> [Option<f32>; 2] {
  let inside = |t: f32| (t > 0.0 && t < 1.0).then_some(t);
  // The derivative, over 2 or 3, is q t² + r t + s.
  let (q, r, s) = match *values {
    [a, b, c] => (0.0, a - 2.0 * b + c, b - a),
    [a, b, c, d] => (-a + 3.0 * b - 3.0 * c + d, 2.0 * (a - 2.0 * b + c), b - a),
    _ => return [None, None],
  };
  if q.abs() < 1e-6 {
    return [(r.abs() >= 1e-6).then(|| -s / r).and_then(inside), None];
  }
  let discriminant = r * r - 4.0 * q * s;
  if discriminant < 0.0 {
    return [None, None];
  }
  let root = discriminant.sqrt();
  [
    inside((-r + root) / (2.0 * q)),
    inside((-r - root) / (2.0 * q)),
  ]
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
  /// Each glyph's ink box from its origin, by its unhinted cache key at
  /// a whole pixel, or `None` if it draws nothing. At most
  /// [`INK_GLYPHS`]; a face ID can be reused once a font is added.
  pub(crate) ink: HashMap<CacheKey, Option<Rect>>,
  /// How many outlines and bitmaps ink boxes were worked out from.
  #[cfg(test)]
  pub(crate) glyphs_built: (usize, usize),
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
      ink: HashMap::new(),
      #[cfg(test)]
      glyphs_built: (0, 0),
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
    self.ink.clear();
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
      ink: self.ink(&node),
    }
  }

  /// The tight box around the ink of the glyphs `node` shows, from its
  /// top-left, or an empty rect at 0, 0 if they draw none.
  fn ink(&mut self, node: &TextNode) -> Rect {
    // Made only when a glyph isn't cached yet.
    let mut scaler = None;
    // Left, top, right and bottom.
    let mut ink = [
      f32::INFINITY,
      f32::INFINITY,
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
    ];
    for run in node.runs() {
      for glyph in run.glyphs.iter() {
        let (key, _, _) = CacheKey::new(
          glyph.font_id,
          glyph.glyph_id,
          glyph.font_size,
          (0.0, 0.0),
          glyph.font_weight,
          glyph.cache_key_flags | CacheKeyFlags::DISABLE_HINTING,
        );
        let found = match self.ink.get(&key) {
          Some(found) => *found,
          None => {
            let found = self.glyph_ink(scaler.get_or_insert_with(SwashCache::new), key);
            if self.ink.len() >= INK_GLYPHS {
              self.ink.clear();
            }
            self.ink.insert(key, found);
            found
          }
        };
        let Some(found) = found else {
          continue;
        };
        let x = glyph.x + glyph.font_size * glyph.x_offset;
        let y = run.line_y + glyph.y - glyph.font_size * glyph.y_offset;
        ink = [
          ink[0].min(x + found.x),
          ink[1].min(y + found.y),
          ink[2].max(x + found.right()),
          ink[3].max(y + found.bottom()),
        ];
      }
    }
    match ink[0] <= ink[2] {
      true => Rect::new(ink[0], ink[1], ink[2] - ink[0], ink[3] - ink[1]),
      false => Rect::default(),
    }
  }

  /// The ink box of the glyph `key` names, from its origin, y down: its
  /// outline's, or a bitmap's pixels' if it has no outline. `None` for a
  /// blank.
  fn glyph_ink(&mut self, scaler: &mut SwashCache, key: CacheKey) -> Option<Rect> {
    let Some(commands) = scaler.get_outline_commands_uncached(&mut self.fonts, key) else {
      #[cfg(test)]
      {
        self.glyphs_built.1 += 1;
      }
      let placement = scaler.get_image_uncached(&mut self.fonts, key)?.placement;
      let (width, height) = (placement.width as f32, placement.height as f32);
      return (width > 0.0 && height > 0.0)
        .then(|| Rect::new(placement.left as f32, -placement.top as f32, width, height));
    };
    #[cfg(test)]
    {
      self.glyphs_built.0 += 1;
    }
    let mut ink = [
      f32::INFINITY,
      f32::INFINITY,
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
    ];
    // Outlines rise up from the baseline.
    outline_extremes(&commands, |x, y| {
      ink = [ink[0].min(x), ink[1].min(-y), ink[2].max(x), ink[3].max(-y)];
    });
    (ink[0] <= ink[2]).then(|| Rect::new(ink[0], ink[1], ink[2] - ink[0], ink[3] - ink[1]))
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
  /// one, or 0 for plain text. cosmic-text shapes the ellipsis with the
  /// first span's attributes, metadata included, so a glyph counts as a
  /// span's only through [`glyph_span`](Self::glyph_span).
  pub(crate) spans: Vec<Span>,
  /// Where each span starts in `content`, in bytes.
  span_starts: Vec<usize>,
  /// Where each of the buffer's lines starts in `content`, in bytes.
  line_starts: Vec<usize>,
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
  /// What a click on a link span runs, from `Text::on_link`.
  pub(crate) on_link: Option<LinkHandler>,
  /// The link span the pointer is over, and the one the keyboard focus
  /// is on while the window has the keyboard. Both draw lit.
  pub(crate) hovered_link: Option<usize>,
  pub(crate) focused_link: Option<usize>,
  /// The color a lit link is drawn in, from `Text::link_hover_color`.
  pub(crate) link_hover_color: Option<Color>,
}

impl TextNode {
  pub(crate) fn new() -> TextNode {
    TextNode {
      content: String::new(),
      spans: Vec::new(),
      span_starts: Vec::new(),
      line_starts: Vec::new(),
      elide: Elide::None,
      max_lines: 1,
      wrap: true,
      buffer: {
        let mut buffer = Buffer::new_empty(metrics(&Font::DEFAULT.into()));
        // Four spaces, as most editors; cosmic-text's default is eight.
        buffer.set_tab_width(4);
        buffer
      },
      style: None,
      fonts_added: 0,
      stale: true,
      origin: Point::default(),
      selectable: false,
      cursor: 0,
      anchor: None,
      on_link: None,
      hovered_link: None,
      focused_link: None,
      link_hover_color: None,
    }
  }

  // Each setter returns whether the value changed, which needs a layout.

  pub(crate) fn set_content(&mut self, content: String) -> bool {
    self.set(|text| &mut text.content, content)
  }

  /// Sets a rich text's spans. Only a change of the text they join drops
  /// the selection; a change of style alone keeps it.
  pub(crate) fn set_spans(&mut self, spans: Vec<Span>) -> bool {
    let content = spans.iter().map(|span| span.text.as_str()).collect();
    self.span_starts = spans
      .iter()
      .scan(0, |start, span| {
        let this = *start;
        *start += span.text.len();
        Some(this)
      })
      .collect();
    let changed = self.set(|text| &mut text.spans, spans);
    let new_text = self.set_content(content);
    self.forget_selection_if(new_text) | changed
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

  pub(crate) fn wraps(&self) -> bool {
    self.max_lines > 1 && self.wrap
  }

  /// Whether the style the text was shaped with aligns it.
  pub(crate) fn aligned(&self) -> bool {
    self.style.is_some_and(|style| style.align.is_some())
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
    let missing = self.fallback_chars(&fonts, &families, text.font_system().db());
    let mut gained = false;
    for (font, chars) in missing {
      gained |= text.load_fallbacks(&font, &chars);
    }
    if gained {
      self.shape(&fonts, &families, &style, text);
    }
    self.line_starts = self
      .buffer
      .lines
      .iter()
      .scan(0, |start, line| {
        let this = *start;
        *start += line.text().len() + line.ending().as_str().len();
        Some(this)
      })
      .collect();
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

  /// Characters that need a fallback font, by the font they were asked
  /// in: those that came out as the missing-glyph box, and those drawn
  /// from a face outside that font's family. The second kind may have
  /// come from a fallback loaded for another weight or family, so
  /// fontconfig is asked again for this one. Spans differ only in weight
  /// and slant, so there are at most a few fonts, however many spans.
  fn fallback_chars(
    &self,
    fonts: &[Font],
    families: &[Option<Rc<str>>],
    db: &Database,
  ) -> Vec<(Font, Vec<char>)> {
    let in_family = |glyph: &LayoutGlyph| {
      let family = families.get(glyph.metadata).cloned().flatten();
      db.face(glyph.font_id).is_some_and(|face| {
        face
          .families
          .iter()
          .any(|(name, _)| Some(name.as_str()) == family.as_deref())
      })
    };
    let mut missing: Vec<(Font, Vec<char>)> = Vec::new();
    let mut seen = HashSet::new();
    for run in self.buffer.layout_runs() {
      let fallback = |glyph: &&LayoutGlyph| glyph.glyph_id == 0 || !in_family(glyph);
      for glyph in run.glyphs.iter().filter(fallback) {
        let font = fonts[glyph.metadata.min(fonts.len() - 1)];
        for ch in run.text.get(glyph.start..glyph.end).unwrap_or("").chars() {
          if !seen.insert((font.weight, font.italic, ch)) {
            continue;
          }
          match missing
            .iter_mut()
            .find(|(other, _)| (other.weight, other.italic) == (font.weight, font.italic))
          {
            Some((_, chars)) => chars.push(ch),
            None => missing.push((font, vec![ch])),
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
    let width = (self.fits_width() || self.aligned()).then_some(content.width);
    self.finish_at(content, width, fonts);
  }

  /// [`finish`](Self::finish), laying the lines out across `width`, or
  /// each as long as it is.
  pub(crate) fn finish_at(&mut self, content: Rect, width: Option<f32>, fonts: &mut FontSystem) {
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

  /// The index of the span `glyph`, on the buffer's line `line`, is part
  /// of: the one its metadata names, if all its bytes are in that span.
  /// The ellipsis holds no bytes, and a ligature across two spans isn't
  /// either's, so both take the text's own style.
  pub(crate) fn glyph_span(&self, line: usize, glyph: &LayoutGlyph) -> Option<usize> {
    let index = glyph.metadata.checked_sub(1)?;
    let from = *self.span_starts.get(index)?;
    let to = from + self.spans.get(index)?.text.len();
    let line_start = *self.line_starts.get(line)?;
    let (start, end) = (line_start + glyph.start, line_start + glyph.end);
    (start < end && from <= start && end <= to).then_some(index)
  }

  /// The index of the link span whose glyph box holds `point`, in the
  /// coordinates `origin` is in, if the text has an `on_link` that hasn't
  /// panicked. A panicked one leaves its links plain text, as a drop
  /// target whose `on_drop` panicked takes nothing more.
  pub(crate) fn link_at(&self, point: Point) -> Option<usize> {
    if !self.links_on() {
      return None;
    }
    let (x, y) = (point.x - self.origin.x, point.y - self.origin.y);
    let run = self
      .runs()
      .find(|run| y >= run.line_top && y < run.line_top + run.line_height)?;
    let glyph = run
      .glyphs
      .iter()
      .find(|glyph| x >= glyph.x && x < glyph.x + glyph.w)?;
    let index = self.glyph_span(run.line_i, glyph)?;
    self.spans[index].link.as_ref()?;
    Some(index)
  }

  /// Whether the text has an `on_link` that hasn't panicked, so its
  /// links are more than plain text.
  fn links_on(&self) -> bool {
    self
      .on_link
      .as_ref()
      .is_some_and(|handler| !handler.disabled())
  }

  /// Whether link span `index` draws lit: still a link, under the
  /// pointer or holding the focus.
  pub(crate) fn lit(&self, index: usize) -> bool {
    self.links_on()
      && (self.hovered_link == Some(index) || self.focused_link == Some(index))
      && self
        .spans
        .get(index)
        .is_some_and(|span| span.link.is_some())
  }

  /// The link spans Tab stops on, in text order: each with a glyph
  /// shown, while the text has an `on_link` that hasn't panicked.
  pub(crate) fn link_stops(&self) -> Vec<usize> {
    if !self.links_on() {
      return Vec::new();
    }
    let mut stops: Vec<usize> = self
      .runs()
      .flat_map(|run| {
        let spans = run.glyphs.iter();
        let spans = spans.filter_map(|glyph| self.glyph_span(run.line_i, glyph));
        spans.collect::<Vec<_>>()
      })
      .filter(|&index| self.spans[index].link.is_some())
      .collect();
    stops.sort_unstable();
    stops.dedup();
    stops
  }

  /// Where link span `index` may paint, a box for each line it is on, in
  /// the coordinates `origin` is in: its glyphs' boxes grown for ink as
  /// [`ink_box`](Self::ink_box) grows the lines.
  pub(crate) fn link_areas(&self, index: usize) -> Vec<Rect> {
    let em = self.buffer.metrics().font_size;
    self
      .runs()
      .filter_map(|run| {
        let mine = run
          .glyphs
          .iter()
          .filter(|glyph| self.glyph_span(run.line_i, glyph) == Some(index));
        let (start, end) = mine.fold(None, |span: Option<(f32, f32)>, glyph| {
          let (start, end) = span.unwrap_or((glyph.x, glyph.x + glyph.w));
          Some((start.min(glyph.x), end.max(glyph.x + glyph.w)))
        })?;
        Some(Rect::new(
          self.origin.x + start - em,
          self.origin.y + run.line_top - em / 2.0,
          end - start + 2.0 * em,
          run.line_height + em,
        ))
      })
      .collect()
  }

  /// Where an empty line sits across the width the text was laid out
  /// in, from 0 at the left to 1 at the right, as its alignment and
  /// direction (`rtl`) put a line that has glyphs. cosmic-text places
  /// every empty line at 0.
  pub(crate) fn empty_line_place(&self, rtl: bool) -> f32 {
    let start = if rtl { 1.0 } else { 0.0 };
    match self.style.and_then(|style| style.align) {
      Some(TextAlign::Center) => 0.5,
      Some(TextAlign::End) => 1.0 - start,
      _ => start,
    }
  }

  /// The width the text was laid out in: its box's, or its widest
  /// line's.
  pub(crate) fn layout_width(&self) -> f32 {
    self.buffer.size().0.unwrap_or_else(|| self.width())
  }

  /// The lines shown, at most `max_lines`. Everything that reads the
  /// lines goes through here, so drawing, hit testing, selection and
  /// links agree.
  pub(crate) fn runs(&self) -> impl Iterator<Item = TextRun<'_>> {
    self
      .buffer
      .layout_runs()
      .take(self.max_lines as usize)
      .map(|run| {
        // Without blanks dropped, the left edge stays where it is.
        let moved = 1.0 - self.empty_line_place(false);
        TextRun::shown(run, moved)
      })
  }

  pub(crate) fn width(&self) -> f32 {
    self.runs().map(|run| run.line_w).fold(0.0, f32::max)
  }

  fn height(&self) -> f32 {
    self.runs().map(|run| run.line_height).sum()
  }
}

/// A line of text as shown: cosmic-text's layout run, less the blank
/// that cosmic-text 0.19 starts an elided line with when the line before
/// it wrapped at a space.
#[derive(Clone, Debug)]
pub struct TextRun<'a> {
  /// The index of the buffer line it is part of.
  pub line_i: usize,
  /// That buffer line's text, which glyphs index into.
  pub text: &'a str,
  /// Whether the paragraph runs right to left.
  pub rtl: bool,
  /// The glyphs to draw, left to right, from the text's origin.
  pub glyphs: Cow<'a, [LayoutGlyph]>,
  /// From the text's top to the baseline.
  pub line_y: f32,
  /// From the text's top to the line's top.
  pub line_top: f32,
  /// The line's height.
  pub line_height: f32,
  /// The line's width.
  pub line_w: f32,
  /// How far left the glyphs were moved, and the first byte shown.
  pub(crate) shift: f32,
  pub(crate) first: usize,
}

impl<'a> TextRun<'a> {
  /// `run` with the blanks dropped that open an elided left-to-right line
  /// cut out of a longer one, the glyphs after them moved left by
  /// `moved` times what they took: 1 for a line placed from the left, a
  /// half for a centered one and 0 for one placed from the right.
  ///
  /// shortcut: works around cosmic-text 0.19's word wrap, which starts
  /// the elided line at the blank before the word that wrapped; drop it
  /// once an upgrade fixes that.
  fn shown(run: LayoutRun<'a>, moved: f32) -> TextRun<'a> {
    // The ellipsis holds no bytes; a line that starts inside its buffer
    // line was wrapped.
    let elided = run.glyphs.iter().any(|glyph| glyph.start == glyph.end);
    let blank = |glyph: &&LayoutGlyph| {
      glyph.start > 0
        && glyph.start < glyph.end
        && run
          .text
          .get(glyph.start..glyph.end)
          .is_some_and(|text| text.chars().all(char::is_whitespace))
    };
    let dropped = match elided && !run.rtl {
      true => run.glyphs.iter().take_while(blank).count(),
      false => 0,
    };
    let (glyphs, width, shift, first) = if dropped == 0 {
      (Cow::Borrowed(run.glyphs), 0.0, 0.0, 0)
    } else {
      let width: f32 = run.glyphs[..dropped].iter().map(|glyph| glyph.w).sum();
      let shift = width * moved;
      let glyphs = run.glyphs[dropped..].iter().map(|glyph| LayoutGlyph {
        x: glyph.x - shift,
        ..glyph.clone()
      });
      let first = run.glyphs.get(dropped).map_or(0, |glyph| glyph.start);
      (Cow::Owned(glyphs.collect()), width, shift, first)
    };
    TextRun {
      first,
      line_i: run.line_i,
      text: run.text,
      rtl: run.rtl,
      glyphs,
      line_y: run.line_y,
      line_top: run.line_top,
      line_height: run.line_height,
      line_w: run.line_w - width,
      shift,
    }
  }

  /// The line as a cosmic-text layout run, for its selection and caret
  /// helpers. Decoration spans aren't drawn, so it has none.
  pub(crate) fn layout(&self) -> LayoutRun<'_> {
    LayoutRun {
      line_i: self.line_i,
      text: self.text,
      rtl: self.rtl,
      glyphs: &self.glyphs,
      decorations: &[],
      line_y: self.line_y,
      line_top: self.line_top,
      line_height: self.line_height,
      line_w: self.line_w,
    }
  }
}
