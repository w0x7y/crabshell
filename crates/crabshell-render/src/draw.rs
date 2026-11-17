//! Drawing a window's view into a buffer.
//!
//! The areas to repaint are drawn from scratch, one tile of a fixed grid at
//! a time, into an RGBA pixmap of the tile's size, then copied into the
//! buffer. The pixmap clips the drawing to the tile, and the copy converts
//! the pixels to the buffer's byte order, so tiny-skia, glyph masks, resvg
//! and PNG decoding all work in the RGBA order they share.

use std::collections::HashMap;

use cosmic_text::{FontSystem, LayoutGlyph, SwashCache, SwashContent, SwashImage};
use crabshell_ui::{
  Affine, CaptureView, Color, Content, IconView, ImageView, InputView, NodeView, Radii, Rect,
  Shadow, Size, Style, TextSystem, TextView, WindowView,
};
use tiny_skia::{
  FillRule, FilterQuality, LineCap, LineJoin, Mask, Paint, PathBuilder, PathSegment, Pixmap,
  PixmapMut, PixmapPaint, Stroke, StrokeDash, Transform,
};

use crate::damage::{BufferAge, DamageHistory, PixelRect};
use crate::icons::{Drawn, Icons};

mod canvas;
mod effects;
mod gradient;
mod transition;

/// The side of a tile, in buffer pixels. tiny-skia cuts a curve where it
/// leaves the pixmap, which changes the antialiasing beside the cut, so
/// every pixel is drawn in the same tile whatever the damage, and a repaint
/// matches drawing everything.
const TILE: u32 = 64;

/// How many glyph images cosmic-text's cache may hold after a draw before
/// it is cleared. A bar needs a few hundred; the cache never shrinks itself.
const GLYPH_CACHE_LIMIT: usize = 2048;

/// The most pixels a transformed layer may hold, 16 MiB. Only a widget
/// shrunk to a sliver of its size, so that a tile shows much of it, comes
/// near; past this it isn't drawn.
const LAYER_LIMIT: u64 = 1 << 22;

/// Buffer pixels a transformed layer reaches past what it samples, so the
/// edges where tiny-skia cuts its curves stay out of the picture.
const LAYER_MARGIN: f32 = 2.0;

/// How a buffer stores each pixel, as `wl_shm` names the formats. Both are
/// 32 bits per pixel with premultiplied alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
  /// Bytes B, G, R, A in memory. Every compositor supports it.
  Argb8888,
  /// Bytes R, G, B, A in memory, as tiny-skia draws. Copied as is.
  Abgr8888,
}

/// A buffer to draw a window into.
pub struct Canvas<'a> {
  /// The buffer's memory: `width * height` pixels of 4 bytes, row by row.
  pub pixels: &'a mut [u8],
  /// The width in pixels: the window's logical width times `scale`,
  /// rounded halfway away from zero, as `wp_fractional_scale_v1` says.
  /// The window's edge is drawn at the buffer's edge either way.
  pub width: u32,
  /// The height in pixels, likewise.
  pub height: u32,
  /// Physical pixels per logical pixel.
  pub scale: f32,
  /// How the buffer stores pixels.
  pub format: PixelFormat,
  /// The frame the buffer last held, updated by the draw.
  pub age: &'a mut BufferAge,
}

/// Draws windows. One serves every window; it holds the glyph and icon
/// caches.
pub struct Renderer {
  glyphs: SwashCache,
  icons: Icons,
  /// Where each tile is drawn before it is copied into the buffer, kept
  /// between frames.
  scratch: Vec<u8>,
  work: Work,
}

/// What drawing has cost so far, for tests to check that a draw stays
/// small.
#[derive(Default)]
#[cfg_attr(not(test), allow(dead_code, reason = "only tests read it"))]
struct Work {
  /// Pixels of opacity, clip and transform layers allocated.
  layer_pixels: u64,
  /// Nodes drawn.
  nodes: u64,
  /// Pixels of alpha content shadows were blurred over.
  blurred: u64,
  /// Pixels gradients were worked out for. A `Cell`, since a ramp is
  /// worked out while painting reads the rest.
  gradient_pixels: std::cell::Cell<u64>,
}

impl Renderer {
  /// A renderer with empty caches, looking icons up in the `hicolor`
  /// theme.
  pub fn new() -> Renderer {
    Renderer {
      glyphs: SwashCache::new(),
      icons: Icons::new(),
      scratch: Vec::new(),
      work: Work::default(),
    }
  }

  /// The number of glyph images cached, for tests.
  #[cfg(test)]
  pub(crate) fn cached_glyphs(&self) -> usize {
    self.glyphs.image_cache.len()
  }

  /// The pixels of opacity layers allocated and the nodes drawn so far,
  /// for tests.
  #[cfg(test)]
  pub(crate) fn work(&self) -> (u64, u64, u64) {
    (self.work.layer_pixels, self.work.nodes, self.work.blurred)
  }

  /// The pixels gradients were worked out for so far, for tests.
  #[cfg(test)]
  pub(crate) fn gradient_pixels(&self) -> u64 {
    self.work.gradient_pixels.get()
  }

  /// A renderer that looks icon names up in `bases` instead of the
  /// environment's directories, for tests.
  #[cfg(test)]
  pub(crate) fn with_icon_dirs(bases: Vec<std::path::PathBuf>) -> Renderer {
    Renderer {
      icons: Icons::with_themes(crate::theme::Themes::new(bases)),
      ..Renderer::new()
    }
  }

  /// Sets the icon theme names are looked up in.
  pub fn set_icon_theme(&mut self, theme: impl Into<String>) {
    self.icons.set_theme(theme.into());
  }

  /// The number of rasterized icons cached, for tests.
  #[cfg(test)]
  pub(crate) fn cached_icons(&self) -> usize {
    self.icons.cached()
  }

  /// The number of times an icon was rasterized, for tests.
  #[cfg(test)]
  pub(crate) fn rasterized_icons(&self) -> usize {
    self.icons.rasterized()
  }

  /// Drops the scaled frames kept for screencopy views `gone`, at once:
  /// each is as large as its view, and an idle shell may draw nothing
  /// for a long while.
  pub fn forget_captures(&mut self, gone: &[u64]) {
    self.icons.forget_captures(gone);
  }

  /// When the next frame of an animated GIF is due, counting from `now`
  /// for one drawn since the last call. `None` while no GIF drawn plays.
  /// The platform wakes then and calls
  /// [`advance_images`](Self::advance_images).
  pub fn next_image_frame(&mut self, now: std::time::Instant) -> Option<std::time::Instant> {
    self.icons.next_frame(now)
  }

  /// Moves each GIF whose next frame is due by `now` on to it, and returns
  /// their image ids for the platform to repaint with
  /// [`Window::repaint_image`](crabshell_ui::Window::repaint_image). A
  /// GIF waits for its next frame only once that one is drawn, so one no
  /// longer shown wakes nothing more.
  pub fn advance_images(&mut self, now: std::time::Instant) -> Vec<u64> {
    self.icons.advance(now)
  }

  /// The number of screencopy frames kept scaled, for tests.
  #[cfg(test)]
  pub(crate) fn cached_captures(&self) -> usize {
    self.icons.captures()
  }

