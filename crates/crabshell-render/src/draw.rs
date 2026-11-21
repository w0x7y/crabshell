//! Drawing a window's view into a buffer.
//!
//! The areas to repaint are drawn from scratch, one tile of a fixed grid at
//! a time, into an RGBA pixmap of the tile's size, then copied into the
//! buffer. The pixmap clips the drawing to the tile, and the copy converts
//! the pixels to the buffer's byte order, so tiny-skia, glyph masks, resvg
//! and PNG decoding all work in the RGBA order they share.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use cosmic_text::{FontSystem, LayoutGlyph, SwashCache, SwashContent, SwashImage};
use crabshell_ui::{
  Affine, CaptureView, Color, Content, IconView, ImageView, InputView, NodeView, Radii, Rect,
  Shadow, Size, Span, Style, TextSystem, TextView, WindowView,
};
use tiny_skia::{
  FillRule, FilterQuality, LineCap, LineJoin, Mask, Paint, PathBuilder, PathSegment, Pixmap,
  PixmapMut, PixmapPaint, Stroke, StrokeDash, Transform,
};

use crate::damage::{BufferAge, DamageHistory, PixelRect};
use crate::icons::{Drawn, Icons};
use crate::theme::IconLookup;

mod canvas;
mod effects;
mod gradient;
mod kept;
mod patched;
mod transition;

pub(crate) use kept::Kept;
use kept::{Area, Layer, Seen, Want};
#[cfg(test)]
pub(crate) use kept::{KEPT_LIMIT, PATCHES};

/// The side of a tile, in buffer pixels. tiny-skia cuts a curve where it
/// leaves the pixmap, which changes the antialiasing beside the cut, so
/// every pixel is drawn in the same tile whatever the damage, and a repaint
/// matches drawing everything.
const TILE: u32 = 64;

#[cfg(test)]
thread_local! {
  /// The tile side for this thread's draws, which a test sets larger to
  /// draw a reference with no seams.
  pub(crate) static TILE_SIDE: std::cell::Cell<u32> = const { std::cell::Cell::new(TILE) };
}

