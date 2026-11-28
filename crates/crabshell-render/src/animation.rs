//! Animated GIFs: each frame composed on the GIF's canvas as its disposal
//! says, then scaled and filtered as a still image is, under a cap on
//! frames times size.

use std::io::Cursor;
use std::num::NonZeroU64;
use std::path::Path;
use std::time::Duration;

use crabshell_ui::{Fit, ImageFilters};
use gif::DisposalMethod;
use tiny_skia::Pixmap;

use crate::icons::{premultiplied, read_capped};
use crate::images::{MAX_BYTES, MAX_PIXELS, Picture, check, filtered, finish};

/// The most canvas pixels a GIF's frames may come to together: each is
/// composed on the canvas on the loop's thread at its first draw, so this
/// bounds that work. Past it, the first frame shows.
const MAX_DECODED_PIXELS: u64 = 64 * 1024 * 1024;

/// The most pixels at the size drawn one GIF's frames may keep, 32 MiB,
/// within what the picture cache has left. Past it, the first frame shows.
const MAX_KEPT_PIXELS: u64 = 8 * 1024 * 1024;

/// What an image file draws as: one picture, or a GIF's frames.
pub(crate) struct Frames {
  pub(crate) frames: Vec<Frame>,
  /// How many more times a GIF plays after its first, `None` for ever.
  pub(crate) repeats: Option<u32>,
}

impl Frames {
  pub(crate) fn still(pixmap: Pixmap) -> Frames {
    Frames {
      frames: vec![Frame {
        pixmap,
        delay: Duration::ZERO,
      }],
      repeats: Some(0),
    }
  }
}

/// One frame, scaled and filtered, and how long it shows.
pub(crate) struct Frame {
  pub(crate) pixmap: Pixmap,
  pub(crate) delay: Duration,
}

/// The file at `path` as `filtered` draws it, every frame of a GIF so,
/// keeping at most `room` bytes of frames. A GIF past that,
/// [`MAX_KEPT_PIXELS`] or [`MAX_DECODED_PIXELS`] shows its first frame,
/// and one with a broken frame stops on the last good one; both are
/// logged. A blurred GIF shows its first frame, still.
pub(crate) fn frames(
  path: &Path,
  size: (u32, u32),
  fit: Fit,
  filters: &ImageFilters,
  scale: f32,
  room: usize,
) -> Result<Frames, String> {
  let limits = Limits {
    work: MAX_DECODED_PIXELS,
    kept: MAX_KEPT_PIXELS.min(room as u64 / 4),
  };
  frames_within(path, size, fit, filters, scale, limits)
}

/// How many canvas pixels a GIF may decode and how many pixels at the
/// size drawn its frames may keep.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
  pub(crate) work: u64,
  pub(crate) kept: u64,
}

/// [`frames`] within `limits`.
pub(crate) fn frames_within(
  path: &Path,
  size: (u32, u32),
  fit: Fit,
  filters: &ImageFilters,
  scale: f32,
  limits: Limits,
) -> Result<Frames, String> {
  let is_gif = crate::images::extension(path).as_deref() == Some("gif");
  // The blur is for still pictures: blurring every frame would stall the
  // first draw.
  if !is_gif || filters.blur > 0.0 {
    return filtered(path, size, fit, filters, scale).map(Frames::still);
  }
  if size.0 == 0 || size.1 == 0 {
    return Err(String::from("an image with no pixels"));
  }
  let data = read_capped(path, MAX_BYTES)?;
  let mut gif = Gif::open(&data)?;
  let canvas_pixels = u64::from(gif.width) * u64::from(gif.height);
  let drawn = u64::from(size.0) * u64::from(size.1);
  let mut frames = Vec::new();
  let mut broken = false;
  loop {
    let (canvas, delay) = match gif.next_frame() {
      Ok(Some(frame)) => frame,
      Ok(None) => break,
      Err(error) if frames.is_empty() => return Err(error),
      Err(error) => {
        tracing::warn!(image = %path.display(), %error, "a GIF stops at a broken frame");
        broken = true;
        break;
      }
    };
    // Counted once a frame is read, so a GIF that ends in time isn't cut;
    // at most one more canvas is read past the limit.
    let count = frames.len() as u64 + 1;
    let past = if count.saturating_mul(canvas_pixels) > limits.work {
      Some(("decoding", limits.work))
    } else if count.saturating_mul(drawn) > limits.kept {
      Some(("keeping", limits.kept))
    } else {
      None
    };
    if let Some((what, limit)) = past.filter(|_| count > 1) {
      tracing::warn!(
        image = %path.display(),
        "{what} a GIF's frames at this size is past {limit} pixels, so it shows its first"
      );
      frames.truncate(1);
      break;
    }
    let pixmap = finish(&Picture::Pixels(canvas), size, fit, filters, scale)?;
    frames.push(Frame { pixmap, delay });
  }
  let repeats = match gif.decoder.repeat() {
    _ if broken || frames.len() < 2 => Some(0),
    gif::Repeat::Infinite => None,
    gif::Repeat::Finite(count) => Some(u32::from(count)),
  };
  Ok(Frames { frames, repeats })
}