  /// Draws the laid-out window `view` into `canvas` and returns the areas
  /// to report with `wl_surface.damage_buffer`.
  ///
  /// `damage` is what the window reported since the last draw, from
  /// [`Window::take_damage`](crabshell_ui::Window::take_damage). The draw
  /// repaints that, plus what changed in frames this buffer missed
  /// according to `history`, or the whole buffer when that is more than
  /// half of it or the buffer is new. Text and glyphs come from `text`.
  ///
  /// Commit the buffer after every successful draw, in order: the history
  /// counts each one as a frame the compositor showed.
  ///
  /// # Errors
  ///
  /// If `canvas` holds the wrong number of bytes for its size, or its
  /// scale isn't a positive number. Nothing is drawn and the history
  /// doesn't advance.
  pub fn draw(
    &mut self,
    view: &WindowView<'_>,
    damage: &[Rect],
    text: &mut TextSystem,
    history: &mut DamageHistory,
    canvas: Canvas<'_>,
  ) -> Result<Vec<PixelRect>, CanvasError> {
    let Canvas {
      pixels,
      width,
      height,
      scale,
      format,
      age,
    } = canvas;
    if pixels.len() as u64 != u64::from(width) * u64::from(height) * 4
      || !(scale.is_finite() && scale > 0.0)
    {
      return Err(CanvasError {
        width,
        height,
        scale,
        bytes: pixels.len(),
      });
    }
    self.icons.start_frame(history.id());
    let plan = history.plan(damage, scale, width, height, age);
    let mut tiles: Vec<(u32, u32)> = Vec::new();
    for area in plan.repaint.iter().filter(|area| area.area() > 0) {
      for row in area.y / TILE..=(area.y + area.height - 1) / TILE {
        tiles
          .extend((area.x / TILE..=(area.x + area.width - 1) / TILE).map(|column| (row, column)));
      }
    }
    tiles.sort_unstable();
    tiles.dedup();
    let mut casts = HashMap::new();
    let mut fonts = text.font_system();
    for (row, column) in tiles {
      let (x, y) = (column * TILE, row * TILE);
      let size = (width.saturating_sub(x), height.saturating_sub(y));
      let area = PixelRect::new(x, y, TILE.min(size.0), TILE.min(size.1));
      self.scratch.clear();
      self.scratch.resize(area.area() as usize * 4, 0);
      let Some(mut pixmap) = PixmapMut::from_bytes(&mut self.scratch, area.width, area.height)
      else {
        continue;
      };
      let mut painter = Painter {
        grid: Grid {
          scale,
          window: view.size(),
          width,
          height,
          left: area.x as f32,
          top: area.y as f32,
        },
        glyphs: &mut self.glyphs,
        icons: &mut self.icons,
        fonts: &mut fonts,
        work: &mut self.work,
        casts: &mut casts,
      };
      painter.node(&mut pixmap, view.root());
      copy(&self.scratch, area, pixels, width, format);
    }
    self.icons.end_frame();
    if self.glyphs.image_cache.len() > GLYPH_CACHE_LIMIT {
      self.glyphs.image_cache.clear();
      self.glyphs.outline_command_cache.clear();
    }
    Ok(plan.report)
  }
}

/// A canvas whose bytes don't fit its size, or whose scale isn't a positive
/// number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasError {
  /// The canvas's width in pixels.
  pub width: u32,
  /// The canvas's height in pixels.
  pub height: u32,
  /// The canvas's scale.
  pub scale: f32,
  /// How many bytes the canvas held.
  pub bytes: usize,
}

impl std::fmt::Display for CanvasError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "a {}x{} canvas at scale {} needs {} bytes, but has {}",
      self.width,
      self.height,
      self.scale,
      u64::from(self.width) * u64::from(self.height) * 4,
      self.bytes
    )
  }
}

impl std::error::Error for CanvasError {}

impl Default for Renderer {
  fn default() -> Self {
    Self::new()
  }
}

/// Copies an area drawn in RGBA into the buffer, converting byte order.
fn copy(scratch: &[u8], area: PixelRect, pixels: &mut [u8], width: u32, format: PixelFormat) {
  let stride = width as usize * 4;
  let row_bytes = area.width as usize * 4;
  for (row, source) in scratch.chunks_exact(row_bytes).enumerate() {
    let start = (area.y as usize + row) * stride + area.x as usize * 4;
    let target = &mut pixels[start..start + row_bytes];
    match format {
      PixelFormat::Abgr8888 => target.copy_from_slice(source),
      PixelFormat::Argb8888 => {
        let (to, _) = target.as_chunks_mut::<4>();
        let (from, _) = source.as_chunks::<4>();
        for (to, from) in to.iter_mut().zip(from) {
          *to = [from[2], from[1], from[0], from[3]];
        }
      }
    }
  }
}

/// Turns logical coordinates into pixels of the area being drawn.
#[derive(Clone, Copy)]
struct Grid {
  scale: f32,
  /// The window's logical size.
  window: Size,
  /// The buffer's size in pixels.
  width: u32,
  height: u32,
  /// Where the area being drawn starts in the buffer.
  left: f32,
  top: f32,
}

impl Grid {
  /// A logical x coordinate on the pixel grid. The window's right edge is
  /// the buffer's, even where rounding would leave a column out.
  fn x(&self, x: f32) -> f32 {
    let pixel = if x >= self.window.width {
      self.width as f32
    } else {
      (x * self.scale).round()
    };
    pixel - self.left
  }

  /// A logical y coordinate on the pixel grid, likewise.
  fn y(&self, y: f32) -> f32 {
    let pixel = if y >= self.window.height {
      self.height as f32
    } else {
      (y * self.scale).round()
    };
    pixel - self.top
  }

  /// The pixels of a `width` by `height` area that `rect`, in logical
  /// pixels, touches: rounded outward, a pixel added around for
  /// antialiasing, and cut to the area. `None` if it misses the area.
  fn cover(&self, rect: Rect, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    if !(rect.width > 0.0 && rect.height > 0.0) {
      return None;
    }
    // `as` saturates, so huge or negative values stay in range.
    let left = ((rect.x * self.scale).floor() - self.left - 1.0).max(0.0) as u32;
    let top = ((rect.y * self.scale).floor() - self.top - 1.0).max(0.0) as u32;
    let right = (((rect.right() * self.scale).ceil() - self.left + 1.0).max(0.0) as u32).min(width);
    let bottom =
      (((rect.bottom() * self.scale).ceil() - self.top + 1.0).max(0.0) as u32).min(height);
    (left < right && top < bottom).then(|| (left, top, right - left, bottom - top))
  }

  /// This grid for a pixmap placed at `x`, `y` within this one.
  fn moved(&self, x: u32, y: u32) -> Grid {
    Grid {
      left: self.left + x as f32,
      top: self.top + y as f32,
      ..*self
    }
  }

  /// A logical rectangle on the pixel grid, or `None` if it covers no
  /// pixel.
  fn rect(&self, rect: Rect) -> Option<tiny_skia::Rect> {
    tiny_skia::Rect::from_ltrb(
      self.x(rect.x),
      self.y(rect.y),
      self.x(rect.right()),
      self.y(rect.bottom()),
    )
    .filter(|rect| rect.width() >= 1.0 && rect.height() >= 1.0)
  }
}

/// What drawing needs besides the pixmap.
struct Painter<'a> {
  grid: Grid,
  glyphs: &'a mut SwashCache,
  icons: &'a mut Icons,
  fonts: &'a mut FontSystem,
  work: &'a mut Work,
  /// The cells of content shadows cast in this draw.
  casts: &'a mut HashMap<CastKey, effects::Cast>,
}

/// A cell of a content shadow cast in a draw: its widget's path, and the
/// cell's column and row.
type CastKey = (String, i64, i64);

