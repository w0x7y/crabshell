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

/// The most pixels a GIF's frames may hold together, counting each frame
/// at the larger of the GIF's canvas and the size drawn: 64 Mi, 256 MiB
/// at most. Every frame is composed and scaled on the loop's thread and
/// kept while shown, so this bounds both the first draw's work and the
/// memory kept; it still takes a 480 by 270 sticker of 500 frames, or a
/// 128 by 128 avatar of 4,000. Past it, the first frame shows.
const MAX_FRAME_PIXELS: u64 = 64 * 1024 * 1024;

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

/// The file at `path` as `filtered` draws it, every frame of a GIF so.
/// A GIF past [`MAX_FRAME_PIXELS`] shows its first frame, and one with a
/// broken frame stops on the last good one; both are logged.
pub(crate) fn frames(
  path: &Path,
  size: (u32, u32),
  fit: Fit,
  filters: &ImageFilters,
  scale: f32,
) -> Result<Frames, String> {
  frames_within(path, size, fit, filters, scale, MAX_FRAME_PIXELS)
}

/// [`frames`] with a cap of `cap` pixels instead, for tests.
pub(crate) fn frames_within(
  path: &Path,
  size: (u32, u32),
  fit: Fit,
  filters: &ImageFilters,
  scale: f32,
  cap: u64,
) -> Result<Frames, String> {
  let is_gif = path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"));
  if !is_gif {
    return filtered(path, size, fit, filters, scale).map(Frames::still);
  }
  if size.0 == 0 || size.1 == 0 {
    return Err(String::from("an image with no pixels"));
  }
  let data = read_capped(path, MAX_BYTES)?;
  let mut gif = Gif::open(&data)?;
  let canvas = u64::from(gif.width) * u64::from(gif.height);
  let each = canvas.max(u64::from(size.0) * u64::from(size.1));
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
    if !frames.is_empty() && (frames.len() as u64 + 1) * each > cap {
      tracing::warn!(
        image = %path.display(),
        "a GIF's frames are past {cap} pixels at this size, so it shows its first"
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
    let frame_width = usize::from(frame.width);
    let previous = (frame.dispose == DisposalMethod::Previous).then(|| self.canvas.clone());
    // The part of the frame on the canvas: rows and columns past it are
    // cut off.
    let columns = frame_width.min(self.width.saturating_sub(left) as usize);
    let mut rows = Vec::new();
    for (row, line) in frame.buffer.chunks_exact(frame_width * 4).enumerate() {
      let y = top + row as u32;
      if y >= self.height {
        break;
      }
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
