//! The main colors of an image, by median cut, as for a theme taken from a
//! wallpaper.

use std::cmp::Reverse;
use std::ops::Range;
use std::path::Path;

use crabshell_ui::{Color, Fit};

use crate::icons::halve;
use crate::images::{self, Picture};

/// The deepest cut: 2^8 boxes, 256 colors.
const MAX_DEPTH: u8 = 8;

/// The main colors of the image file at `path`, most common first.
///
/// The file is decoded as [`Image`](crabshell_ui::Image) decodes it, under
/// the same caps, then halved until its longer side is at most `size`
/// pixels; 0 keeps its size. Its pixels are split by median cut into up to
/// 2^`depth` boxes, with `depth` clamped to 0..=8: each time, the box with
/// the widest range on one channel is split at its median on that channel.
/// Each box gives the mean of its pixels, opaque. Pixels with alpha under
/// 128 are skipped. Boxes with as many pixels as each other go by their
/// color's bytes, so a result is the same on every run. An image with
/// fewer distinct colors than boxes gives each of them once.
///
/// Decoding a wallpaper takes tens of milliseconds, so call it off the
/// loop's thread.
///
/// # Errors
///
/// If the file is missing or isn't a regular file, isn't a PNG, JPEG,
/// GIF, WebP or SVG, is past the caps, or doesn't decode.
pub fn quantize_colors(path: &Path, depth: u8, size: u32) -> Result<Vec<Color>, String> {
  let image = match images::decode(path)? {
    Picture::Pixels(mut image) => {
      while size > 0 && image.width().max(image.height()) > size {
        image = halve(&image)?;
      }
      image
    }
    Picture::Svg(tree) => {
      let (width, height) = (tree.size().width(), tree.size().height());
      let shrink = match size {
        0 => 1.0,
        size => (size as f32 / width.max(height)).min(1.0),
      };
      let (width, height) = (
        (width * shrink).ceil() as u32,
        (height * shrink).ceil() as u32,
      );
      images::check(width, height)?;
      images::svg(&tree, width, height, Fit::Fill)?
    }
  };
  let pixels = image
    .pixels()
    .iter()
    .filter(|pixel| pixel.alpha() >= 128)
    .map(|pixel| {
      let color = pixel.demultiply();
      [color.red(), color.green(), color.blue()]
    })
    .collect();
  Ok(median_cut(pixels, depth))
}

/// A box of the cut: a run of the pixels, and its widest channel.
struct Cut {
  run: Range<usize>,
  channel: usize,
  /// How far that channel's values reach: 0 for a box of one color.
  spread: u8,
}

impl Cut {
  fn new(pixels: &[[u8; 3]], run: Range<usize>) -> Cut {
    let (mut low, mut high) = ([u8::MAX; 3], [0; 3]);
    for pixel in &pixels[run.clone()] {
      for channel in 0..3 {
        low[channel] = low[channel].min(pixel[channel]);
        high[channel] = high[channel].max(pixel[channel]);
      }
    }
    let spread = |channel: usize| high[channel].saturating_sub(low[channel]);
    let channel = (0..3).max_by_key(|&channel| spread(channel)).unwrap_or(0);
    Cut {
      run,
      channel,
      spread: spread(channel),
    }
  }
}

fn median_cut(mut pixels: Vec<[u8; 3]>, depth: u8) -> Vec<Color> {
  let mut cuts = Vec::new();
  if !pixels.is_empty() {
    cuts.push(Cut::new(&pixels, 0..pixels.len()));
  }
  while cuts.len() < 1 << depth.min(MAX_DEPTH) {
    // None left to split means each box holds one color.
    let Some(index) = (0..cuts.len())
      .filter(|&index| cuts[index].spread > 0)
      .max_by_key(|&index| cuts[index].spread)
    else {
      break;
    };
    let (run, channel) = (cuts[index].run.clone(), cuts[index].channel);
    let values = &mut pixels[run.clone()];
    values.sort_unstable_by_key(|pixel| pixel[channel]);
    let median = values[values.len() / 2][channel];
    // Equal values stay on one side, so no color is split between boxes.
    let mut split = values.partition_point(|pixel| pixel[channel] < median);
    if split == 0 {
      split = values.partition_point(|pixel| pixel[channel] <= median);
    }
    let middle = run.start + split;
    cuts[index] = Cut::new(&pixels, run.start..middle);
    cuts.push(Cut::new(&pixels, middle..run.end));
  }
  let mut colors: Vec<(usize, Color)> = cuts
    .iter()
    .map(|cut| {
      let values = &pixels[cut.run.clone()];
      let count = values.len() as u64;
      let mut sums = [0_u64; 3];
      for pixel in values {
        for (sum, &value) in sums.iter_mut().zip(pixel) {
          *sum += u64::from(value);
        }
      }
      let [r, g, b] = sums.map(|sum| ((sum + count / 2) / count) as u8);
      (values.len(), Color::rgb(r, g, b))
    })
    .collect();
  colors.sort_by_key(|&(count, color)| (Reverse(count), [color.r, color.g, color.b]));
  colors.into_iter().map(|(_, color)| color).collect()
}
