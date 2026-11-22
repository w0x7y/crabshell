//! What a window's draws keep for the next: the layer of a subtree that
//! only fades or moves, blended in again through its new opacity and
//! transform instead of drawn again.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crabshell_ui::Affine;
use tiny_skia::{Mask, Pixmap};

use super::patched::{Key, Patch};

/// The most bytes one renderer keeps for all its windows, 32 MiB: two
/// 1080p screens of layers. Past it, a layer is drawn as usual.
pub(crate) const KEPT_LIMIT: u64 = 32 << 20;

/// A window's kept layers, by widget path. Each lives only while every
/// draw of its window visits its node and draws it through a layer, so a
/// settled animation, a hidden widget or a closed window keeps nothing.
#[derive(Default)]
pub(crate) struct Kept {
  /// Counts this window's draws.
  pub(super) frame: u64,
  /// Counts the bytes kept against the renderer's [`KEPT_LIMIT`].
  pub(super) budget: Arc<AtomicU64>,
  /// The renderer's icon theme changes at the last draw.
  theme: u64,
  pub(super) seen: HashMap<String, Seen>,
  /// The small blurred boxes large shadows are drawn from, with the draw
  /// that last used each.
  pub(super) patches: HashMap<Key, (u64, Patch)>,
  /// Large buffers this draw let go of, for its next large layer or
  /// mask, each held against the budget: a fresh one costs a page fault
  /// every 4 KiB as it is first written. Dropped as the draw ends, so an
  /// idle window holds none.
  spare: Vec<(Vec<u8>, Held)>,
  /// How many large buffers came from the spare ones, for tests.
  #[cfg(test)]
  reused: usize,
}

/// The least bytes a buffer needs to be kept spare.
const SPARE_FROM: usize = 1 << 20;

/// The most bytes a draw keeps spare.
const SPARE_LIMIT: usize = 16 << 20;

impl std::fmt::Debug for Kept {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let layers = self.seen.values().filter(|seen| seen.layer.is_some());
    let patches = self.patches.len();
    write!(
      f,
      "Kept {{ layers: {}, patches: {patches}, spare: {} }}",
      layers.count(),
      self.spare.len()
    )
  }
}

impl Kept {
  /// Starts a draw, counting what it keeps against `budget`. A layer
  /// drawn before the icon theme's `theme`th change is dropped.
  pub(crate) fn start(&mut self, budget: &Arc<AtomicU64>, theme: u64) {
    self.frame += 1;
    self.budget = Arc::clone(budget);
    if self.theme != theme {
      self.theme = theme;
      self.seen.clear();
    }
  }

  /// Ends a draw: what it didn't visit is dropped, and every spare buffer.
  pub(crate) fn end(&mut self) {
    let frame = self.frame;
    self.seen.retain(|_, seen| seen.frame == frame);
    self.patches.retain(|_, (used, _)| *used == frame);
    self.spare.clear();
  }

  /// `bytes` zeroed bytes, from a spare buffer when they are many.
  pub(crate) fn buffer(&mut self, bytes: usize) -> Vec<u8> {
    let fits = (bytes >= SPARE_FROM)
      .then(|| {
        let spare = self.spare.iter().enumerate();
        let fitting = spare.filter(|(_, (data, _))| data.capacity() >= bytes);
        fitting
          .min_by_key(|(_, (data, _))| data.capacity())
          .map(|(index, _)| index)
      })
      .flatten();
    let Some(index) = fits else {
      return vec![0; bytes];
    };
    let (mut data, _held) = self.spare.swap_remove(index);
    #[cfg(test)]
    {
      self.reused += 1;
    }
    data.clear();
    // No larger than asked, so a layer holds only what it is charged for.
    data.shrink_to(bytes);
    data.resize(bytes, 0);
    data
  }

  /// Keeps `data` spare for [`buffer`](Self::buffer) until the draw
  /// ends, if it is large and there is room in the draw's limit and the
  /// renderer's budget.
  pub(crate) fn spare(&mut self, data: Vec<u8>) {
    let kept: usize = self.spare.iter().map(|(data, _)| data.capacity()).sum();
    if data.capacity() < SPARE_FROM || kept + data.capacity() > SPARE_LIMIT {
      return;
    }
    if let Some(held) = self.hold(data.capacity() as u64) {
      self.spare.push((data, held));
    }
  }

  /// How many large buffers came from spare ones, for tests.
  #[cfg(test)]
  pub(crate) fn reused(&self) -> usize {
    self.reused
  }

