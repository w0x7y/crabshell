//! Image filters: a tint, brightness, contrast and saturation as CSS's
//! filter functions define them, and a Gaussian blur. They run once on a
//! picture at the size it is drawn, in buffer pixels, and the cache keeps
//! the result.

use crabshell_ui::{Color, ImageFilters};
use tiny_skia::Pixmap;

/// Runs `filters` on `pixmap` in place, in their order: the tint, then
/// brightness, contrast and saturation, then the blur, whose standard
/// deviation is `filters.blur` logical pixels at `scale`.
pub(crate) fn apply(pixmap: &mut Pixmap, filters: &ImageFilters, scale: f32) {
  if let Some(tint) = filters.tint {
    self::tint(pixmap.data_mut(), tint);
  }
  colors(pixmap.data_mut(), filters);
  let sigma = filters.blur * scale;
  if sigma > 0.0 {
    let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
    blur(pixmap.data_mut(), width, height, sigma);
  }
}

/// Paints every premultiplied pixel in `tint`, keeping its alpha, times
/// the tint's own.
pub(crate) fn tint(data: &mut [u8], tint: Color) {
  for pixel in data.as_chunks_mut::<4>().0 {
    let alpha = mul(pixel[3], tint.a);
    *pixel = [
      mul(tint.r, alpha),
      mul(tint.g, alpha),
      mul(tint.b, alpha),
      alpha,
    ];
  }
}

/// Brightness, contrast and saturation, each clamped before the next as
/// CSS does, worked on premultiplied pixels: a channel's range is 0 to the
/// pixel's alpha.
fn colors(data: &mut [u8], filters: &ImageFilters) {
  let ImageFilters {
    brightness,
    contrast,
    saturation,
    ..
  } = *filters;
  if brightness == 1.0 && contrast == 1.0 && saturation == 1.0 {
    return;
  }
  let s = saturation;
  // CSS's saturate() matrix, from Filter Effects Module Level 1.
  let matrix = [
    [0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s],
    [0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s],
    [0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s],
  ];
  for pixel in data.as_chunks_mut::<4>().0 {
    if pixel[3] == 0 {
      continue;
    }
    let alpha = f32::from(pixel[3]);
    let mut rgb = [pixel[0], pixel[1], pixel[2]].map(f32::from);
    if brightness != 1.0 {
      rgb = rgb.map(|channel| (channel * brightness).clamp(0.0, alpha));
    }
    if contrast != 1.0 {
      let middle = alpha / 2.0;
      rgb = rgb.map(|channel| ((channel - middle) * contrast + middle).clamp(0.0, alpha));
    }
    if saturation != 1.0 {
      let [r, g, b] = rgb;
      rgb = matrix.map(|row| (row[0] * r + row[1] * g + row[2] * b).clamp(0.0, alpha));
    }
    for (channel, value) in pixel.iter_mut().zip(rgb) {
      *channel = value.round() as u8;
    }
  }
}

/// A Gaussian blur of standard deviation `sigma` pixels over a `width` by
/// `height` premultiplied picture, as three box blurs each way, the
/// approximation the Filter Effects spec allows. Pixels past an edge
/// repeat the edge pixel, so the picture's edge stays as opaque as it was.
pub(crate) fn blur(data: &mut [u8], width: usize, height: usize, sigma: f32) {
  if width == 0 || height == 0 || sigma.is_nan() || sigma <= 0.0 {
    return;
  }
  // Past the longer side, every pixel is the same edge-weighted average,
  // and box sums stay well inside a u32.
  let sigma = sigma.min(width.max(height) as f32);
  let mut scratch = vec![0; data.len()];
  for radius in box_radii(sigma) {
    if radius == 0 {
      continue;
    }
    rows(data, &mut scratch, width, height, radius);
    columns(&scratch, data, width, height, radius);
  }
  // Rounding in each pass may leave a channel a step above its alpha.
  for pixel in data.as_chunks_mut::<4>().0 {
    let alpha = pixel[3];
    for channel in &mut pixel[..3] {
      *channel = (*channel).min(alpha);
    }
  }
}

