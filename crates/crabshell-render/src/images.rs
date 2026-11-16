//! Images: PNG, JPEG, GIF, WebP and SVG files, decoded under size caps and
//! scaled to the size they are drawn at. Image files come from other
//! programs and the network, so every decoder is told the caps before it
//! allocates.

use std::io::Cursor;
use std::path::Path;

use crabshell_ui::{Fit, ImageFilters};
use resvg::usvg;
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, PixmapRef, Transform};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::icons::{decode_png, halve, premultiplied, read_capped, svg_tree};

/// The largest image file read, in bytes.
pub(crate) const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// The largest SVG image read, in bytes.
const MAX_SVG_BYTES: u64 = 4 * 1024 * 1024;

/// The widest and tallest image decoded, in pixels.
const MAX_SIDE: u32 = 8192;

/// The most pixels an image may have: a 6K picture, 96 MiB decoded.
pub(crate) const MAX_PIXELS: u64 = 24_000_000;

/// The file at `path` as a `width` by `height` pixmap, scaled as `fit`
/// says, through `filters` at `scale` buffer pixels a logical pixel.
pub(crate) fn filtered(
  path: &Path,
  (width, height): (u32, u32),
  fit: Fit,
  filters: &ImageFilters,
  scale: f32,
) -> Result<Pixmap, String> {
  if width == 0 || height == 0 {
    return Err(String::from("an image with no pixels"));
  }
  finish(&decode(path)?, (width, height), fit, filters, scale)
}

/// `picture` scaled to `width` by `height` as `fit` says, through
/// `filters` at `scale`.
pub(crate) fn finish(
  picture: &Picture,
  (width, height): (u32, u32),
  fit: Fit,
  filters: &ImageFilters,
  scale: f32,
) -> Result<Pixmap, String> {
  if fit != Fit::Contain || filters.blur == 0.0 {
    let mut pixmap = draw(picture, width, height, fit)?;
    crate::filters::apply(&mut pixmap, filters, scale);
    return Ok(pixmap);
  }
  // A contained picture is blurred on its own, in whole pixels, before it
  // is centered, so its edges blur against themselves, not the clear box.
  let (x, y) = factors(picture.size(), (width, height), fit);
  let (source_width, source_height) = picture.size();
  let inner = (
    ((source_width * x).round() as u32).clamp(1, width),
    ((source_height * y).round() as u32).clamp(1, height),
  );
  let mut inside = draw(picture, inner.0, inner.1, Fit::Fill)?;
  crate::filters::apply(&mut inside, filters, scale);
  let mut pixmap = Pixmap::new(width, height).ok_or("an image with no pixels")?;
  pixmap.draw_pixmap(
    ((width - inner.0) / 2) as i32,
    ((height - inner.1) / 2) as i32,
    inside.as_ref(),
    &PixmapPaint::default(),
    Transform::identity(),
    None,
  );
  Ok(pixmap)
}

/// `picture` as a `width` by `height` pixmap, scaled as `fit` says.
fn draw(picture: &Picture, width: u32, height: u32, fit: Fit) -> Result<Pixmap, String> {
  match picture {
    Picture::Svg(tree) => svg(tree, width, height, fit),
    Picture::Pixels(image) => scale(image.as_ref(), width, height, fit),
  }
}

/// A decoded image file: pixels, or an SVG to draw at any size.
pub(crate) enum Picture {
  Pixels(Pixmap),
  Svg(Box<usvg::Tree>),
}

impl Picture {
  /// Its width and height, in pixels or the SVG's own units.
  fn size(&self) -> (f32, f32) {
    match self {
      Picture::Pixels(pixmap) => (pixmap.width() as f32, pixmap.height() as f32),
      Picture::Svg(tree) => (tree.size().width(), tree.size().height()),
    }
  }
}

/// Reads and decodes the file at `path` under the caps. The format comes
/// from the extension.
pub(crate) fn decode(path: &Path) -> Result<Picture, String> {
  let extension = path
    .extension()
    .and_then(|extension| extension.to_str())
    .map(str::to_ascii_lowercase);
  let image = match extension.as_deref() {
    Some("svg") => {
      let tree = svg_tree(&read_capped(path, MAX_SVG_BYTES)?)?;
      return Ok(Picture::Svg(Box::new(tree)));
    }
    Some("png") => decode_png(&read_capped(path, MAX_BYTES)?, MAX_SIDE, MAX_PIXELS)?,
    Some("jpg" | "jpeg") => jpeg(&read_capped(path, MAX_BYTES)?)?,
    Some("gif") => gif(&read_capped(path, MAX_BYTES)?)?,
    Some("webp") => webp(&read_capped(path, MAX_BYTES)?)?,
    _ => {
      return Err(String::from(
        "images must be .png, .jpg, .gif, .webp or .svg files",
      ));
    }
  };
  Ok(Picture::Pixels(image))
}