  /// How many buffers are spare and their bytes, for tests.
  #[cfg(test)]
  pub(crate) fn spare_buffers(&self) -> (usize, usize) {
    let bytes = self.spare.iter().map(|(data, _)| data.capacity()).sum();
    (self.spare.len(), bytes)
  }

  /// A clear `width` by `height` pixmap, from a spare buffer when large.
  pub(super) fn pixmap(&mut self, width: u32, height: u32) -> Option<Pixmap> {
    let size = tiny_skia::IntSize::from_wh(width, height)?;
    let bytes = usize::try_from(u64::from(width) * u64::from(height) * 4).ok()?;
    Pixmap::from_vec(self.buffer(bytes), size)
  }

  /// A clear `width` by `height` mask, from a spare buffer when large.
  pub(super) fn mask(&mut self, width: u32, height: u32) -> Option<Mask> {
    let size = tiny_skia::IntSize::from_wh(width, height)?;
    let bytes = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
    Mask::from_vec(self.buffer(bytes), size)
  }

  /// The small box a large shadow of `key` is drawn from, worked out on
  /// its first use, or `None` past the budget.
  pub(super) fn patch(&mut self, key: Key) -> Option<&Patch> {
    #[cfg(test)]
    if !PATCHES.get() {
      return None;
    }
    let frame = self.frame;
    if !self.patches.contains_key(&key) {
      let held = self.hold(Patch::bytes(&key))?;
      self.patches.insert(key, (frame, Patch::new(&key, held)));
    }
    let (used, patch) = self.patches.get_mut(&key)?;
    *used = frame;
    Some(patch)
  }

  /// The layer kept for the node at `path` in this draw.
  pub(super) fn layer(&self, path: &str) -> Option<&Layer> {
    self.seen.get(path)?.layer.as_ref()
  }

  /// Counts `bytes` against the budget, or `None` if they don't fit.
  pub(super) fn hold(&self, bytes: u64) -> Option<Held> {
    let total = self.budget.fetch_add(bytes, Ordering::Relaxed) + bytes;
    let held = Held {
      bytes,
      budget: Arc::clone(&self.budget),
    };
    (total <= KEPT_LIMIT).then_some(held)
  }
}

#[cfg(test)]
thread_local! {
  /// Whether this thread's draws draw large shadows from patches, which a
  /// test turns off to draw them pixel by pixel.
  pub(crate) static PATCHES: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// What a draw of a window saw of a node drawn through a layer for its
/// opacity or transform.
pub(super) struct Seen {
  /// The draw that saw it.
  pub(super) frame: u64,
  pub(super) revision: u64,
  pub(super) opacity: f32,
  pub(super) transform: Option<Affine>,
  /// The layer this draw blends in, if it keeps one.
  pub(super) layer: Option<Layer>,
}

/// A subtree's layer, kept.
pub(super) struct Layer {
  pub(super) pixmap: Pixmap,
  /// Where it is: its corner and size, in buffer pixels for an opacity
  /// layer, or in the node's own pixels, zoomed, for a transformed one.
  pub(super) area: Area,
  /// The pixel scale and zoom it was drawn at, which a reuse needs again.
  pub(super) at: (f32, f32),
  pub(super) _held: Held,
}

/// Where a layer goes in a draw: what of it shows in the window, all of
/// it, and the pixel scale and zoom it is drawn at.
pub(super) struct Want {
  pub(super) needed: Area,
  pub(super) whole: Area,
  pub(super) at: (f32, f32),
}

/// A rectangle of whole pixels: its corner and size.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Area {
  pub(super) x: i32,
  pub(super) y: i32,
  pub(super) width: u32,
  pub(super) height: u32,
}

impl Area {
  /// Whether `inner` lies within this one.
  pub(super) fn holds(&self, inner: &Area) -> bool {
    let right = |area: &Area| i64::from(area.x) + i64::from(area.width);
    let bottom = |area: &Area| i64::from(area.y) + i64::from(area.height);
    inner.x >= self.x
      && inner.y >= self.y
      && right(inner) <= right(self)
      && bottom(inner) <= bottom(self)
  }
}

/// Bytes counted against a renderer's budget while kept.
pub(super) struct Held {
  bytes: u64,
  budget: Arc<AtomicU64>,
}

impl Drop for Held {
  fn drop(&mut self) {
    self.budget.fetch_sub(self.bytes, Ordering::Relaxed);
  }
}
