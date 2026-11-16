//! Damage tracking: which pixels of a buffer to repaint, and which to report
//! to the compositor.
//!
//! A window's damage arrives in logical pixels. It is turned into physical
//! pixels, rounding outward, and overlapping rectangles are merged so that
//! no pixel is repainted twice. Each buffer remembers the frame it last
//! held, and a window's [`DamageHistory`] remembers what changed in recent
//! frames, so a buffer that skipped frames also gets those areas repainted.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use crabshell_ui::Rect;

/// How many frames of damage a history keeps. A window has two or three
/// buffers, so a buffer reused at all is at most this many frames behind.
const HISTORY: usize = 4;

/// Physical pixels added around every damaged rectangle, for antialiased
/// edges and glyphs that reach a pixel past the area layout gave them.
const MARGIN: u32 = 1;

/// A rectangle of buffer pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PixelRect {
  /// The left edge.
  pub x: u32,
  /// The top edge.
  pub y: u32,
  /// The width.
  pub width: u32,
  /// The height.
  pub height: u32,
}

impl PixelRect {
  /// A rectangle with its top-left corner at `x`, `y`.
  pub const fn new(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
    PixelRect {
      x,
      y,
      width,
      height,
    }
  }

  /// The right edge, one past the last column.
  pub fn right(&self) -> u32 {
    self.x + self.width
  }

  /// The bottom edge, one past the last row.
  pub fn bottom(&self) -> u32 {
    self.y + self.height
  }

  /// The number of pixels.
  pub fn area(&self) -> u64 {
    u64::from(self.width) * u64::from(self.height)
  }

  /// Whether the two share any pixel.
  pub fn overlaps(&self, other: &PixelRect) -> bool {
    self.x < other.right()
      && other.x < self.right()
      && self.y < other.bottom()
      && other.y < self.bottom()
  }

  /// The smallest rectangle covering both.
  fn union(&self, other: &PixelRect) -> PixelRect {
    let x = self.x.min(other.x);
    let y = self.y.min(other.y);
    PixelRect::new(
      x,
      y,
      self.right().max(other.right()) - x,
      self.bottom().max(other.bottom()) - y,
    )
  }
}

/// Converts a logical rectangle to the buffer pixels it touches at `scale`,
/// rounding outward and adding [`MARGIN`], clipped to a `width` by `height`
/// buffer. `None` if nothing is left.
pub(crate) fn to_pixels(rect: Rect, scale: f32, width: u32, height: u32) -> Option<PixelRect> {
  if !(rect.width > 0.0 && rect.height > 0.0) {
    return None;
  }
  // `as` saturates: negative values become 0, huge ones the maximum.
  let left = ((rect.x * scale).floor() as u32).saturating_sub(MARGIN);
  let top = ((rect.y * scale).floor() as u32).saturating_sub(MARGIN);
  let right = ((rect.right() * scale).ceil() as u32)
    .saturating_add(MARGIN)
    .min(width);
  let bottom = ((rect.bottom() * scale).ceil() as u32)
    .saturating_add(MARGIN)
    .min(height);
  (left < right && top < bottom).then(|| PixelRect::new(left, top, right - left, bottom - top))
}

/// Merges overlapping rectangles into their bounding boxes until no two
/// overlap, so every pixel is in at most one. Rectangles that only touch
/// stay apart.
pub(crate) fn merge(mut rects: Vec<PixelRect>) -> Vec<PixelRect> {
  rects.retain(|rect| rect.area() > 0);
  let mut merged = true;
  while merged {
    merged = false;
    'outer: for i in 0..rects.len() {
      for j in i + 1..rects.len() {
        if rects[i].overlaps(&rects[j]) {
          let other = rects.swap_remove(j);
          rects[i] = rects[i].union(&other);
          merged = true;
          break 'outer;
        }
      }
    }
  }
  rects
}