impl Painter<'_> {
  /// A painter for a pixmap on `grid`, in the same space.
  fn on(&mut self, grid: Grid) -> Painter<'_> {
    Painter {
      grid,
      glyphs: &mut *self.glyphs,
      icons: &mut *self.icons,
      fonts: &mut *self.fonts,
      work: &mut *self.work,
      casts: &mut *self.casts,
    }
  }

  /// Draws `node` and everything inside it. With an opacity below 1, a
  /// mask or a content shadow, they are drawn into one layer first, which
  /// is masked, given its shadow and blended in at that opacity, so
  /// overlapping children don't show through each other.
  fn node(&mut self, pixmap: &mut PixmapMut<'_>, node: NodeView<'_>) {
    // Nothing the node or its descendants paint reaches this area.
    let Some((x, y, width, height)) =
      self
        .grid
        .cover(node.bounds(), pixmap.width(), pixmap.height())
    else {
      return;
    };
    let style = node.style();
    let opacity = style.opacity.unwrap_or(1.0);
    let opacity = if opacity.is_nan() {
      1.0
    } else {
      opacity.clamp(0.0, 1.0)
    };
    if opacity == 0.0 {
      return;
    }
    let shadow = effects::cast_by_content(&style);
    if let Some(transform) = node.transform() {
      self.transformed(pixmap, node, &style, transform, opacity);
    } else if opacity < 1.0 || style.mask.is_some() || shadow.is_some() {
      // The layer covers only what the subtree paints in this area.
      let Some(mut layer) = Pixmap::new(width, height) else {
        return;
      };
      self.work.layer_pixels += u64::from(layer.width()) * u64::from(layer.height());
      let mut inner = self.on(self.grid.moved(x, y));
      inner.layer_contents(&mut layer.as_mut(), node, &style, shadow);
      let paint = PixmapPaint {
        opacity,
        ..PixmapPaint::default()
      };
      let (x, y) = (x as i32, y as i32);
      pixmap.draw_pixmap(x, y, layer.as_ref(), &paint, Transform::identity(), None);
    } else {
      self.node_contents(pixmap, node, &style);
    }
  }

  /// What `node_contents` draws into a fresh `layer`, kept only where the
  /// style's mask is opaque, with `shadow` cast by what is left put under
  /// it.
  fn layer_contents(
    &mut self,
    layer: &mut PixmapMut<'_>,
    node: NodeView<'_>,
    style: &Style,
    shadow: Option<Shadow>,
  ) {
    self.node_contents(layer, node, style);
    if let Some(mask) = &style.mask {
      self.mask(layer, node.rect(), mask);
    }
    if let Some(shadow) = shadow {
      self.content_shadow(layer, node, &shadow);
    }
  }

  /// Draws `node` and everything inside it into a layer in its own
  /// coordinates, then blends that in through its `transform`, filtered
  /// bilinearly, at `opacity`: one layer for both. The layer covers only
  /// what lands in this pixmap, in the node's own buffer pixels, so a
  /// pixel is always drawn the same way in the same tile.
  fn transformed(
    &mut self,
    pixmap: &mut PixmapMut<'_>,
    node: NodeView<'_>,
    style: &Style,
    transform: Affine,
    opacity: f32,
  ) {
    let Affine {
      sx,
      ky,
      kx,
      sy,
      tx,
      ty,
    } = transform;
    let Grid {
      scale, left, top, ..
    } = self.grid;
    // From the node's own buffer pixels to its parent's.
    let device = Transform::from_scale(scale, scale)
      .pre_concat(Transform::from_row(sx, ky, kx, sy, tx, ty))
      .pre_scale(1.0 / scale, 1.0 / scale);
    let to_pixmap = device.post_translate(-left, -top);
    let Some(back) = to_pixmap.invert() else {
      return;
    };
    let inner = node.inner_bounds();
    let shown = tiny_skia::Rect::from_xywh(0.0, 0.0, pixmap.width() as f32, pixmap.height() as f32)
      .and_then(|rect| rect.transform(back));
    let drawn = tiny_skia::Rect::from_xywh(
      inner.x * scale,
      inner.y * scale,
      inner.width * scale,
      inner.height * scale,
    );
    let Some(area) = shown
      .zip(drawn)
      .and_then(|(shown, drawn)| shown.intersect(&drawn))
      .and_then(|area| area.outset(LAYER_MARGIN, LAYER_MARGIN))
      .and_then(|area| area.round_out())
    else {
      return;
    };
    let pixels = u64::from(area.width()) * u64::from(area.height());
    if pixels > LAYER_LIMIT {
      tracing::debug!(
        "{} is transformed into a layer too large to draw",
        node.path()
      );
      return;
    }
    let Some(mut layer) = Pixmap::new(area.width(), area.height()) else {
      return;
    };
    self.work.layer_pixels += pixels;
    let (x, y) = (area.x() as f32, area.y() as f32);
    let mut inner = self.on(Grid {
      left: x,
      top: y,
      ..self.grid
    });
    let shadow = effects::cast_by_content(style);
    inner.layer_contents(&mut layer.as_mut(), node, style, shadow);
    let paint = PixmapPaint {
      opacity,
      quality: FilterQuality::Bilinear,
      ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(
      0,
      0,
      layer.as_ref(),
      &paint,
      to_pixmap.pre_translate(x, y),
      None,
    );
  }

  fn node_contents(&mut self, pixmap: &mut PixmapMut<'_>, node: NodeView<'_>, style: &Style) {
    self.work.nodes += 1;
    // A content shadow is cast by the layer this is drawn into, and an
    // inset one inside the box.
    let outside = |shadow: &Shadow| !shadow.follow_content && !shadow.inset;
    if let Some(shadow) = style.shadow.filter(outside) {
      self.shadow(pixmap, node.rect(), style, &shadow);
    }
    self.draw_box(pixmap, node.rect(), style);
    match node.content() {
      Content::Text(text) => {
        self.selection(pixmap, &text.selection(), text.color());
        self.text(pixmap, &text);
      }
      Content::Icon(icon) => self.icon(pixmap, &icon),
      Content::Image(image) => self.image(pixmap, &image),
      Content::Capture(capture) => self.capture(pixmap, &capture),
      Content::Input(input) => self.input(pixmap, &input),
      Content::Canvas(canvas) => self.canvas(pixmap, &canvas, style),
      Content::None => {}
    }
    if node.clips() {
      self.clipped_children(pixmap, node, style);
    } else {
      for child in node.children() {
        self.node(pixmap, child);
      }
    }
  }

  /// `node`'s children, drawn into a layer that is blended in through a
  /// mask of the inside of the node's border. The mask is drawn in the
  /// area's own pixels, on the same fixed grid as everything else, so a
  /// repaint cuts every edge exactly as a full draw does.
  fn clipped_children(&mut self, pixmap: &mut PixmapMut<'_>, node: NodeView<'_>, style: &Style) {
    let Some((area, corners)) = self.box_shape(node.rect(), style) else {
      return;
    };
    let width = self.border_width(style);
    let Some(inside) = tiny_skia::Rect::from_ltrb(
      area.left() + width,
      area.top() + width,
      area.right() - width,
      area.bottom() - width,
    ) else {
      return;
    };
    let corners = inset(corners, width);
    self.through_mask(pixmap, node.rect(), (inside, corners), |painter, layer| {
      for child in node.children() {
        painter.node(layer, child);
      }
      // A scroll view always clips; its bar is cut to its corners too.
      for (handle, bar) in node.scroll_bars() {
        painter.draw_box(layer, handle, &bar);
      }
    });
  }

  /// What `draw` paints within `cover`, a logical rectangle, blended in
  /// through a mask of `shape`, a pixel rectangle with rounded corners.
  fn through_mask(
    &mut self,
    pixmap: &mut PixmapMut<'_>,
    cover: Rect,
    (shape, corners): (tiny_skia::Rect, Corners),
    draw: impl FnOnce(&mut Painter<'_>, &mut PixmapMut<'_>),
  ) {
    if !visible(pixmap, shape) {
      return;
    }
    let (Some(mut mask), Some((x, y, layer_width, layer_height))) = (
      Mask::new(pixmap.width(), pixmap.height()),
      self.grid.cover(cover, pixmap.width(), pixmap.height()),
    ) else {
      return;
    };
    let Some(mut layer) = Pixmap::new(layer_width, layer_height) else {
      return;
    };
    self.work.layer_pixels += u64::from(layer_width) * u64::from(layer_height);
    let mut inner = self.on(self.grid.moved(x, y));
    draw(&mut inner, &mut layer.as_mut());
    let mut path = PathBuilder::new();
    rounded_rect(&mut path, shape, corners);
    let Some(path) = path.finish() else {
      return;
    };
    mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
    pixmap.draw_pixmap(
      x as i32,
      y as i32,
      layer.as_ref(),
      &PixmapPaint::default(),
      Transform::identity(),
      Some(&mask),
    );
  }

  /// A widget's rectangle on the pixel grid and each corner's radius in
  /// pixels, fitted to it as CSS does, so two on one side never overlap.
  /// `None` if it covers no pixel.
  fn box_shape(&self, rect: Rect, style: &Style) -> Option<(tiny_skia::Rect, Corners)> {
    let area = self.grid.rect(rect)?;
    let corners = style.radius.unwrap_or_default().corners();
    let corners = fit(corners.map(|radius| radius * self.grid.scale), area);
    Some((area, corners))
  }

  /// The border's width in pixels: as in CSS, rounded down to whole
  /// pixels, but at least one. 0 without a border.
  fn border_width(&self, style: &Style) -> f32 {
    let Some(border) = style.border else {
      return 0.0;
    };
    let width = (border.width * self.grid.scale).floor();
    if border.width > 0.0 {
      width.max(1.0)
    } else {
      width.max(0.0)
    }
  }

  /// The background, an inset shadow and the border of a widget's
  /// rectangle.
  fn draw_box(&self, pixmap: &mut PixmapMut<'_>, rect: Rect, style: &Style) {
    let inset_shadow = style.shadow.filter(|shadow| shadow.inset);
    if style.background.is_none()
      && style.gradient.is_none()
      && style.border.is_none()
      && inset_shadow.is_none()
    {
      return;
    }
    let Some((area, corners)) = self.box_shape(rect, style) else {
      return;
    };
    if !visible(pixmap, area) {
      return;
    }

    if let Some(color) = style.background {
      let mut path = PathBuilder::new();
      rounded_rect(&mut path, area, corners);
      fill(pixmap, path, color, FillRule::Winding);
    }
    if let Some(gradient) = style.gradient {
      let mut path = PathBuilder::new();
      rounded_rect(&mut path, area, corners);
      let size = (pixmap.width(), pixmap.height());
      if let (Some(path), Some(ramp)) = (path.finish(), self.ramp(&gradient, area, area, size)) {
        ramp.fill(pixmap, &path);
      }
    }
    if let Some(shadow) = inset_shadow {
      self.inset_shadow(pixmap, (area, corners), self.border_width(style), &shadow);
    }
    if let Some(border) = style.border {
      let width = self.border_width(style);
      let dashed = width >= 1.0
        && style.border_dash.is_some_and(|dash| {
          self.dashed_border(pixmap, (area, corners), width, border.color, dash)
        });

      if width >= 1.0 && !dashed {
        // A ring: the outer shape with the inner one cut out.
        let mut path = PathBuilder::new();
        rounded_rect(&mut path, area, corners);
        if let Some(inner) = tiny_skia::Rect::from_ltrb(
          area.left() + width,
          area.top() + width,
          area.right() - width,
          area.bottom() - width,
        ) {
          rounded_rect(&mut path, inner, inset(corners, width));
        }
        fill(pixmap, path, border.color, FillRule::EvenOdd);
      }
    }
  }

  /// A border `width` pixels wide inside `area`, dashed by `on` and `off`
  /// logical pixels along its middle line, so the dashes follow the
  /// corners. The line is laid out and dashed in buffer pixels and moved
  /// onto the pixmap, so tiles cut the dashes in the same places. `false`
  /// when the pattern or the box can't take dashes, to draw it solid.
  fn dashed_border(
    &self,
    pixmap: &mut PixmapMut<'_>,
    (area, corners): (tiny_skia::Rect, Corners),
    width: f32,
    color: Color,
    (on, off): (f32, f32),
  ) -> bool {
    let Grid {
      scale, left, top, ..
    } = self.grid;
    let half = width / 2.0;
    let Some(middle) = tiny_skia::Rect::from_ltrb(
      area.left() + left + half,
      area.top() + top + half,
      area.right() + left - half,
      area.bottom() + top - half,
    ) else {
      return false;
    };
    let corners = inset(corners, half);
    let (on, off) = (on * scale, off * scale);
    let mut all = PathBuilder::new();
    let square: Vec<usize> = (0..4).filter(|&index| corners[index] <= 0.0).collect();
    if square.is_empty() {
      // Each corner trades two radii of straight line for a quarter
      // circle.
      let rounding: f32 = corners
        .iter()
        .map(|radius| (2.0 - std::f32::consts::FRAC_PI_2) * radius)
        .sum();
      let perimeter = 2.0 * (middle.width() + middle.height()) - rounding;
      let mut path = PathBuilder::new();
      rounded_rect(&mut path, middle, corners);
      if !dash_into(&mut all, path, perimeter, on, off) {
        return false;
      }
    } else {
      // As browsers do, each run from one square corner to the next gets
      // its own whole number of dashes, centered on both corners.
      for (at, &from) in square.iter().enumerate() {
        let to = square[(at + 1) % square.len()];
        let steps = (to + 4 - from - 1) % 4 + 1;
        let [.., mut was] = corner(middle, corners, from);
        let mut path = PathBuilder::new();
        path.move_to(was.x, was.y);
        let mut length = 0.0;
        for index in (from + 1..=from + steps).map(|index| index % 4) {
          let [start, one, two, end] = corner(middle, corners, index);
          path.line_to(start.x, start.y);
          length += was.distance(start);
          if corners[index] > 0.0 {
            path.cubic_to(one.x, one.y, two.x, two.y, end.x, end.y);
            length += std::f32::consts::FRAC_PI_2 * corners[index];
          }
          was = end;
        }
        if !dash_into(&mut all, path, length, on, off) {
          return false;
        }
      }
      // The two half dashes meeting at a square corner end on its middle
      // line, so the outer quarter of the corner is filled by a piece
      // joined there with a miter. A dot is round there as anywhere.
      if on > 0.0 {
        // Each corner's incoming direction, clockwise from the top left.
        const IN: [(f32, f32); 4] = [(0.0, -1.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0)];
        for &index in &square {
          let [point, ..] = corner(middle, corners, index);
          let ((ax, ay), (bx, by)) = (IN[index], IN[(index + 1) % 4]);
          all.move_to(point.x - half * ax, point.y - half * ay);
          all.line_to(point.x, point.y);
          all.line_to(point.x + half * bx, point.y + half * by);
        }
      }
    }
    let Some(dashes) = all.finish() else {
      return true;
    };
    // Every tile the border crosses would stroke every dash, so only the
    // dashes near this one are kept. A closed outline that misses the
    // tile adds nothing inside it, so the pixels are the same. A miter
    // reaches at most twice the width past its line.
    let reach = 2.0 * width + 1.0;
    let near = [
      left - reach,
      top - reach,
      left + pixmap.width() as f32 + reach,
      top + pixmap.height() as f32 + reach,
    ];
    let mut kept = PathBuilder::new();
    let mut contour = Vec::new();
    for segment in dashes.segments() {
      if matches!(segment, PathSegment::MoveTo(_)) {
        keep_near(&mut kept, &contour, near);
        contour.clear();
      }
      contour.push(segment);
    }
    keep_near(&mut kept, &contour, near);
    let Some(kept) = kept.finish() else {
      return true;
    };
    let stroke = Stroke {
      width,
      // A dash of 0 is a dot.
      line_cap: if on == 0.0 {
        LineCap::Round
      } else {
        LineCap::Butt
      },
      line_join: LineJoin::Miter,
      ..Stroke::default()
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    let moved = Transform::from_translate(-left, -top);
    pixmap.stroke_path(&kept, &paint, &stroke, moved, None);
    true
  }

  /// `shadow` of the box `rect` with `style`'s corners. A sharp one is a
  /// filled shape; a blurred one is worked out pixel by pixel in buffer
  /// pixels, so every tile agrees, and only within one and a half blurs
  /// of its shape, past which it fades below a 255th.
  fn shadow(&self, pixmap: &mut PixmapMut<'_>, rect: Rect, style: &Style, shadow: &Shadow) {
    let Grid {
      scale, left, top, ..
    } = self.grid;
    let numbers = [shadow.x, shadow.y, shadow.blur, shadow.spread];
    if shadow.color.a == 0 || !numbers.iter().all(|n| n.is_finite()) {
      return;
    }
    let Some((area, corners)) = self.box_shape(rect, style) else {
      return;
    };
    // The shape in buffer pixels.
    let spread = shadow.spread * scale;
    let Some(shape) = tiny_skia::Rect::from_ltrb(
      area.left() + left + shadow.x * scale - spread,
      area.top() + top + shadow.y * scale - spread,
      area.right() + left + shadow.x * scale + spread,
      area.bottom() + top + shadow.y * scale + spread,
    ) else {
      return;
    };
    let corners = fit(spread_corners(corners, spread), shape);
    // A cut-out shadow leaves clear what the box's background covers.
    let cut = if shadow.cut_out {
      self.box_mask(pixmap, rect, style)
    } else {
      None
    };
    // CSS draws a blur as a Gaussian of half its length.
    let sigma = shadow.blur.max(0.0) * scale / 2.0;
    if sigma < 0.25 {
      let mut path = PathBuilder::new();
      let Some(shape) = shape.transform(Transform::from_translate(-left, -top)) else {
        return;
      };
      rounded_rect(&mut path, shape, corners);
      let Some(path) = path.finish() else {
        return;
      };
      let mut paint = Paint::default();
      let color = shadow.color;
      paint.set_color_rgba8(color.r, color.g, color.b, color.a);
      paint.anti_alias = true;
      let outside = cut.map(|mut cut| {
        cut.invert();
        cut
      });
      let (rule, at) = (FillRule::Winding, Transform::identity());
      pixmap.fill_path(&path, &paint, rule, at, outside.as_ref());
      return;
    }
    let reach = 3.0 * sigma;
    let columns = (shape.left() - reach - left).floor().max(0.0) as u32
      ..((shape.right() + reach - left).ceil().max(0.0) as u32).min(pixmap.width());
    let rows = (shape.top() - reach - top).floor().max(0.0) as u32
      ..((shape.bottom() + reach - top).ceil().max(0.0) as u32).min(pixmap.height());
    if columns.is_empty() || rows.is_empty() {
      return;
    }
    let blurred = BlurredBox::new(shape, corners, sigma);
    let mut mask = Vec::with_capacity(columns.len() * rows.len());
    for row in rows.clone() {
      let xs = columns.clone().map(|column| left + column as f32 + 0.5);
      blurred.row(top + row as f32 + 0.5, xs, &mut mask);
    }
    if let Some(cut) = cut {
      let stride = pixmap.width() as usize;
      let under = rows.clone().flat_map(|row| {
        columns
          .clone()
          .map(move |column| row as usize * stride + column as usize)
      });
      for (alpha, at) in mask.iter_mut().zip(under) {
        *alpha = mul(*alpha, 255 - cut.data()[at]);
      }
    }
    let width = columns.len() as u32;
    let height = rows.len() as u32;
    let (x, y) = (columns.start as i32, rows.start as i32);
    blend_mask(pixmap, x, y, width, height, &mask, shadow.color);
  }

  /// An inset `shadow` inside `area` with `corners`, within a border
  /// `border` pixels wide: the inside of the border, but for a hole of its
  /// shape moved, shrunk by the spread and blurred, as CSS draws one. Like
  /// an outer shadow, it is worked out in buffer pixels, so tiles agree.
  fn inset_shadow(
    &self,
    pixmap: &mut PixmapMut<'_>,
    (area, corners): (tiny_skia::Rect, Corners),
    border: f32,
    shadow: &Shadow,
  ) {
    let Grid {
      scale, left, top, ..
    } = self.grid;
    let numbers = [shadow.x, shadow.y, shadow.blur, shadow.spread];
    if shadow.color.a == 0 || !numbers.iter().all(|n| n.is_finite()) {
      return;
    }
    let Some(inside) = tiny_skia::Rect::from_ltrb(
      area.left() + border,
      area.top() + border,
      area.right() - border,
      area.bottom() - border,
    ) else {
      return;
    };
    let inside_corners = inset(corners, border);
    if !visible(pixmap, inside) {
      return;
    }
    let mut path = PathBuilder::new();
    rounded_rect(&mut path, inside, inside_corners);
    let Some(path) = path.finish() else {
      return;
    };
    // The hole in buffer pixels; a spread that swallows it leaves none.
    let (x, y, spread) = (shadow.x * scale, shadow.y * scale, shadow.spread * scale);
    let hole = tiny_skia::Rect::from_ltrb(
      inside.left() + left + x + spread,
      inside.top() + top + y + spread,
      inside.right() + left + x - spread,
      inside.bottom() + top + y - spread,
    );
    let hole = hole.map(|hole| (hole, fit(spread_corners(inside_corners, -spread), hole)));
    let sigma = shadow.blur.max(0.0) * scale / 2.0;
    let color = shadow.color;
    if sigma < 0.25 {
      let mut outside = None;
      if let Some((hole, corners)) = hole {
        let mut cut = PathBuilder::new();
        let Some(moved) = hole.transform(Transform::from_translate(-left, -top)) else {
          return;
        };
        rounded_rect(&mut cut, moved, corners);
        let Some(mut mask) = Mask::new(pixmap.width(), pixmap.height()) else {
          return;
        };
        let (rule, at) = (FillRule::Winding, Transform::identity());
        let Some(cut) = cut.finish() else {
          return;
        };
        mask.fill_path(&cut, rule, true, at);
        mask.invert();
        outside = Some(mask);
      }
      let mut paint = Paint::default();
      paint.set_color_rgba8(color.r, color.g, color.b, color.a);
      paint.anti_alias = true;
      let (rule, at) = (FillRule::Winding, Transform::identity());
      pixmap.fill_path(&path, &paint, rule, at, outside.as_ref());
      return;
    }
    // The inside of the border covers these pixels, and the shadow shows
    // where the blurred hole doesn't.
    let Some(mut mask) = Mask::new(pixmap.width(), pixmap.height()) else {
      return;
    };
    mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
    let columns = inside.left().floor().max(0.0) as u32
      ..(inside.right().ceil().max(0.0) as u32).min(pixmap.width());
    let rows = inside.top().floor().max(0.0) as u32
      ..(inside.bottom().ceil().max(0.0) as u32).min(pixmap.height());
    if columns.is_empty() || rows.is_empty() {
      return;
    }
    let blurred = hole.map(|(hole, corners)| BlurredBox::new(hole, corners, sigma));
    let mut alpha = Vec::with_capacity(columns.len() * rows.len());
    for row in rows.clone() {
      let start = alpha.len();
      match &blurred {
        Some(blurred) => {
          let xs = columns.clone().map(|column| left + column as f32 + 0.5);
          blurred.row(top + row as f32 + 0.5, xs, &mut alpha);
        }
        None => alpha.resize(start + columns.len(), 0),
      }
      let stride = pixmap.width() as usize;
      let row = &mask.data()[row as usize * stride..];
      let covered = &row[columns.start as usize..columns.end as usize];
      for (value, &covered) in alpha[start..].iter_mut().zip(covered) {
        *value = mul(255 - *value, covered);
      }
    }
    let (width, height) = (columns.len() as u32, rows.len() as u32);
    let (x, y) = (columns.start as i32, rows.start as i32);
    blend_mask(pixmap, x, y, width, height, &alpha, color);
  }

  /// How much of each of this pixmap's pixels the box `rect` with
  /// `style`'s corners covers, as its background fills it.
  fn box_mask(&self, pixmap: &PixmapMut<'_>, rect: Rect, style: &Style) -> Option<Mask> {
    let (area, corners) = self.box_shape(rect, style)?;
    let mut path = PathBuilder::new();
    rounded_rect(&mut path, area, corners);
    let mut mask = Mask::new(pixmap.width(), pixmap.height())?;
    let (rule, at) = (FillRule::Winding, Transform::identity());
    mask.fill_path(&path.finish()?, rule, true, at);
    Some(mask)
  }

  /// Glyphs, drawn as masks in their span's color or else the text's,
  /// then any underline and strikethrough. The baseline sits on a whole
  /// pixel so horizontal strokes stay sharp; glyphs keep their sub-pixel
  /// horizontal position.
  fn text(&mut self, pixmap: &mut PixmapMut<'_>, text: &TextView<'_>) {
    let Grid {
      scale, left, top, ..
    } = self.grid;
    let origin = text.origin();
    let x = origin.x * scale - left;
    for run in text.runs() {
      // Rounded in buffer coordinates, so every area drawn agrees.
      let baseline = ((origin.y + run.line_y) * scale).round() - top;
      if baseline + run.line_height * scale < 0.0
        || baseline - run.line_height * scale > pixmap.height() as f32
      {
        continue;
      }
      for glyph in run.glyphs.iter() {
        let span = text.span(run.line_i, glyph);
        let color = span.and_then(|span| span.color).unwrap_or(text.color());
        if color.a == 0 {
          continue;
        }
        let physical = glyph.physical((x, baseline), scale);
        let Some(image) = self.glyphs.get_image(self.fonts, physical.cache_key) else {
          continue;
        };
        let left = physical.x + image.placement.left;
        let top = physical.y - image.placement.top;
        let (width, height) = (image.placement.width, image.placement.height);
        match image.content {
          SwashContent::Mask => {
            blend_mask(pixmap, left, top, width, height, &image.data, color);
          }
          SwashContent::Color => {
            let premultiplied = is_color_outline(image);
            let rgba = (image.data.as_slice(), premultiplied);
            blend_rgba(pixmap, left, top, width, height, rgba, color.a);
          }
          // Not requested: there is no subpixel antialiasing.
          SwashContent::SubpixelMask => {}
        }
      }
      let origin = text.origin().x;
      let own = (text.underline(), text.strikethrough());
      if own != (false, false) {
        self.decorations(pixmap, origin, &run.glyphs, baseline, own, text.color());
      }
      // A span's own lines run across its glyphs on the line, each in its
      // color. cosmic-text's decoration spans join two decorated spans
      // across the text between them, so they aren't used.
      let same_span = |a: &LayoutGlyph, b: &LayoutGlyph| match (
        text.span(run.line_i, a),
        text.span(run.line_i, b),
      ) {
        (Some(a), Some(b)) => std::ptr::eq(a, b),
        (a, b) => a.is_none() && b.is_none(),
      };
      for glyphs in run.glyphs.chunk_by(same_span) {
        let Some(span) = text.span(run.line_i, &glyphs[0]) else {
          continue;
        };
        let lines = (span.underline, span.strikethrough);
        if lines != (false, false) {
          let color = span.color.unwrap_or(text.color());
          self.decorations(pixmap, origin, glyphs, baseline, lines, color);
        }
      }
    }
  }

  /// The underline and strikethrough across `glyphs`, at the place and
  /// thickness the first glyph's font gives, as cosmic-text draws them.
  /// Edges snap to whole pixels in the buffer, so every tile agrees, and
  /// each line is at least a pixel thick. A line is kept within half an
  /// em of the baseline, which the text's painted area covers.
  fn decorations(
    &mut self,
    pixmap: &mut PixmapMut<'_>,
    origin: f32,
    glyphs: &[LayoutGlyph],
    baseline: f32,
    (underline, strikethrough): (bool, bool),
    color: Color,
  ) {
    let Some(first) = glyphs.first() else {
      return;
    };
    let Some(font) = self.fonts.get_font(first.font_id, first.font_weight) else {
      return;
    };
    let metrics = font.metrics();
    let em = f32::from(metrics.units_per_em);
    if em == 0.0 {
      return;
    }
    let Grid { scale, left, .. } = self.grid;
    let (start, end) = glyphs
      .iter()
      .fold((f32::MAX, f32::MIN), |(start, end), glyph| {
        (start.min(glyph.x), end.max(glyph.x + glyph.w))
      });
    let x0 = ((origin + start) * scale).round() - left;
    let x1 = ((origin + end) * scale).round() - left;
    let size = first.font_size * scale;
    // cosmic-text's defaults for a font that gives none.
    let lines = [
      (underline, metrics.underline, -0.125),
      (strikethrough, metrics.strikeout, 0.3),
    ];
    for (on, decoration, offset) in lines {
      if !on {
        continue;
      }
      let (offset, thickness) =
        decoration.map_or((offset, 1.0 / 14.0), |d| (d.offset / em, d.thickness / em));
      // The offset is to the line's top, upward from the baseline.
      let thickness = (thickness.clamp(0.0, 0.25) * size).round().max(1.0);
      let y = baseline - (offset.clamp(-0.25, 0.5) * size).round();
      if let Some(area) = tiny_skia::Rect::from_ltrb(x0, y, x1, y + thickness) {
        let mut path = PathBuilder::new();
        path.push_rect(area);
        fill(pixmap, path, color, FillRule::Winding);
      }
    }
  }

  /// A text field, cut to its content box.
  fn input(&mut self, pixmap: &mut PixmapMut<'_>, input: &InputView<'_>) {
    let Some(area) = self.grid.rect(input.area) else {
      return;
    };
    self.through_mask(pixmap, input.area, (area, [0.0; 4]), |painter, layer| {
      painter.input_contents(layer, input);
    });
  }

  /// A field's selection in a third of the text's color, the text, and a
  /// caret in the text's color.
  fn input_contents(&mut self, pixmap: &mut PixmapMut<'_>, input: &InputView<'_>) {
    let color = input.text.color();
    self.selection(pixmap, input.selection, color);
    self.text(pixmap, &input.text);
    for line in input.underline {
      if let Some(area) = self.grid.rect(*line) {
        let mut path = PathBuilder::new();
        path.push_rect(area);
        fill(pixmap, path, color, FillRule::Winding);
      }
    }
    if let Some(caret) = input.caret {
      // At least one pixel wide whatever the scale.
      let left = self.grid.x(caret.x);
      let right = self.grid.x(caret.right()).max(left + 1.0);
      if let Some(area) = tiny_skia::Rect::from_ltrb(
        left,
        self.grid.y(caret.y),
        right,
        self.grid.y(caret.bottom()),
      ) {
        let mut path = PathBuilder::new();
        path.push_rect(area);
        fill(pixmap, path, color, FillRule::Winding);
      }
    }
  }

  /// A selection, behind text in `color`, in a third of it.
  fn selection(&mut self, pixmap: &mut PixmapMut<'_>, spans: &[Rect], color: Color) {
    let tint = Color::rgba(color.r, color.g, color.b, color.a / 3);
    for span in spans {
      if let Some(area) = self.grid.rect(*span) {
        let mut path = PathBuilder::new();
        path.push_rect(area);
        fill(pixmap, path, tint, FillRule::Winding);
      }
    }
  }

  /// An image, scaled to its box in pixels.
  fn image(&mut self, pixmap: &mut PixmapMut<'_>, image: &ImageView<'_>) {
    let Some((x, y, width, height)) = self.picture_area(pixmap, image.rect) else {
      return;
    };
    match self.icons.image(image, (width, height), self.grid.scale) {
      Some(Drawn::One(pixels)) => transition::blit(pixmap, (x, y), pixels),
      Some(Drawn::Two { from, to, t }) => {
        transition::draw(pixmap, (x, y), from, to, t, image.transition);
      }
      None => {}
    }
  }

  /// A screencopy view's frame, scaled to its box in pixels.
  fn capture(&mut self, pixmap: &mut PixmapMut<'_>, capture: &CaptureView<'_>) {
    let Some((x, y, width, height)) = self.picture_area(pixmap, capture.rect) else {
      return;
    };
    let frame = capture.frame;
    if let Some(pixels) = self
      .icons
      .capture(capture.id, frame, width, height, capture.fit)
    {
      draw_at(pixmap, x, y, pixels);
    }
  }

  /// Where a picture in `rect` goes in the pixmap, and its size in
  /// pixels; `None` if it covers no pixel of it. Placed and sized apart,
  /// as an icon is, so a picture past the window's edge is cut there
  /// instead of squeezed inside it.
  fn picture_area(&self, pixmap: &PixmapMut<'_>, rect: Rect) -> Option<(i32, i32, u32, u32)> {
    let Grid {
      scale, left, top, ..
    } = self.grid;
    let (width, height) = ((rect.width * scale).round(), (rect.height * scale).round());
    let (x, y) = (
      (rect.x * scale).round() - left,
      (rect.y * scale).round() - top,
    );
    if !(width >= 1.0 && height >= 1.0) {
      return None;
    }
    let area = tiny_skia::Rect::from_xywh(x, y, width, height)?;
    // `as` saturates, so a huge box stays in range.
    visible(pixmap, area).then_some((x as i32, y as i32, width as u32, height as u32))
  }

  /// An icon, rasterized at its size in pixels.
  fn icon(&mut self, pixmap: &mut PixmapMut<'_>, icon: &IconView<'_>) {
    let Grid {
      scale, left, top, ..
    } = self.grid;
    let side = (icon.rect.width * scale).round();
    if side.is_nan() || side < 1.0 {
      return;
    }
    let x = (icon.rect.x * scale).round() - left;
    let y = (icon.rect.y * scale).round() - top;
    // Theme directories come in whole scales; 1.5 looks for @2 icons.
    let size = icon.rect.width.round() as u32;
    let theme_scale = scale.ceil().max(1.0) as u32;
    let sizes = (side as u32, size, theme_scale);
    if let Some(image) = self.icons.get(icon.source, sizes, icon.tint) {
      pixmap.draw_pixmap(
        x as i32,
        y as i32,
        image.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
      );
    }
  }
}

/// Draws `pixels` unscaled with its corner at `x`, `y`.
fn draw_at(pixmap: &mut PixmapMut<'_>, x: i32, y: i32, pixels: &Pixmap) {
  let paint = PixmapPaint::default();
  pixmap.draw_pixmap(x, y, pixels.as_ref(), &paint, Transform::identity(), None);
}

/// Whether any of `area` lies inside the pixmap.
fn visible(pixmap: &PixmapMut<'_>, area: tiny_skia::Rect) -> bool {
  area.right() > 0.0
    && area.bottom() > 0.0
    && area.left() < pixmap.width() as f32
    && area.top() < pixmap.height() as f32
}

fn fill(pixmap: &mut PixmapMut<'_>, path: PathBuilder, color: Color, rule: FillRule) {
  let Some(path) = path.finish() else {
    return;
  };
  let mut paint = Paint::default();
  paint.set_color_rgba8(color.r, color.g, color.b, color.a);
  paint.anti_alias = true;
  pixmap.fill_path(&path, &paint, rule, Transform::identity(), None);
}

/// Each corner's radius in pixels, from the top left clockwise.
type Corners = [f32; 4];

/// Dashes `path`, `length` pixels long, by `on` and `off` stretched so a
/// whole number of them fits, each centered on a multiple of the period
/// from its start, and adds them to `all`. `false` when the pattern can't
/// be drawn.
fn dash_into(all: &mut PathBuilder, path: PathBuilder, length: f32, on: f32, off: f32) -> bool {
  let Some((on, off)) = fit_dash(length, on, off) else {
    return false;
  };
  let (Some(path), Some(dash)) = (path.finish(), StrokeDash::new(vec![on, off], on / 2.0)) else {
    return false;
  };
  if let Some(dashes) = path.dash(&dash, 1.0) {
    all.push_path(&dashes);
  }
  true
}

/// Where corner `index` of `rect`, from the top left clockwise, starts
/// and ends going clockwise, with its quarter circle's control points
/// between: `[start, one, two, end]`, all one point for a square corner.
fn corner(rect: tiny_skia::Rect, corners: Corners, index: usize) -> [tiny_skia::Point; 4] {
  // How far a cubic curve's control points sit along the tangents to
  // follow a quarter circle closely.
  const KAPPA: f32 = 0.552_284_8;
  let (l, t, r, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
  let radius = corners[index];
  let k = radius * (1.0 - KAPPA);
  let points = match index {
    0 => [(l, t + radius), (l, t + k), (l + k, t), (l + radius, t)],
    1 => [(r - radius, t), (r - k, t), (r, t + k), (r, t + radius)],
    2 => [(r, b - radius), (r, b - k), (r - k, b), (r - radius, b)],
    _ => [(l + radius, b), (l + k, b), (l, b - k), (l, b - radius)],
  };
  points.map(|(x, y)| tiny_skia::Point::from_xy(x, y))
}

/// `corners` for a shadow's shape grown by `spread` pixels, or shrunk by
/// a negative one. As in CSS, a corner smaller than the spread grows by
/// less, so a square box keeps square corners.
fn spread_corners(corners: Corners, spread: f32) -> Corners {
  corners.map(|radius| {
    if spread > 0.0 && radius < spread {
      radius + spread * (1.0 + (radius / spread - 1.0).powi(3))
    } else {
      (radius + spread).max(0.0)
    }
  })
}

/// `corners` fitted to `area` as CSS fits them ([`Radii::fit`]).
fn fit(
  [top_left, top_right, bottom_right, bottom_left]: Corners,
  area: tiny_skia::Rect,
) -> Corners {
  let radii = Radii {
    top_left,
    top_right,
    bottom_right,
    bottom_left,
  };
  radii.fit(area.width(), area.height()).corners()
}

/// `corners` for a shape `by` pixels inside the edge.
fn inset(corners: Corners, by: f32) -> Corners {
  corners.map(|radius| (radius - by).max(0.0))
}

/// Adds a rectangle with circular `corners` to `path`, clockwise from
/// the end of its top-left one.
fn rounded_rect(path: &mut PathBuilder, rect: tiny_skia::Rect, corners: Corners) {
  if corners.iter().all(|&radius| radius <= 0.0) {
    path.push_rect(rect);
    return;
  }
  let [.., start] = corner(rect, corners, 0);
  path.move_to(start.x, start.y);
  for index in [1, 2, 3, 0] {
    let [start, one, two, end] = corner(rect, corners, index);
    path.line_to(start.x, start.y);
    if corners[index] > 0.0 {
      path.cubic_to(one.x, one.y, two.x, two.y, end.x, end.y);
    }
  }
  path.close();
}

/// `a * b / 255`, rounded.
fn mul(a: u8, b: u8) -> u8 {
  ((u16::from(a) * u16::from(b) + 127) / 255) as u8
}

/// Blends a premultiplied `source` over the pixel at `index` (in bytes).
fn over(data: &mut [u8], index: usize, source: [u8; 4]) {
  let rest = 255 - source[3];
  for (channel, value) in data[index..index + 4].iter_mut().zip(source) {
    *channel = value.saturating_add(mul(*channel, rest));
  }
}

/// The part of a `width` by `height` image placed at `left`, `top` that
/// lies inside the pixmap: the first and last column and row, as image
/// coordinates.
fn overlap(
  pixmap: &PixmapMut<'_>,
  left: i32,
  top: i32,
  width: u32,
  height: u32,
) -> Option<(usize, usize, usize, usize)> {
  let column_start = (-i64::from(left)).max(0);
  let row_start = (-i64::from(top)).max(0);
  let column_end = i64::from(width).min(i64::from(pixmap.width()) - i64::from(left));
  let row_end = i64::from(height).min(i64::from(pixmap.height()) - i64::from(top));
  (column_start < column_end && row_start < row_end).then_some((
    column_start as usize,
    column_end as usize,
    row_start as usize,
    row_end as usize,
  ))
}

/// Blends a glyph's coverage mask in `color`.
fn blend_mask(
  pixmap: &mut PixmapMut<'_>,
  left: i32,
  top: i32,
  width: u32,
  height: u32,
  mask: &[u8],
  color: Color,
) {
  if mask.len() < width as usize * height as usize {
    return;
  }
  let Some((columns, columns_end, rows, rows_end)) = overlap(pixmap, left, top, width, height)
  else {
    return;
  };
  let stride = pixmap.width() as usize;
  let data = pixmap.data_mut();
  for row in rows..rows_end {
    for column in columns..columns_end {
      let coverage = mask[row * width as usize + column];
      if coverage == 0 {
        continue;
      }
      let alpha = mul(color.a, coverage);
      let source = [
        mul(color.r, alpha),
        mul(color.g, alpha),
        mul(color.b, alpha),
        alpha,
      ];
      let x = (i64::from(left) + column as i64) as usize;
      let y = (i64::from(top) + row as i64) as usize;
      over(data, (y * stride + x) * 4, source);
    }
  }
}

/// How many rows a point near a corner sums.
const SAMPLES: usize = 8;

/// A rectangle with rounded corners blurred by a Gaussian: how much of it
/// covers each point. Away from the corners it is a plain rectangle,
/// whose blur is the product of each axis's, from the error function.
/// Near a corner, each row's span is blurred across and the rows are
/// summed over eight samples of the Gaussian down. Each corner has its
/// own radius; the largest says where the corners' rows begin.
struct BlurredBox {
  middle: (f32, f32),
  half: (f32, f32),
  corners: Corners,
  largest: f32,
  sigma: f32,
  /// What the samples sum to where the shape covers them all, a little
  /// under 1, so a point the shape covers whole is covered whole.
  whole: f32,
}

impl BlurredBox {
  fn new(shape: tiny_skia::Rect, corners: Corners, sigma: f32) -> BlurredBox {
    let step = 6.0 * sigma / SAMPLES as f32;
    let whole = (0..SAMPLES)
      .map(|sample| gaussian(step * (sample as f32 + 0.5) - 3.0 * sigma, sigma) * step)
      .sum();
    BlurredBox {
      whole,
      middle: (
        (shape.left() + shape.right()) / 2.0,
        (shape.top() + shape.bottom()) / 2.0,
      ),
      half: (shape.width() / 2.0, shape.height() / 2.0),
      corners,
      largest: corners.into_iter().fold(0.0, f32::max),
      sigma,
    }
  }

  /// Pushes the coverage at each of `xs` on the row at `y`, out of 255,
  /// onto `mask`.
  fn row(&self, y: f32, xs: impl Iterator<Item = f32>, mask: &mut Vec<u8>) {
    let y = y - self.middle.1;
    let (half_width, half_height) = self.half;
    let reach = 3.0 * self.sigma;
    let down = self.along(y, half_height);
    let clear_of_corners = y.abs() <= half_height - self.largest - reach;
    let rows = if clear_of_corners {
      [(0.0, 0.0, 0.0); SAMPLES]
    } else {
      self.rows_near(y)
    };
    for x in xs {
      let x = x - self.middle.0;
      let coverage = if clear_of_corners || x.abs() <= half_width - self.largest - reach {
        self.along(x, half_width) * down
      } else {
        let sum: f32 = rows
          .iter()
          .map(|&(left, right, weight)| self.between(x, left, right) * weight)
          .sum();
        (sum / self.whole).clamp(0.0, 1.0)
      };
      mask.push((coverage * 255.0).round() as u8);
    }
  }

  /// The blur of the span from `-half` to `half` at `p`.
  fn along(&self, p: f32, half: f32) -> f32 {
    self.between(p, half, half)
  }

  /// The blur of the span from `-left` to `right` at `p`. One the blur
  /// stays inside of is covered whole.
  fn between(&self, p: f32, left: f32, right: f32) -> f32 {
    let reach = 3.0 * self.sigma;
    if -(left - reach) <= p && p <= right - reach {
      return 1.0;
    }
    let scale = std::f32::consts::FRAC_1_SQRT_2 / self.sigma;
    0.5 * (erf((p + left) * scale) - erf((p - right) * scale))
  }

  /// The rows of the shape a point at height `y` near a corner sums: for
  /// each sample within the shape and the Gaussian's reach, how far the
  /// row reaches left and right of the middle, and its weight.
  fn rows_near(&self, y: f32) -> [(f32, f32, f32); SAMPLES] {
    let (half_width, half_height) = self.half;
    let reach = 3.0 * self.sigma;
    let start = (y - half_height).clamp(-reach, reach);
    let end = (y + half_height).clamp(-reach, reach);
    let step = (end - start) / SAMPLES as f32;
    std::array::from_fn(|sample| {
      let offset = start + step * (sample as f32 + 0.5);
      // How far into a corner's rows this one is, and how much it
      // narrows there.
      let row = y - offset;
      let [top_left, top_right, bottom_right, bottom_left] = self.corners;
      let (left, right) = if row < 0.0 {
        (top_left, top_right)
      } else {
        (bottom_left, bottom_right)
      };
      let span = |radius: f32| {
        let into_corner = (row.abs() - (half_height - radius)).max(0.0);
        half_width - radius
          + (radius * radius - into_corner * into_corner)
            .max(0.0)
            .sqrt()
      };
      (span(left), span(right), gaussian(offset, self.sigma) * step)
    })
  }
}

/// The normal distribution's density at `x`, for a deviation of `sigma`.
fn gaussian(x: f32, sigma: f32) -> f32 {
  let root_two_pi = (2.0 * std::f32::consts::PI).sqrt();
  (-(x * x) / (2.0 * sigma * sigma)).exp() / (root_two_pi * sigma)
}

/// The error function, within 5e-4, an eighth of a color level, by
/// Abramowitz and Stegun's 7.1.27, which needs no exponential.
fn erf(x: f32) -> f32 {
  let a = x.abs();
  let d = 1.0 + a * (0.278_393 + a * (0.230_389 + a * (0.000_972 + a * 0.078_108)));
  let d = d * d;
  (1.0 - 1.0 / (d * d)).copysign(x)
}

/// Whether a color glyph came from vector color layers (COLR), which swash
/// renders premultiplied, rather than an embedded bitmap, which it hands
/// over as stored: straight alpha.
///
/// cosmic-text doesn't re-export swash's `Source`, and swash isn't a
/// dependency of this crate, so the variant is read from its name.
fn is_color_outline(image: &SwashImage) -> bool {
  format!("{:?}", image.source).starts_with("ColorOutline")
}

/// Blends a color glyph's RGBA pixels at `opacity` out of 255. `rgba` says
/// whether they are premultiplied already.
fn blend_rgba(
  pixmap: &mut PixmapMut<'_>,
  left: i32,
  top: i32,
  width: u32,
  height: u32,
  (rgba, premultiplied): (&[u8], bool),
  opacity: u8,
) {
  if rgba.len() < width as usize * height as usize * 4 {
    return;
  }
  let Some((columns, columns_end, rows, rows_end)) = overlap(pixmap, left, top, width, height)
  else {
    return;
  };
  let stride = pixmap.width() as usize;
  let data = pixmap.data_mut();
  for row in rows..rows_end {
    for column in columns..columns_end {
      let index = (row * width as usize + column) * 4;
      let alpha = mul(rgba[index + 3], opacity);
      if alpha == 0 {
        continue;
      }
      // Premultiplied colors only need the opacity; straight ones need
      // their own alpha too.
      let factor = if premultiplied { opacity } else { alpha };
      let source = [
        mul(rgba[index], factor),
        mul(rgba[index + 1], factor),
        mul(rgba[index + 2], factor),
        alpha,
      ];
      let x = (i64::from(left) + column as i64) as usize;
      let y = (i64::from(top) + row as i64) as usize;
      over(data, (y * stride + x) * 4, source);
    }
  }
}

/// Adds `contour` to `kept` when the box around its points reaches into
/// `near`, given as left, top, right and bottom.
fn keep_near(
  kept: &mut PathBuilder,
  contour: &[PathSegment],
  [left, top, right, bottom]: [f32; 4],
) {
  let (mut low, mut high) = (
    (f32::INFINITY, f32::INFINITY),
    (f32::NEG_INFINITY, f32::NEG_INFINITY),
  );
  let points = contour.iter().flat_map(|segment| match *segment {
    PathSegment::MoveTo(a) | PathSegment::LineTo(a) => [Some(a), None, None],
    PathSegment::QuadTo(a, b) => [Some(a), Some(b), None],
    PathSegment::CubicTo(a, b, c) => [Some(a), Some(b), Some(c)],
    PathSegment::Close => [None; 3],
  });
  for point in points.flatten() {
    low = (low.0.min(point.x), low.1.min(point.y));
    high = (high.0.max(point.x), high.1.max(point.y));
  }
  if low.0 > right || low.1 > bottom || high.0 < left || high.1 < top {
    return;
  }
  for segment in contour {
    match *segment {
      PathSegment::MoveTo(a) => kept.move_to(a.x, a.y),
      PathSegment::LineTo(a) => kept.line_to(a.x, a.y),
      PathSegment::QuadTo(a, b) => kept.quad_to(a.x, a.y, b.x, b.y),
      PathSegment::CubicTo(a, b, c) => kept.cubic_to(a.x, a.y, b.x, b.y, c.x, c.y),
      PathSegment::Close => kept.close(),
    }
  }
}

/// A dash of `on` pixels and a gap of `off`, stretched or squeezed so a
/// whole number of them goes around `perimeter`, at least one. `None` for
/// a pattern that can't be drawn: a number that isn't finite or is
/// negative, no gap, or a period shorter than a pixel.
pub(crate) fn fit_dash(perimeter: f32, on: f32, off: f32) -> Option<(f32, f32)> {
  let period = on + off;
  if !(on >= 0.0 && off > 0.0 && period.is_finite() && period >= 1.0 && perimeter > 0.0) {
    return None;
  }
  let count = (perimeter / period).round().max(1.0);
  let stretch = perimeter / (count * period);
  Some((on * stretch, off * stretch))
}