/// The radii of three box blurs whose sum is close to a Gaussian of
/// `sigma`: boxes of two odd widths whose variances add up to sigma².
fn box_radii(sigma: f32) -> [usize; 3] {
  let variance = 12.0 * sigma * sigma;
  let ideal = (variance / 3.0 + 1.0).sqrt();
  let mut lower = ideal.floor() as usize;
  if lower.is_multiple_of(2) {
    lower = lower.saturating_sub(1);
  }
  let lower = lower.max(1);
  let upper = lower + 2;
  let low = lower as f32;
  // How many of the three take the lower width.
  let narrow = ((variance - 3.0 * low * low - 12.0 * low - 9.0) / (-4.0 * low - 4.0))
    .round()
    .clamp(0.0, 3.0) as usize;
  std::array::from_fn(|index| {
    let size = if index < narrow { lower } else { upper };
    (size - 1) / 2
  })
}

/// Blurs each row of `source` into `target` with a box `2 × radius + 1`
/// wide.
fn rows(source: &[u8], target: &mut [u8], width: usize, height: usize, radius: usize) {
  let average = Average::new(2 * radius + 1);
  let last = width - 1;
  for y in 0..height {
    let row = &source[y * width * 4..(y + 1) * width * 4];
    let out = &mut target[y * width * 4..(y + 1) * width * 4];
    // The window from -radius to radius; past an edge, the edge repeats.
    let mut sum = [0_u32; 4];
    for x in 0..=2 * radius {
      let x = x.saturating_sub(radius).min(last);
      for channel in 0..4 {
        sum[channel] += u32::from(row[x * 4 + channel]);
      }
    }
    for x in 0..width {
      for channel in 0..4 {
        out[x * 4 + channel] = average.of(sum[channel]);
      }
      let enter = (x + radius + 1).min(last) * 4;
      let leave = x.saturating_sub(radius) * 4;
      for channel in 0..4 {
        sum[channel] =
          sum[channel] + u32::from(row[enter + channel]) - u32::from(row[leave + channel]);
      }
    }
  }
}

/// Blurs each column of `source` into `target`, a row at a time so memory
/// is read in order.
fn columns(source: &[u8], target: &mut [u8], width: usize, height: usize, radius: usize) {
  let average = Average::new(2 * radius + 1);
  let stride = width * 4;
  let last = height - 1;
  let row = |y: usize| &source[y * stride..(y + 1) * stride];
  let mut sums = vec![0_u32; stride];
  for y in 0..=2 * radius {
    let y = y.saturating_sub(radius).min(last);
    for (total, &value) in sums.iter_mut().zip(row(y)) {
      *total += u32::from(value);
    }
  }
  for y in 0..height {
    let out = &mut target[y * stride..(y + 1) * stride];
    for (value, &total) in out.iter_mut().zip(&sums) {
      *value = average.of(total);
    }
    let (enter, leave) = (
      row((y + radius + 1).min(last)),
      row(y.saturating_sub(radius)),
    );
    for ((total, &add), &take) in sums.iter_mut().zip(enter).zip(leave) {
      *total = *total + u32::from(add) - u32::from(take);
    }
  }
}

/// Divides a box's sum by its size, rounded, with a multiply instead of a
/// division.
struct Average {
  factor: u64,
}

impl Average {
  fn new(size: usize) -> Average {
    let size = size as u64;
    Average {
      factor: ((1 << 32) + size / 2) / size,
    }
  }

  fn of(&self, sum: u32) -> u8 {
    ((u64::from(sum) * self.factor + (1 << 31)) >> 32) as u8
  }
}

/// `a × b / 255`, rounded.
fn mul(a: u8, b: u8) -> u8 {
  ((u16::from(a) * u16::from(b) + 127) / 255) as u8
}
