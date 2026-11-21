//! What a window's draws keep for the next: the layer of a subtree that
//! only fades or moves, blended in again through its new opacity and
//! transform instead of drawn again.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crabshell_ui::Affine;
use tiny_skia::Pixmap;

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
}

impl std::fmt::Debug for Kept {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let layers = self.seen.values().filter(|seen| seen.layer.is_some());
    let patches = self.patches.len();
    write!(
      f,
      "Kept {{ layers: {}, patches: {patches} }}",
      layers.count()
    )
  }
}

impl Kept {
  /// Starts a draw, counting what it keeps against `budget`. A layer
  /// drawn before the icon theme's `theme`th change is dropped.
  pub(super) fn start(&mut self, budget: &Arc<AtomicU64>, theme: u64) {
    self.frame += 1;
    self.budget = Arc::clone(budget);
    if self.theme != theme {
      self.theme = theme;
      self.seen.clear();
    }
  }

  /// Ends a draw: what it didn't visit is dropped.
  pub(super) fn end(&mut self) {
    let frame = self.frame;
    self.seen.retain(|_, seen| seen.frame == frame);
    self.patches.retain(|_, (used, _)| *used == frame);
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