/// Refuses a `width` by `height` image past the caps.
pub(crate) fn check(width: u32, height: u32) -> Result<(), String> {
  if width == 0 || height == 0 {
    return Err(String::from("an image with no pixels"));
  }
  if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
    return Err(format!(
      "a {width}x{height} image is larger than {MAX_SIDE} pixels a side or {MAX_PIXELS} pixels"
    ));
  }
  Ok(())
}

fn jpeg(data: &[u8]) -> Result<Pixmap, String> {
  let options = DecoderOptions::default()
    .set_max_width(MAX_SIDE as usize)
    .set_max_height(MAX_SIDE as usize)
    .jpeg_set_out_colorspace(ColorSpace::RGBA);
  let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
  decoder
    .decode_headers()
    .map_err(|error| error.to_string())?;
  let info = decoder.info().ok_or("a JPEG without a size")?;
  let (width, height) = (u32::from(info.width), u32::from(info.height));
  check(width, height)?;
  let rgba = decoder.decode().map_err(|error| error.to_string())?;
  premultiplied(rgba, width, height)
}

/// The first frame of a GIF, on its whole canvas.
pub(crate) fn gif(data: &[u8]) -> Result<Pixmap, String> {
  crate::animation::Gif::open(data)?
    .next_frame()?
    .map(|(pixmap, _)| pixmap)
    .ok_or_else(|| String::from("a GIF with no frames"))
}

/// A WebP, or the first frame of an animated one.
// shortcut: image-webp sizes a lossy frame from its own header, up to
// 16383 a side, before checking it against the canvas size checked here;
// cap it in image-webp or parse the VP8 header if that matters.
fn webp(data: &[u8]) -> Result<Pixmap, String> {
  let mut decoder =
    image_webp::WebPDecoder::new(Cursor::new(data)).map_err(|error| error.to_string())?;
  let (width, height) = decoder.dimensions();
  check(width, height)?;
  decoder.set_memory_limit((MAX_PIXELS * 4) as usize);
  let size = decoder
    .output_buffer_size()
    .ok_or("the WebP is too large to decode")?;
  let mut buffer = vec![0; size];
  decoder
    .read_image(&mut buffer)
    .map_err(|error| error.to_string())?;
  let rgba = if decoder.has_alpha() {
    buffer
  } else {
    buffer
      .as_chunks::<3>()
      .0
      .iter()
      .flat_map(|&[r, g, b]| [r, g, b, 255])
      .collect()
  };
  premultiplied(rgba, width, height)
}

pub(crate) fn svg(tree: &usvg::Tree, width: u32, height: u32, fit: Fit) -> Result<Pixmap, String> {
  let size = tree.size();
  let mut pixmap = Pixmap::new(width, height).ok_or("an image with no pixels")?;
  let transform = placement((size.width(), size.height()), (width, height), fit);
  resvg::render(tree, transform, &mut pixmap.as_mut());
  Ok(pixmap)
}

/// `image` scaled into a `width` by `height` pixmap as `fit` says. An
/// image more than twice the size is first halved, averaging each 2 by 2
/// block, so detail shrinks smoothly instead of flickering.
pub(crate) fn scale(
  image: PixmapRef<'_>,
  width: u32,
  height: u32,
  fit: Fit,
) -> Result<Pixmap, String> {
  let mut halved;
  let mut image = image;
  loop {
    let (x, y) = factors(
      (image.width() as f32, image.height() as f32),
      (width, height),
      fit,
    );
    // shortcut: halves only while both axes shrink past half, so a Fill
    // squashing one axis alone can alias; halve per axis if that shows.
    if x >= 0.5 || y >= 0.5 {
      break;
    }
    halved = halve(image)?;
    image = halved.as_ref();
  }
  let mut pixmap = Pixmap::new(width, height).ok_or("an image with no pixels")?;
  let transform = placement(
    (image.width() as f32, image.height() as f32),
    (width, height),
    fit,
  );
  let paint = PixmapPaint {
    quality: FilterQuality::Bicubic,
    ..PixmapPaint::default()
  };
  pixmap.draw_pixmap(0, 0, image, &paint, transform, None);
  Ok(pixmap)
}

/// How much `fit` scales a `source` sized picture across and down to fill
/// `target`.
pub(crate) fn factors(source: (f32, f32), target: (u32, u32), fit: Fit) -> (f32, f32) {
  let across = target.0 as f32 / source.0;
  let down = target.1 as f32 / source.1;
  match fit {
    Fit::Fill => (across, down),
    Fit::Contain => (across.min(down), across.min(down)),
    Fit::Cover => (across.max(down), across.max(down)),
  }
}

/// Scales a `source` sized picture as `fit` says and centers it in
/// `target`; a cover's overflow is cut by the target's edges.
fn placement(source: (f32, f32), target: (u32, u32), fit: Fit) -> Transform {
  let (x, y) = factors(source, target, fit);
  Transform::from_scale(x, y).post_translate(
    (target.0 as f32 - source.0 * x) / 2.0,
    (target.1 as f32 - source.1 * y) / 2.0,
  )
}