/// The frame a buffer last held. Keep one next to each buffer, starting
/// with [`BufferAge::default`] for a new one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferAge {
  /// The history and the number of the frame last drawn into the buffer.
  drawn: Option<(u64, u64)>,
}

/// Gives every history its own id, so a buffer's age is only trusted by
/// the history that drew it.
static HISTORIES: AtomicU64 = AtomicU64::new(0);

/// What changed in one window's recent frames. Keep one per window.
#[derive(Debug)]
pub struct DamageHistory {
  id: u64,
  /// The number the next frame gets.
  next: u64,
  /// The buffer size and scale of the last frame. Another size or scale
  /// changes every pixel.
  shape: Option<(u32, u32, u32)>,
  /// The damage of recent frames, oldest first.
  recent: VecDeque<(u64, Vec<PixelRect>)>,
}

/// What one frame repaints and reports.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan {
  /// The areas to repaint, none overlapping: this frame's damage and
  /// whatever changed in frames the buffer missed.
  pub(crate) repaint: Vec<PixelRect>,
  /// The areas that differ from the last frame the compositor showed, for
  /// `wl_surface.damage_buffer`.
  pub(crate) report: Vec<PixelRect>,
}

impl Default for DamageHistory {
  fn default() -> Self {
    Self::new()
  }
}

impl DamageHistory {
  /// Names the history, and so its window, for the caches.
  pub(crate) fn id(&self) -> u64 {
    self.id
  }

  /// A history with no frames yet; the first frame repaints everything.
  pub fn new() -> DamageHistory {
    DamageHistory {
      id: HISTORIES.fetch_add(1, Ordering::Relaxed),
      next: 0,
      shape: None,
      recent: VecDeque::new(),
    }
  }

  /// Works out what a frame with logical `damage` repaints in a `width` by
  /// `height` buffer at `scale` that last held the frame in `age`, and
  /// records the frame as the buffer's.
  pub(crate) fn plan(
    &mut self,
    damage: &[Rect],
    scale: f32,
    width: u32,
    height: u32,
    age: &mut BufferAge,
  ) -> Plan {
    let full = vec![PixelRect::new(0, 0, width, height)];
    let frame = self.next;
    self.next += 1;

    let shape = (width, height, scale.to_bits());
    let mut changed = if self.shape == Some(shape) {
      merge(
        damage
          .iter()
          .filter_map(|rect| to_pixels(*rect, scale, width, height))
          .collect(),
      )
    } else {
      // A new size or scale changes every pixel, and earlier frames say
      // nothing about this one.
      self.shape = Some(shape);
      self.recent.clear();
      full.clone()
    };
    if more_than_half(&changed, width, height) {
      changed = full.clone();
    }

    // Frames the buffer missed, if it was this history's and the history
    // still has them all.
    let missed = match age.drawn {
      Some((history, drawn))
        if history == self.id
          && self
            .recent
            .front()
            .is_none_or(|(first, _)| *first <= drawn + 1) =>
      {
        Some(
          self
            .recent
            .iter()
            .filter(|(number, _)| *number > drawn)
            .flat_map(|(_, rects)| rects.iter().copied())
            .collect::<Vec<_>>(),
        )
      }
      _ => None,
    };
    let repaint = match missed {
      Some(missed) => {
        let repaint = merge(changed.iter().copied().chain(missed).collect());
        if more_than_half(&repaint, width, height) {
          full
        } else {
          repaint
        }
      }
      None => full,
    };

    self.recent.push_back((frame, changed.clone()));
    while self.recent.len() > HISTORY {
      self.recent.pop_front();
    }
    age.drawn = Some((self.id, frame));
    Plan {
      repaint,
      report: changed,
    }
  }
}

/// Whether non-overlapping `rects` cover more than half of the buffer, in
/// which case repainting all of it is simpler and about as fast.
fn more_than_half(rects: &[PixelRect], width: u32, height: u32) -> bool {
  let covered: u64 = rects.iter().map(PixelRect::area).sum();
  covered * 2 > u64::from(width) * u64::from(height)
}