/// A GIF being decoded, with its canvas as the frames so far left it.
pub(crate) struct Gif<'a> {
  decoder: gif::Decoder<Cursor<&'a [u8]>>,
  width: u32,
  height: u32,
  /// Straight RGBA, as the decoder gives it.
  canvas: Vec<u8>,
}

impl<'a> Gif<'a> {
  /// Reads the GIF's header, refusing one past the image caps.
  pub(crate) fn open(data: &'a [u8]) -> Result<Gif<'a>, String> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    // A frame past the canvas is an error, before its pixels are read.
    options.check_frame_consistency(true);
    if let Some(limit) = NonZeroU64::new(MAX_PIXELS * 4) {
      options.set_memory_limit(gif::MemoryLimit::Bytes(limit));
    }
    let decoder = options
      .read_info(Cursor::new(data))
      .map_err(|error| error.to_string())?;
    let (width, height) = (u32::from(decoder.width()), u32::from(decoder.height()));
    check(width, height)?;
    Ok(Gif {
      decoder,
      width,
      height,
      canvas: vec![0; width as usize * height as usize * 4],
    })
  }

  /// The next frame on the whole canvas and how long it shows, or `None`
  /// after the last. A delay under 20 ms counts as 100 ms, as browsers do.
  pub(crate) fn next_frame(&mut self) -> Result<Option<(Pixmap, Duration)>, String> {
    let Some(frame) = self
      .decoder
      .read_next_frame()
      .map_err(|error| error.to_string())?
    else {
      return Ok(None);
    };
    if frame.width == 0 || frame.height == 0 {
      return Err(String::from("a GIF frame with no pixels"));
    }
    let (left, top) = (u32::from(frame.left), u32::from(frame.top));
    // The decoder checks this too; the slices below rely on it.
    if left + u32::from(frame.width) > self.width || top + u32::from(frame.height) > self.height {
      return Err(String::from("a GIF frame past its canvas"));
    }
    let columns = usize::from(frame.width);
    let previous = (frame.dispose == DisposalMethod::Previous).then(|| self.canvas.clone());
    let mut rows = Vec::new();
    for (row, line) in frame.buffer.chunks_exact(columns * 4).enumerate() {
      let y = top + row as u32;
      let start = (y as usize * self.width as usize + left as usize) * 4;
      rows.push(start);
      let target = &mut self.canvas[start..start + columns * 4];
      // A clear pixel leaves what the frames before put there.
      for (pixel, source) in target
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(line.as_chunks::<4>().0)
      {
        if source[3] != 0 {
          *pixel = *source;
        }
      }
    }
    let delay = match frame.delay {
      0 | 1 => Duration::from_millis(100),
      delay => Duration::from_millis(u64::from(delay) * 10),
    };
    let dispose = frame.dispose;
    let shown = premultiplied(self.canvas.clone(), self.width, self.height)?;
    match dispose {
      DisposalMethod::Background => {
        for start in rows {
          self.canvas[start..start + columns * 4].fill(0);
        }
      }
      DisposalMethod::Previous => {
        if let Some(previous) = previous {
          self.canvas = previous;
        }
      }
      DisposalMethod::Any | DisposalMethod::Keep => {}
    }
    Ok(Some((shown, delay)))
  }
}