/// The side of a tile: [`TILE`], or what a test set.
fn tile_side() -> u32 {
  #[cfg(test)]
  return TILE_SIDE.get();
  #[cfg(not(test))]
  TILE
}

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
  /// The bytes every window's kept layers hold, against
  /// [`kept::KEPT_LIMIT`].
  kept: Arc<AtomicU64>,
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
  /// Nodes looked through for images a draw skips.
  skipped: u64,
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
      kept: Arc::default(),
    }
  }

  /// The bytes kept layers hold for every window, for tests.
  #[cfg(test)]
  pub(crate) fn kept_bytes(&self) -> u64 {
    self.kept.load(std::sync::atomic::Ordering::Relaxed)
  }

  /// Counts `bytes` as kept, for tests of the budget.
  #[cfg(test)]
  pub(crate) fn spend_kept(&self, bytes: u64) {
    self
      .kept
      .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
  }

  /// How many times an image's file was looked up, for tests.
  #[cfg(test)]
  pub(crate) fn image_lookups(&self) -> usize {
    self.icons.lookups()
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

  /// The nodes looked through for images a draw skips so far, for tests.
  #[cfg(test)]
  pub(crate) fn skipped_nodes(&self) -> u64 {
    self.work.skipped
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

  /// The lookup icon names go through, which follows
  /// [`set_icon_theme`](Renderer::set_icon_theme).
  pub fn icon_lookup(&self) -> IconLookup {
    self.icons.lookup()
  }

  /// The bytes the icon cache keeps, for tests.
  #[cfg(test)]
  pub(crate) fn cached_bytes(&self) -> usize {
    self.icons.cached_bytes()
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

  /// From now on, decodes image files on a worker thread, which starts
  /// with the first and ends once none is left: a GIF's frames, a blurred
  /// picture, and any file past 64 KiB or drawn past 256 by 256 pixels.
  /// Until its picture comes in, an image draws the picture its
  /// transition leaves, or the one it drew before at its size, or
  /// nothing. Without this, as for a snapshot, each is decoded as it is
  /// first drawn.
  ///
  /// The platform then follows [`decoding`](Self::decoding), and while it
  /// holds asks to be woken with
  /// [`wake_when_decoded`](Self::wake_when_decoded) and calls
  /// [`take_decoded`](Self::take_decoded) when it is.
  pub fn decode_off_thread(&mut self) {
    self.icons.decode_off_thread();
  }

  /// Loads the images of `view` whose status still waits, drawn or not,
  /// at their laid-out size at `scale`, or one pixel without one, and
  /// tells each status: at once, or when the worker is done with it. The
  /// platform calls it for every window, drawn or not, whenever
  /// [`image_sources_changed`](crabshell_ui::image_sources_changed) says
  /// a source changed.
  pub fn load_images(&mut self, view: &WindowView<'_>, scale: f32) {
    self.icons.load_unseen(&view.loading_images(), scale);
  }

  /// Whether pictures are being decoded for images drawn, or came in and
  /// weren't taken with [`take_decoded`](Self::take_decoded) yet.
  pub fn decoding(&self) -> bool {
    self.icons.decoding()
  }

  /// Calls `wake` once, from the worker thread, when a picture is
  /// decoded, or at once if one waits to be taken. It replaces a `wake`
  /// given before.
  pub fn wake_when_decoded(&mut self, wake: impl FnOnce() + Send + 'static) {
    self.icons.wake_when_decoded(Box::new(wake));
  }

  /// Whether the last draw drew an image whose picture is still being
  /// decoded, so the platform can hold a window's first frame for it.
  pub fn waits_for_pictures(&self) -> bool {
    self.icons.waited()
  }

  /// Caches the pictures decoded since the last call, logging each
  /// failure once, and returns the ids of the images that wait for them,
  /// for the platform to repaint with
  /// [`Window::repaint_image`](crabshell_ui::Window::repaint_image) in
  /// every window.
  pub fn take_decoded(&mut self) -> Vec<u64> {
    self.icons.take_decoded()
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

  /// How many jobs the decoding thread did, for tests.
  #[cfg(test)]
  pub(crate) fn decoded_off_thread(&self) -> usize {
    self.icons.worker_work().0
  }

  /// Whether the decoding thread runs, for tests.
  #[cfg(test)]
  pub(crate) fn decoder_running(&self) -> bool {
    self.icons.worker_work().1
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
  /// An image in that area it skips, at opacity 0 or cut out by its mask
  /// region, still has its picture loaded at the size drawing it would
  /// give, once a draw, so revealing it draws it on its first frame.
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
    self.icons.start_frame(history.id(), view.has_images());
    let plan = history.plan(damage, scale, width, height, age);
    history.kept.start(&self.kept);
    let tile = tile_side();
    let mut tiles: Vec<(u32, u32)> = Vec::new();
    for area in plan.repaint.iter().filter(|area| area.area() > 0) {
      for row in area.y / tile..=(area.y + area.height - 1) / tile {
        tiles
          .extend((area.x / tile..=(area.x + area.width - 1) / tile).map(|column| (row, column)));
      }
    }
    tiles.sort_unstable();
    tiles.dedup();
    let mut casts = HashMap::new();
    let mut fonts = text.font_system();
    for &(row, column) in &tiles {
      self.icons.start_tile((row, column));
      let (x, y) = (column * tile, row * tile);
      let size = (width.saturating_sub(x), height.saturating_sub(y));
      let area = PixelRect::new(x, y, tile.min(size.0), tile.min(size.1));
      self.scratch.clear();
      self.scratch.resize(area.area() as usize * 4, 0);
      let Some(mut pixmap) = PixmapMut::from_bytes(&mut self.scratch, area.width, area.height)
      else {
        continue;
      };
      let mut painter = Painter {
        grid: Grid {
          scale,
          zoom: 1.0,
          linear: Affine {
            sx: 1.0,
            ky: 0.0,
            kx: 0.0,
            sy: 1.0,
            tx: 0.0,
            ty: 0.0,
          },
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
        kept: &mut history.kept,
        clip: Some(Rect::new(0.0, 0.0, view.size().width, view.size().height)),
        extent: [0, 0, width, height],
      };
      painter.node(&mut pixmap, view.root());
      copy(&self.scratch, area, pixels, width, format);
    }
    history.kept.end();
    self.icons.repainted(&tiles);
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
        // Red and blue swapped in a whole word, which vectorizes, unlike
        // moving the bytes one by one.
        for (to, from) in to.iter_mut().zip(from) {
          let pixel = u32::from_le_bytes(*from);
          *to = (pixel & 0xff00_ff00 | (pixel >> 16) & 0xff | (pixel & 0xff) << 16).to_le_bytes();
        }
      }
    }
  }
}

/// Turns logical coordinates into pixels of the area being drawn.
#[derive(Clone, Copy)]
struct Grid {
  /// Pixels a logical pixel: the output's scale times `zoom`.
  scale: f32,
  /// How many times its own resolution a transformed layer is drawn at,
  /// from [`Affine::layer_zoom`]; 1 outside one.
  zoom: f32,
  /// The transforms around what is drawn, from its coordinates to the
  /// window's, without their moves.
  linear: Affine,
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

  /// The transforms around a node moved by `transform`, without their
  /// moves, and the zoom and scale its layer is drawn at.
  fn through(&self, transform: Affine) -> (Affine, f32, f32) {
    let (outer, inner) = (self.linear, transform);
    let linear = Transform::from_row(outer.sx, outer.ky, outer.kx, outer.sy, 0.0, 0.0).pre_concat(
      Transform::from_row(inner.sx, inner.ky, inner.kx, inner.sy, 0.0, 0.0),
    );
    let linear = Affine {
      sx: linear.sx,
      ky: linear.ky,
      kx: linear.kx,
      sy: linear.sy,
      tx: 0.0,
      ty: 0.0,
    };
    let zoom = linear.layer_zoom();
    (linear, zoom, self.scale / self.zoom * zoom)
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
  /// The window's layers kept from its last draw, for this one.
  kept: &'a mut Kept,
  /// Where the clipping widgets around what is drawn and the window's
  /// edges let anything show, in logical pixels; `None` inside a
  /// transform, whose coordinates are its own.
  clip: Option<Rect>,
  /// Outside a transform, the buffer pixels the layers being drawn into
  /// may hold, left, top, right and bottom, whatever the tile: a kept
  /// layer is cut where they are.
  extent: [u32; 4],
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
      kept: &mut *self.kept,
      clip: self.clip,
      extent: self.extent,
    }
  }

  /// The extent within a layer that holds what lands in `rect`.
  fn narrowed(&self, rect: Rect) -> [u32; 4] {
    let window = Grid {
      left: 0.0,
      top: 0.0,
      ..self.grid
    };
    let [left, top, right, bottom] = self.extent;
    match window.cover(rect, window.width, window.height) {
      Some((x, y, width, height)) => [
        left.max(x),
        top.max(y),
        right.min(x + width),
        bottom.min(y + height),
      ],
      None => [0; 4],
    }
  }

  /// Draws `node` and everything inside it. With an opacity below 1, a
  /// mask, a mask region or a content shadow, they are drawn into one layer first, which
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
      self.skip(node);
      return;
    }
    let shadow = effects::cast_by_content(&style);
    // A region that covers this area whole or not at all needs no layer;
    // a content shadow may still fall where it is clear.
    let region = node
      .mask_region()
      .filter(|_| node.transform().is_none() && shadow.is_none())
      .map(|region| {
        let inner = self.on(self.grid.moved(x, y));
        inner.region_cover(width, height, node.rect(), region)
      });
    if matches!(region, Some(effects::Cover::Clear)) {
      self.skip(node);
      return;
    }
    if let Some(transform) = node.transform() {
      self.transformed(pixmap, node, &style, transform, opacity);
    } else if opacity < 1.0
      || style.mask.is_some()
      || matches!(region, Some(effects::Cover::Part(_)))
      || shadow.is_some()
    {
      if opacity < 1.0 && self.kept_fade(pixmap, node, &style, shadow, opacity) {
        return;
      }
      // The layer covers only what the subtree paints in this area.
      let Some(mut layer) = Pixmap::new(width, height) else {
        return;
      };
      self.work.layer_pixels += u64::from(layer.width()) * u64::from(layer.height());
      let mut inner = self.on(self.grid.moved(x, y));
      inner.extent = inner.narrowed(node.bounds());
      inner.layer_contents(&mut layer.as_mut(), node, &style, shadow, region);
      composite(pixmap, (x as i32, y as i32), &layer, opacity, None);
    } else {
      self.node_contents(pixmap, node, &style);
    }
  }

  /// Loads the pictures of the images in `node` and everything inside it
  /// that a reveal would draw, which this draw skips, at the size drawing
  /// them would give, so the reveal draws them on its first frame. Each
  /// node skipped is looked through once a draw, and none in a window
  /// without images.
  fn skip(&mut self, node: NodeView<'_>) {
    if self.icons.look_through(node.path()) {
      let (grid, clip) = (self.grid, self.clip);
      self.skipped(node, grid, clip);
    }
  }

  /// [`skip`](Self::skip)'s walk of `node` on `grid`, loading the images
  /// that show within `clip`.
  fn skipped(&mut self, node: NodeView<'_>, mut grid: Grid, mut clip: Option<Rect>) {
    self.work.skipped += 1;
    if let Some(transform) = node.transform() {
      (grid.linear, grid.zoom, grid.scale) = grid.through(transform);
      // shortcut: the clip isn't carried into a transform's coordinates, so
      // a transformed subtree loads what clips cut away; map it if that costs.
      clip = None;
    }
    let shows =
      |clip: Option<Rect>, rect: Rect| clip.is_none_or(|clip| !clip.intersect(rect).is_empty());
    if let Content::Image(image) = node.content()
      && shows(clip, image.rect)
    {
      let (width, height) = (
        (image.rect.width * grid.scale).round(),
        (image.rect.height * grid.scale).round(),
      );
      if width >= 1.0 && height >= 1.0 {
        // `as` saturates, so a huge box stays in range.
        let size = (width as u32, height as u32);
        self.icons.skipped(&image, size, grid.scale);
      }
    }
    if node.clips() {
      let rect = node.rect();
      clip = Some(clip.map_or(rect, |clip| clip.intersect(rect)));
    }
    for child in node.children() {
      if shows(clip, child.bounds()) {
        self.skipped(child, grid, clip);
      }
    }
  }

  /// Whether this draw blends `node`'s layer from the last instead of
  /// drawing it, which is decided on the node's first visit: only when
  /// the last draw drew it through a layer too, nothing inside it changed
  /// since, and its own `transform` did, or without one its `opacity`.
  /// Outside a transform, where `want` says where the layer goes now;
  /// `fill` draws it over an area.
  fn keeps(
    &mut self,
    node: NodeView<'_>,
    (opacity, transform): (f32, Option<Affine>),
    want: Want,
    fill: impl FnOnce(&mut Painter<'_>, Area) -> Option<Pixmap>,
  ) -> bool {
    let frame = self.kept.frame;
    if let Some(seen) = self.kept.seen.get(node.path())
      && seen.frame == frame
    {
      return seen.layer.is_some();
    }
    let seen = self.kept.seen.remove(node.path());
    let revision = node.revision();
    // A transformed layer kept is drawn whole, without the seams of one
    // cut to each tile, so it is kept only while the transform moves,
    // when those seams move too; a still one fading draws as before.
    let keep = seen.as_ref().is_some_and(|seen| {
      seen.revision == revision
        && match transform {
          Some(_) => seen.transform != transform,
          None => seen.opacity != opacity,
        }
    });
    let Want { needed, whole, at } = want;
    let fits = |layer: &Layer| layer.area.holds(&needed) && layer.at == at;
    let mut layer = seen
      .and_then(|seen| seen.layer)
      .filter(|layer| keep && fits(layer));
    let pixels = |area: &Area| u64::from(area.width) * u64::from(area.height);
    // All of it when it fits, so a layer that moves into view needn't be
    // drawn again.
    let area = [whole, needed]
      .into_iter()
      .find(|area| pixels(area) <= LAYER_LIMIT);
    if keep
      && layer.is_none()
      && let Some(area) = area
    {
      // A transform inside takes its zoom from the one moving around it.
      let (nested, playing) = self.inside(node);
      let refused = playing || nested && transform.is_some();
      let held = (!refused).then(|| self.kept.hold(pixels(&area) * 4));
      if let Some(held) = held.flatten()
        && let Some(pixmap) = fill(self, area)
      {
        layer = Some(Layer {
          pixmap,
          area,
          at,
          _held: held,
        });
      }
    }
    let kept = layer.is_some();
    self.kept.seen.insert(
      node.path().to_owned(),
      Seen {
        frame,
        revision,
        opacity,
        transform,
        layer,
      },
    );
    kept
  }

  /// Whether anything inside `node` has a transform of its own, and
  /// whether a GIF plays inside it, which a kept layer would stop.
  fn inside(&self, node: NodeView<'_>) -> (bool, bool) {
    node
      .children()
      .fold((false, false), |(nested, playing), child| {
        let (deeper, plays) = self.inside(child);
        let gif = matches!(child.content(), Content::Image(image) if self.icons.animates(image.id));
        (
          nested || deeper || child.transform().is_some(),
          playing || plays || gif,
        )
      })
  }

  /// Blends `node`'s kept layer in at `opacity`, if this draw keeps one,
  /// drawn over all of the window it can show in a tile at a time as
  /// each tile's own layer is, so the pixels match.
  fn kept_fade(
    &mut self,
    pixmap: &mut PixmapMut<'_>,
    node: NodeView<'_>,
    style: &Style,
    shadow: Option<Shadow>,
    opacity: f32,
  ) -> bool {
    let Some(clip) = self.clip else {
      return false;
    };
    let window = Grid {
      left: 0.0,
      top: 0.0,
      ..self.grid
    };
    let Some((x, y, width, height)) =
      window.cover(node.bounds().intersect(clip), window.width, window.height)
    else {
      return false;
    };
    let area = Area {
      x: x as i32,
      y: y as i32,
      width,
      height,
    };
    let want = Want {
      needed: area,
      whole: area,
      at: (window.scale, window.zoom),
    };
    let fill = |painter: &mut Painter<'_>, _: Area| {
      let mut kept = Pixmap::new(width, height)?;
      let side = tile_side();
      for row in y / side..=(y + height - 1) / side {
        for column in x / side..=(x + width - 1) / side {
          // What the tile's own layer holds: the tile, cut where the
          // layers around are.
          let [ex, ey, ex1, ey1] = painter.extent;
          let (left, top) = ((column * side).max(ex), (row * side).max(ey));
          let right = ((column + 1) * side).min(window.width).min(ex1);
          let bottom = ((row + 1) * side).min(window.height).min(ey1);
          if left >= right || top >= bottom {
            continue;
          }
          let tile = window.moved(left, top);
          let size = (right - left, bottom - top);
          let Some((dx, dy, w, h)) = tile.cover(node.bounds(), size.0, size.1) else {
            continue;
          };
          let mut layer = Pixmap::new(w, h)?;
          painter.work.layer_pixels += u64::from(w) * u64::from(h);
          let extent = painter.narrowed(node.bounds());
          let mut inner = painter.on(tile.moved(dx, dy));
          inner.extent = extent;
          inner.layer_contents(&mut layer.as_mut(), node, style, shadow, None);
          let at = ((left + dx) as i32 - area.x, (top + dy) as i32 - area.y);
          transition::blit(&mut kept.as_mut(), at, &layer);
        }
      }
      Some(kept)
    };
    if !self.keeps(node, (opacity, None), want, fill) {
      return false;
    }
    let Some(layer) = self.kept.layer(node.path()) else {
      return false;
    };
    // The grid's corner is a whole pixel.
    let at = (
      layer.area.x - self.grid.left as i32,
      layer.area.y - self.grid.top as i32,
    );
    composite(pixmap, at, &layer.pixmap, opacity, None);
    true
  }

  /// What `node_contents` draws into a fresh `layer`, kept only where the
  /// style's mask is opaque and inside the node's mask region, which
  /// `cover` holds when it was worked out over this layer already, with
  /// `shadow` cast by what is left put under it.
  fn layer_contents(
    &mut self,
    layer: &mut PixmapMut<'_>,
    node: NodeView<'_>,
    style: &Style,
    shadow: Option<Shadow>,
    cover: Option<effects::Cover>,
  ) {
    self.node_contents(layer, node, style);
    if let Some(mask) = &style.mask {
      self.mask(layer, node.rect(), mask);
    }
    let (width, height) = (layer.width(), layer.height());
    let cover = cover.or_else(|| {
      let region = node.mask_region()?;
      Some(self.region_cover(width, height, node.rect(), region))
    });
    match cover {
      Some(effects::Cover::Clear) => layer.fill(tiny_skia::Color::TRANSPARENT),
      Some(effects::Cover::Part(cut)) => layer.apply_mask(&cut),
      Some(effects::Cover::Whole) | None => {}
    }
    if let Some(shadow) = shadow {
      self.content_shadow(layer, node, &shadow);
    }
  }

  /// Draws `node` and everything inside it into a layer in its own
  /// coordinates, then blends that in through its `transform`, filtered
  /// bilinearly, at `opacity`: one layer for both. The layer covers only
  /// what lands in this pixmap, in the node's own buffer pixels times the
  /// zoom of every transform around it, so what they scale up stays
  /// sharp, and a pixel is always drawn the same way in the same tile.
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
    let zoomed = self.grid.through(transform);
    let layer_scale = zoomed.2;
    // From the node's own pixels, zoomed, to its parent's.
    let device = Transform::from_scale(scale, scale)
      .pre_concat(Transform::from_row(sx, ky, kx, sy, tx, ty))
      .pre_scale(1.0 / layer_scale, 1.0 / layer_scale);
    let to_pixmap = device.post_translate(-left, -top);
    if let Some(clip) = self.clip {
      // All of the window it can show in, for a layer kept.
      let shown = tiny_skia::Rect::from_ltrb(
        (clip.x * scale).floor(),
        (clip.y * scale).floor(),
        (clip.right() * scale).ceil(),
        (clip.bottom() * scale).ceil(),
      );
      let needed = shown.and_then(|shown| layer_area(node, device, Some(shown), layer_scale));
      let whole = layer_area(node, device, None, layer_scale);
      if let (Some(needed), Some(whole)) = (needed, whole) {
        let want = Want {
          needed,
          whole,
          at: (layer_scale, zoomed.1),
        };
        let fill =
          |painter: &mut Painter<'_>, area| painter.transformed_layer(node, style, area, zoomed);
        if self.keeps(node, (opacity, Some(transform)), want, fill)
          && let Some(layer) = self.kept.layer(node.path())
        {
          let corner = (layer.area.x as f32, layer.area.y as f32);
          let at = to_pixmap.pre_translate(corner.0, corner.1);
          blend_transformed(pixmap, &layer.pixmap, at, opacity);
          return;
        }
      }
    }
    let shown = tiny_skia::Rect::from_xywh(0.0, 0.0, pixmap.width() as f32, pixmap.height() as f32);
    let Some(area) = shown.and_then(|shown| layer_area(node, to_pixmap, Some(shown), layer_scale))
    else {
      return;
    };
    let pixels = u64::from(area.width) * u64::from(area.height);
    if pixels > LAYER_LIMIT {
      tracing::debug!(
        "{} is transformed into a layer too large to draw",
        node.path()
      );
      return;
    }
    let Some(layer) = self.transformed_layer(node, style, area, zoomed) else {
      return;
    };
    let at = to_pixmap.pre_translate(area.x as f32, area.y as f32);
    blend_transformed(pixmap, &layer, at, opacity);
  }

  /// `node`'s layer over `area`, in its own pixels at the scale and zoom
  /// `through` worked out, with the transforms around it.
  fn transformed_layer(
    &mut self,
    node: NodeView<'_>,
    style: &Style,
    area: Area,
    (linear, zoom, layer_scale): (Affine, f32, f32),
  ) -> Option<Pixmap> {
    let mut layer = Pixmap::new(area.width, area.height)?;
    self.work.layer_pixels += u64::from(area.width) * u64::from(area.height);
    let (width, height) = if zoom == self.grid.zoom {
      (self.grid.width, self.grid.height)
    } else {
      // `as` saturates; the window's edge is past any layer drawn.
      let window = self.grid.window;
      (
        (window.width * layer_scale).round() as u32,
        (window.height * layer_scale).round() as u32,
      )
    };
    let mut inner = self.on(Grid {
      scale: layer_scale,
      zoom,
      linear,
      width,
      height,
      left: area.x as f32,
      top: area.y as f32,
      ..self.grid
    });
    inner.clip = None;
    let shadow = effects::cast_by_content(style);
    inner.layer_contents(&mut layer.as_mut(), node, style, shadow, None);
    Some(layer)
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
        let color = text.color();
        let tint = Color::rgba(color.r, color.g, color.b, color.a / 3);
        self.selection(pixmap, &text.selection(), tint);
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
    let rect = node.rect();
    self.through_mask(pixmap, rect, (inside, corners), |painter, layer| {
      painter.clip = painter.clip.map(|clip| clip.intersect(rect));
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
    let extent = self.narrowed(cover);
    if covers(pixmap, shape, corners) {
      // The mask would keep everything, so the layer adds nothing.
      let grid = self.grid;
      let mut inner = self.on(grid);
      inner.extent = extent;
      draw(&mut inner, pixmap);
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
    inner.extent = extent;
    draw(&mut inner, &mut layer.as_mut());
    let mut path = PathBuilder::new();
    rounded_rect(&mut path, shape, corners);
    let Some(path) = path.finish() else {
      return;
    };
    mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
    composite(pixmap, (x as i32, y as i32), &layer, 1.0, Some(&mask));
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

    if let Some(color) = style.background
      && !(covers(pixmap, area, corners) && fill_all(pixmap, color))
      && !(corners == [0.0; 4] && fill_opaque(pixmap, area, color))
    {
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
      let inner = tiny_skia::Rect::from_ltrb(
        area.left() + width,
        area.top() + width,
        area.right() - width,
        area.bottom() - width,
      );
      // A ring whose hole covers the pixmap has nothing in it.
      if inner.is_some_and(|inner| covers(pixmap, inner, inset(corners, width))) {
        return;
      }
      let dashed = width >= 1.0
        && style.border_dash.is_some_and(|dash| {
          self.dashed_border(pixmap, (area, corners), width, border.color, dash)
        });

      if width >= 1.0 && !dashed {
        // A ring: the outer shape with the inner one cut out.
        let mut path = PathBuilder::new();
        rounded_rect(&mut path, area, corners);
        if let Some(inner) = inner {
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
  /// of its shape, past which it fades below a 255th. A large one is
  /// drawn from a small one of its corners, blur and place within a pixel,
  /// worked out once while drawing keeps using it.
  fn shadow(&mut self, pixmap: &mut PixmapMut<'_>, rect: Rect, style: &Style, shadow: &Shadow) {
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
    if shadow.cut_out && covers(pixmap, area, corners) {
      return;
    }
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
    let patch = patched::Patch::key(shape, corners, sigma).and_then(|key| self.kept.patch(key));
    let blurred = patch
      .is_none()
      .then(|| BlurredBox::new(shape, corners, sigma));
    let stride = pixmap.width() as usize;
    // A cut-out shadow leaves a column the box covers in every row clear,
    // so only the runs of columns between them are worked out.
    let clear = |column: u32| {
      cut.as_ref().is_some_and(|cut| {
        let at = |row: u32| row as usize * stride + column as usize;
        rows.clone().all(|row| cut.data()[at(row)] == 255)
      })
    };
    let mut start = columns.start;
    while start < columns.end {
      if clear(start) {
        start += 1;
        continue;
      }
      let end = (start + 1..columns.end)
        .find(|&column| clear(column))
        .unwrap_or(columns.end);
      let run = start..end;
      start = end;
      let mut mask = Vec::with_capacity(run.len() * rows.len());
      for row in rows.clone() {
        match (patch, &blurred) {
          (Some(patch), _) => {
            // The grid's corner is a whole pixel.
            let columns = run.clone().map(|column| left as i64 + i64::from(column));
            patch.row(shape, top as i64 + i64::from(row), columns, &mut mask);
          }
          (None, Some(blurred)) => {
            let xs = run.clone().map(|column| left + column as f32 + 0.5);
            blurred.row(top + row as f32 + 0.5, xs, &mut mask);
          }
          (None, None) => {}
        }
      }
      if let Some(cut) = &cut {
        let under = rows.clone().flat_map(|row| {
          run
            .clone()
            .map(move |column| row as usize * stride + column as usize)
        });
        for (alpha, at) in mask.iter_mut().zip(under) {
          *alpha = mul(*alpha, 255 - cut.data()[at]);
        }
      }
      let width = run.len() as u32;
      let height = rows.len() as u32;
      let (x, y) = (run.start as i32, rows.start as i32);
      blend_mask(pixmap, x, y, width, height, &mask, shadow.color);
    }
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
      // A lit link takes the hover color, if the text has one.
      let span_color = |span: &Span, glyph: &LayoutGlyph| match text.glyph_lit(run.line_i, glyph) {
        true => text.link_hover_color().or(span.color),
        false => span.color,
      };
      for glyph in run.glyphs.iter() {
        let span = text.span(run.line_i, glyph);
        let color = text.selected_color(run.line_i, glyph);
        let color = color.or_else(|| span.and_then(|span| span_color(span, glyph)));
        let color = color.unwrap_or(text.color());
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
        let lit = text.glyph_lit(run.line_i, &glyphs[0]);
        let lines = (span.underline || lit, span.strikethrough);
        if lines != (false, false) {
          let color = span_color(span, &glyphs[0]).unwrap_or(text.color());
          self.decorations(pixmap, origin, glyphs, baseline, lines, color);
        }
      }
    }
  }

  /// The underline and strikethrough across `glyphs`, each run of
  /// glyphs from one font at the place and thickness that font gives, so
  /// a fallback glyph's line sits where its own font puts it.
  fn decorations(
    &mut self,
    pixmap: &mut PixmapMut<'_>,
    origin: f32,
    glyphs: &[LayoutGlyph],
    baseline: f32,
    lines: (bool, bool),
    color: Color,
  ) {
    let same_font = |a: &LayoutGlyph, b: &LayoutGlyph| {
      (a.font_id, a.font_weight, a.font_size) == (b.font_id, b.font_weight, b.font_size)
    };
    for glyphs in glyphs.chunk_by(same_font) {
      self.font_decorations(pixmap, origin, glyphs, baseline, lines, color);
    }
  }

  /// The underline and strikethrough across `glyphs`, all from one font,
  /// at the place and thickness it gives, as cosmic-text draws them.
  /// Edges snap to whole pixels in the buffer, so every tile agrees, and
  /// each line is at least a pixel thick. A line is kept within half an
  /// em of the baseline, which the text's painted area covers.
  fn font_decorations(
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

  /// A field's selection in its color, the text, and a caret in the
  /// text's color.
  fn input_contents(&mut self, pixmap: &mut PixmapMut<'_>, input: &InputView<'_>) {
    let color = input.text.color();
    self.selection(pixmap, input.selection, input.selection_color);
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

  /// A selection, behind text, in `tint`.
  fn selection(&mut self, pixmap: &mut PixmapMut<'_>, spans: &[Rect], tint: Color) {
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
      Some(Drawn::One(pixels)) if (pixels.width(), pixels.height()) == (width, height) => {
        transition::blit(pixmap, (x, y), pixels);
      }
      // A picture of another size, while this one is decoded.
      Some(Drawn::One(pixels)) => {
        let paint = PixmapPaint {
          quality: FilterQuality::Bilinear,
          ..PixmapPaint::default()
        };
        let stretch = Transform::from_scale(
          width as f32 / pixels.width() as f32,
          height as f32 / pixels.height() as f32,
        )
        .post_translate(x as f32, y as f32);
        pixmap.draw_pixmap(0, 0, pixels.as_ref(), &paint, stretch, None);
      }
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
    // The same test `shown` makes, so a view that draws nothing isn't
    // shown.
    let Some([left, top, part_width, part_height]) = frame.drawn_part(capture.crop) else {
      return;
    };
    let crop = capture.crop.and_then(|_| {
      // Within the frame, which is at most 8192 pixels a side.
      let sides = [left, top, part_width, part_height].map(|side| side as i32);
      tiny_skia::IntRect::from_xywh(sides[0], sides[1], part_width, part_height)
    });
    let size = (width, height);
    if let Some(pixels) = self
      .icons
      .capture(capture.id, frame, size, capture.fit, crop)
    {
      transition::blit(pixmap, (x, y), pixels);
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
    // Asked twice when there is a fallback; the second is a cache hit.
    let source = match icon.fallback {
      Some(fallback) if self.icons.get(icon.source, sizes, icon.tint).is_none() => fallback,
      _ => icon.source,
    };
    if let Some(image) = self.icons.get(source, sizes, icon.tint) {
      // `as` saturates, so a huge offset stays in range.
      transition::blit(pixmap, (x as i32, y as i32), image);
    }
  }
}

/// Whether the rectangle `shape` with `corners` covers every pixel of the
/// pixmap whole: its edges are at or past the pixmap's, and no corner's
/// curve reaches into it.
fn covers(pixmap: &PixmapMut<'_>, shape: tiny_skia::Rect, corners: Corners) -> bool {
  let (width, height) = (pixmap.width() as f32, pixmap.height() as f32);
  let (left, top, right, bottom) = (shape.left(), shape.top(), shape.right(), shape.bottom());
  if !(left <= 0.0 && top <= 0.0 && right >= width && bottom >= height) {
    return false;
  }
  // Each corner's curve stays inside the square of its radius.
  corners.iter().enumerate().all(|(index, &radius)| {
    let x = if matches!(index, 0 | 3) {
      left
    } else {
      right - radius
    };
    let y = if index < 2 { top } else { bottom - radius };
    radius <= 0.0 || x + radius <= 0.0 || x >= width || y + radius <= 0.0 || y >= height
  })
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

/// The pixels a layer of `node` at `layer_scale` holds so that what
/// `to_target` maps into `shown`, or all of it, comes out whole: its inner
/// bounds there, and a margin where tiny-skia cuts its curves. `None` if
/// none show.
fn layer_area(
  node: NodeView<'_>,
  to_target: Transform,
  shown: Option<tiny_skia::Rect>,
  layer_scale: f32,
) -> Option<Area> {
  let inner = node.inner_bounds();
  let drawn = tiny_skia::Rect::from_xywh(
    inner.x * layer_scale,
    inner.y * layer_scale,
    inner.width * layer_scale,
    inner.height * layer_scale,
  )?;
  let drawn = match shown {
    Some(shown) => shown.transform(to_target.invert()?)?.intersect(&drawn)?,
    None => drawn,
  };
  let area = drawn.outset(LAYER_MARGIN, LAYER_MARGIN)?.round_out()?;
  Some(Area {
    x: area.x(),
    y: area.y(),
    width: area.width(),
    height: area.height(),
  })
}

/// Blends a transformed `layer` in at `opacity`, its corner going where
/// `at` says, filtered bilinearly.
pub(crate) fn blend_transformed(
  pixmap: &mut PixmapMut<'_>,
  layer: &Pixmap,
  at: Transform,
  opacity: f32,
) {
  if at.kx == 0.0 && at.ky == 0.0 && at.sx > 0.0 && at.sy > 0.0 && !at.is_translate() {
    blend_scaled(pixmap, layer, at, opacity);
    return;
  }
  if at.is_identity() || at.is_translate() {
    // tiny-skia samples a moved pixmap at the nearest pixel, whatever the
    // quality asked for, which puts it at this whole-pixel offset.
    let offset = ((at.tx - 0.5).ceil(), (at.ty - 0.5).ceil());
    // `as` saturates; a layer that far off misses the pixmap.
    composite(
      pixmap,
      (offset.0 as i32, offset.1 as i32),
      layer,
      opacity,
      None,
    );
    return;
  }
  let paint = PixmapPaint {
    opacity,
    quality: FilterQuality::Bilinear,
    ..PixmapPaint::default()
  };
  pixmap.draw_pixmap(0, 0, layer.as_ref(), &paint, at, None);
}

/// [`blend_transformed`] for a transform that only scales and moves:
/// tiny-skia's pipeline gives the same within a level, twice as slow,
/// which a layer the size of a window notices. Each pixel whose middle
/// lands on the layer mixes the four layer pixels around where it lands,
/// the edge ones past it. Each layer row is mixed across once, for the
/// two pixmap rows that need it, then mixed down and blended in.
fn blend_scaled(pixmap: &mut PixmapMut<'_>, layer: &Pixmap, at: Transform, opacity: f32) {
  // Along one axis: the first pixel, then for each the two layer pixels it
  // mixes and how far it is from the first to the second.
  let taps = |scale: f32, offset: f32, length: u32, limit: u32| {
    let start = (offset - 0.5).ceil().max(0.0);
    let end = (offset + length as f32 * scale - 0.5)
      .ceil()
      .min(limit as f32);
    let last = length as usize - 1;
    // `as` saturates, so a far-off layer gives an empty range.
    let taps: Vec<(usize, usize, f32)> = (start as u32..end.max(start) as u32)
      .map(|pixel| {
        let x = (pixel as f32 + 0.5 - offset) / scale - 0.5;
        let floor = x.floor();
        let index = |at: f32| (at.max(0.0) as usize).min(last);
        (index(floor), index(floor + 1.0), x - floor)
      })
      .collect();
    (start as usize, taps)
  };
  let (x0, columns) = taps(at.sx, at.tx, layer.width(), pixmap.width());
  let (y0, rows) = taps(at.sy, at.ty, layer.height(), pixmap.height());
  let opacity = opacity.clamp(0.0, 1.0);
  let stride = pixmap.width() as usize;
  let width = layer.width() as usize;
  let source = layer.data().as_chunks::<4>().0;
  let data = pixmap.data_mut().as_chunks_mut::<4>().0;
  // Layer row `index` mixed across at the columns, in levels.
  let across = |index: usize, mixed: &mut Vec<[f32; 4]>| {
    let row = &source[index * width..(index + 1) * width];
    mixed.clear();
    mixed.extend(columns.iter().map(|&(left, right, t)| {
      let (a, b) = (row[left], row[right]);
      std::array::from_fn(|channel| {
        let (a, b) = (f32::from(a[channel]), f32::from(b[channel]));
        a + (b - a) * t
      })
    }));
  };
  let (mut upper, mut lower) = (Vec::new(), Vec::new());
  let (mut upper_row, mut lower_row) = (usize::MAX, usize::MAX);
  for (y, &(top, bottom, down)) in rows.iter().enumerate() {
    if upper_row != top {
      if lower_row == top {
        std::mem::swap(&mut upper, &mut lower);
        std::mem::swap(&mut upper_row, &mut lower_row);
      } else {
        across(top, &mut upper);
        upper_row = top;
      }
    }
    if lower_row != bottom {
      across(bottom, &mut lower);
      lower_row = bottom;
    }
    let (near, far) = ((1.0 - down) * opacity, down * opacity);
    let start = (y0 + y) * stride + x0;
    let target = &mut data[start..start + columns.len()];
    for ((to, a), b) in target.iter_mut().zip(&upper).zip(&lower) {
      let mixed: [f32; 4] = std::array::from_fn(|channel| a[channel] * near + b[channel] * far);
      // Over what is there, which keeps 1 less the alpha of itself.
      let rest = 1.0 - mixed[3] * (1.0 / 255.0);
      *to = std::array::from_fn(|channel| {
        // `as` saturates below 0.
        (mixed[channel] + f32::from(to[channel]) * rest + 0.5).min(255.0) as u8
      });
    }
  }
}

/// Blends the premultiplied `layer` over the pixmap with its corner at
/// `left`, `top`, at `opacity` and through `mask`, which is the pixmap's
/// size. tiny-skia's `draw_pixmap` gives the same within a level at a
/// whole-pixel offset, through a float pipeline several times slower,
/// which a layer the size of the screen notices.
pub(crate) fn composite(
  pixmap: &mut PixmapMut<'_>,
  (left, top): (i32, i32),
  layer: &Pixmap,
  opacity: f32,
  mask: Option<&Mask>,
) {
  if opacity >= 1.0 && mask.is_none() {
    transition::blit(pixmap, (left, top), layer);
    return;
  }
  let width = layer.width() as usize;
  let Some((x0, x1, y0, y1)) = overlap(pixmap, left, top, layer.width(), layer.height()) else {
    return;
  };
  // In 65536ths, so an opacity near 0 or 1 isn't rounded a level off.
  let scale = (opacity.clamp(0.0, 1.0) * 65536.0).round() as u32;
  let stride = pixmap.width() as usize;
  let mask = mask.map(Mask::data);
  let data = pixmap.data_mut();
  let source = layer.data();
  // Without branches, so the loops vectorize; a clear pixel or one the
  // mask leaves out blends as nothing.
  let fade = |pixel: [u8; 4], coverage: u8| {
    pixel.map(|channel| mul(((u32::from(channel) * scale + 32768) >> 16) as u8, coverage))
  };
  let blend = |to: &mut [u8; 4], from: [u8; 4]| {
    let rest = 255 - from[3];
    *to = std::array::from_fn(|channel| from[channel].saturating_add(mul(to[channel], rest)));
  };
  for y in y0..y1 {
    // `overlap` keeps these inside the pixmap.
    let start =
      (i64::from(top) + y as i64) as usize * stride + (i64::from(left) + x0 as i64) as usize;
    let end = start + (x1 - x0);
    let row = source[(y * width + x0) * 4..(y * width + x1) * 4]
      .as_chunks::<4>()
      .0;
    let target = data[start * 4..end * 4].as_chunks_mut::<4>().0;
    match mask {
      Some(mask) => {
        for ((to, from), &coverage) in target.iter_mut().zip(row).zip(&mask[start..end]) {
          blend(to, fade(*from, coverage));
        }
      }
      None => {
        for (to, from) in target.iter_mut().zip(row) {
          blend(to, fade(*from, 255));
        }
      }
    }
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

/// Fills the whole pixmap with `color`, blended over what is there, as
/// tiny-skia's antialiased path fill gives a shape covering it, within a
/// level, without its raster pipeline. `true`, so `draw_box` chains it as
/// it does `fill_opaque`.
pub(crate) fn fill_all(pixmap: &mut PixmapMut<'_>, color: Color) -> bool {
  let alpha = color.a;
  let source = [
    mul(color.r, alpha),
    mul(color.g, alpha),
    mul(color.b, alpha),
    alpha,
  ];
  let pixels = pixmap.data_mut().as_chunks_mut::<4>().0;
  match alpha {
    0 => {}
    255 => pixels.fill(source),
    _ => {
      let rest = 255 - alpha;
      for pixel in pixels {
        *pixel =
          std::array::from_fn(|channel| source[channel].saturating_add(mul(pixel[channel], rest)));
      }
    }
  }
  true
}

/// Fills `area` with the opaque `color` straight into the pixels, as
/// tiny-skia's path fill gives it, without its raster pipeline: a
/// window's background under a picture was half of a 1080p repaint.
/// `false`, drawing nothing, unless `color` is opaque and every edge is on
/// a whole pixel.
pub(crate) fn fill_opaque(pixmap: &mut PixmapMut<'_>, area: tiny_skia::Rect, color: Color) -> bool {
  let edges = [area.left(), area.top(), area.right(), area.bottom()];
  if color.a != 255 || edges.iter().any(|edge| edge.fract() != 0.0) {
    return false;
  }
  let (width, height) = (pixmap.width() as f32, pixmap.height() as f32);
  let [left, top, right, bottom] = [
    area.left().clamp(0.0, width),
    area.top().clamp(0.0, height),
    area.right().clamp(0.0, width),
    area.bottom().clamp(0.0, height),
  ]
  .map(|edge| edge as usize);
  let stride = pixmap.width() as usize * 4;
  let pixel = [color.r, color.g, color.b, 255];
  for row in pixmap
    .data_mut()
    .chunks_exact_mut(stride)
    .take(bottom)
    .skip(top)
  {
    row[left * 4..right * 4].as_chunks_mut::<4>().0.fill(pixel);
  }
  true
}
