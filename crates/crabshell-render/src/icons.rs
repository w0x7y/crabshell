//! Icons: SVG and PNG files and raw tray pixels, rasterized at the size
//! they are drawn at and cached.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use crabshell_ui::{
  CaptureFrame, Color, Fit, IconPixels, IconSource, ImageFilters, ImageStatus, ImageTransition,
  ImageView,
};
use resvg::usvg;
use tiny_skia::{FilterQuality, IntRect, Pixmap, PixmapPaint, PixmapRef, Transform};

use crate::animation::Frames;
use crate::theme::{IconLookup, Lookup, Themes};
use crate::worker::{Job, Worker};

/// How many rasterized icons stay cached. Past that, icons no draw used
/// within the last [`RECENT`] draws are dropped.
const CAPACITY: usize = 128;

/// How many draws an icon stays cached after its last use once the cache is
/// past [`CAPACITY`]. Several windows draw in turn, so this spans a few
/// frames of each.
const RECENT: u64 = 16;

/// How many bytes of pixels stay cached. Past that, as past [`CAPACITY`],
/// what no recent draw used is dropped: a slideshow's pictures add up.
const MAX_CACHED_BYTES: usize = 32 * 1024 * 1024;

/// The largest SVG file read, in bytes.
const MAX_SVG_BYTES: u64 = 1024 * 1024;

/// The largest PNG file read, in bytes.
const MAX_PNG_BYTES: u64 = 4 * 1024 * 1024;

/// The widest and tallest PNG decoded, in pixels.
const MAX_PNG_SIDE: u32 = 1024;

/// The widest and tallest raw tray pixmap drawn.
const MAX_PIXELS_SIDE: u32 = 512;

/// The largest image file decoded on the loop's thread once decoding has
/// a worker, with [`SMALL_PIXELS`]: an avatar or a small icon shows in the
/// first frame, and decodes in well under a millisecond.
const SMALL_FILE: u64 = 64 * 1024;

/// The most pixels such an image is drawn at, 256 by 256.
const SMALL_PIXELS: u64 = 256 * 256;

/// The theme icon names are looked up in when the config doesn't set one.
pub const DEFAULT_THEME: &str = "hicolor";

/// Rasterized icons, by source and size in physical pixels.
pub(crate) struct Icons {
  lookup: IconLookup,
  cache: HashMap<Key, Entry>,
  /// Counts draws, to tell which icons the latest one used.
  frame: u64,
  /// Sources that failed, so each is logged once.
  failed: HashSet<Key>,
  /// Mask images that failed, so each path is logged once, whatever the
  /// size its box takes.
  failed_masks: HashSet<PathBuf>,
  /// How many icons were rasterized, for tests.
  rasterized: usize,
  /// How many times an image's file was looked up, for tests.
  lookups: usize,
  /// Each screencopy view's frame, scaled to its box: one a view, replaced
  /// when its frame or box changes.
  captures: HashMap<u64, Capture>,
  /// Each animated image's place in its GIF, by image id.
  playbacks: HashMap<u64, Playback>,
  /// What each image with a transition shows, by image id.
  transitions: HashMap<u64, Shown>,
  /// Each image file's modification time and length, looked at once a
  /// draw rather than once a tile.
  modified: HashMap<PathBuf, (Option<std::time::SystemTime>, u64)>,
  /// The picture each image last drew as its own, and when.
  shows: HashMap<u64, (Key, u64)>,
  /// The damage history of the window being drawn.
  window: u64,
  /// The tile being drawn, by row and column.
  tile: (u32, u32),
  /// The pictures masks used, and in which draw, so a transition that
  /// stops leaving one doesn't drop it.
  masked: HashMap<Key, u64>,
  /// Decodes large image files off the loop's thread; `None` decodes
  /// each as it is first drawn.
  worker: Option<Worker>,
  /// The pictures the worker decodes: each one's job, and the images
  /// waiting for it.
  pending: HashMap<Key, (u64, Vec<u64>)>,
  /// Images whose picture came in, to repaint.
  ready: Vec<u64>,
  /// How many jobs the worker did, for tests.
  decoded_off_thread: usize,
  /// Whether the draw drew an image whose picture is being decoded.
  waited: bool,
  /// What the last still image drawn showed, so its other tiles in the
  /// same draw skip the lookups.
  memo: Option<Memo>,
}

/// The pictures an image showed in a draw: its own and the one its
/// transition leaves, as [`Icons::leaving`] gives it. A still picture
/// shown again in the same draw from the same view, at the same size, has
/// no more to record.
struct Memo {
  /// The draw, the image's id, its size in pixels and its scale's bits.
  at: (u64, u64, (u32, u32), u32),
  /// What else of the view picks the pictures: its file, the file its
  /// transition leaves, its fit, filters and transition.
  path: PathBuf,
  leaving: Option<PathBuf>,
  fit: Fit,
  filters: FilterKey,
  transition: ImageTransition,
  to: Key,
  from: Option<Option<Key>>,
}

impl Memo {
  fn new(at: (u64, u64, (u32, u32), u32), image: &ImageView<'_>, scale: f32, to: Key) -> Memo {
    Memo {
      at,
      path: image.path.to_owned(),
      leaving: image.leaving.map(|(path, _)| path.to_owned()),
      fit: image.fit,
      filters: FilterKey::new(&image.filters, scale),
      transition: image.transition,
      to,
      from: None,
    }
  }

  /// Whether `image`, drawn at `at` and `scale`, shows what this did.
  fn matches(&self, at: (u64, u64, (u32, u32), u32), image: &ImageView<'_>, scale: f32) -> bool {
    self.at == at
      && self.path.as_os_str() == image.path.as_os_str()
      && self.leaving.as_deref().map(Path::as_os_str)
        == image.leaving.map(|(path, _)| path.as_os_str())
      && self.fit == image.fit
      && self.filters == FilterKey::new(&image.filters, scale)
      && self.transition == image.transition
  }
}

/// What an image with a transition shows.
#[derive(Default)]
struct Shown {
  /// The last source that loaded, which one that can't falls back to.
  good: Option<PathBuf>,
  /// While a transition runs: the file it leaves, as the view names it,
  /// and the one it draws, the last good one when that can't be loaded.
  leaving: Option<(PathBuf, Option<PathBuf>)>,
  /// The picture it left at its last draw, held only while drawn.
  left: Option<Key>,
  used: u64,
  /// The damage history of the window that last drew it.
  window: u64,
}

/// What an image draws: its picture, or two in a transition.
pub(crate) enum Drawn<'a> {
  One(&'a Pixmap),
  /// The picture left, if any loaded, and the one coming in, `t` of the
  /// way, eased.
  Two {
    from: Option<&'a Pixmap>,
    to: &'a Pixmap,
    t: f32,
  },
}

/// Where an animated image is in its GIF.
struct Playback {
  /// The entry it plays.
  key: Key,
  frame: usize,
  /// How many times it has played through.
  played: u32,
  /// When the frame shown was due, so a late draw doesn't slow the GIF.
  since: Option<Instant>,
  /// When the next frame is due: set once a frame is drawn, cleared when
  /// it is due, so a GIF not drawn again waits for nothing.
  due: Option<Instant>,
  /// Whether a draw showed it since the last schedule.
  drawn: bool,
  playing: bool,
  /// Whether it stopped on its last frame.
  finished: bool,
  used: u64,
  /// The damage history of the window that last drew it, and the tiles
  /// it was drawn in then.
  window: u64,
  tiles: Vec<(u32, u32)>,
}

impl Playback {
  fn new(key: Key) -> Playback {
    Playback {
      key,
      frame: 0,
      played: 0,
      since: None,
      due: None,
      drawn: false,
      playing: true,
      finished: false,
      used: 0,
      window: 0,
      tiles: Vec::new(),
    }
  }
}

struct Capture {
  /// The frame scaled, by its shared allocation.
  frame: Shared,
  width: u32,
  height: u32,
  fit: Fit,
  crop: Option<IntRect>,
  /// `None` if the frame couldn't be scaled; drawn as nothing.
  pixmap: Option<Pixmap>,
  used: u64,
}

/// What an icon was rasterized from, and at what size.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
  /// A name, with the logical size and whole scale it was looked up at.
  Name {
    theme: String,
    name: String,
    size: u32,
    scale: u32,
    side: u32,
    tint: Option<Color>,
  },
  File {
    path: PathBuf,
    side: u32,
    tint: Option<Color>,
  },
  /// An image file, scaled to `width` by `height` pixels, as it was when
  /// last modified: a file rewritten in place, or one that appears after a
  /// failure, is read again.
  Image {
    path: PathBuf,
    modified: Option<std::time::SystemTime>,
    width: u32,
    height: u32,
    fit: Fit,
    filters: FilterKey,
  },
  /// Raw pixels, by their shared allocation: reading the bytes on every
  /// draw would cost as much as rasterizing them.
  Pixels {
    data: Shared,
    width: u32,
    height: u32,
    side: u32,
    tint: Option<Color>,
  },
}

/// An image's filters as a cache key: the tint and the bits of each
/// number, the blur's in buffer pixels, since that is what it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FilterKey {
  tint: Option<Color>,
  numbers: [u32; 4],
}

impl FilterKey {
  fn new(filters: &ImageFilters, scale: f32) -> FilterKey {
    FilterKey {
      tint: filters.tint,
      numbers: [
        filters.brightness,
        filters.contrast,
        filters.saturation,
        filters.blur * scale,
      ]
      .map(f32::to_bits),
    }
  }
}

/// Tray pixel data, compared and hashed by its address. The key holds a
/// reference, so the address can't be reused by other data while cached.
#[derive(Clone)]
struct Shared(Rc<[u8]>);

impl PartialEq for Shared {
  fn eq(&self, other: &Shared) -> bool {
    Rc::ptr_eq(&self.0, &other.0)
  }
}

impl Eq for Shared {}

impl Hash for Shared {
  fn hash<H: Hasher>(&self, state: &mut H) {
    (Rc::as_ptr(&self.0) as *const u8 as usize, self.0.len()).hash(state);
  }
}

impl std::fmt::Debug for Shared {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{} bytes", self.0.len())
  }
}

struct Entry {
  /// `None` if the icon couldn't be loaded; drawn as nothing.
  frames: Option<Frames>,
  used: u64,
}

impl Key {
  /// Whether both are the same image file, as last modified.
  fn same_file(&self, other: &Key) -> bool {
    match (self, other) {
      (
        Key::Image { path, modified, .. },
        Key::Image {
          path: other_path,
          modified: other_modified,
          ..
        },
      ) => (path, modified) == (other_path, other_modified),
      _ => false,
    }
  }
}

impl Icons {
  /// Icons whose names are looked up in the environment's base
  /// directories.
  pub(crate) fn new() -> Icons {
    Icons::with_themes(Themes::from_env())
  }

  pub(crate) fn with_themes(themes: Themes) -> Icons {
    Icons {
      lookup: IconLookup::new(themes),
      cache: HashMap::new(),
      frame: 0,
      failed: HashSet::new(),
      failed_masks: HashSet::new(),
      rasterized: 0,
      lookups: 0,
      captures: HashMap::new(),
      playbacks: HashMap::new(),
      transitions: HashMap::new(),
      modified: HashMap::new(),
      shows: HashMap::new(),
      window: 0,
      tile: (0, 0),
      masked: HashMap::new(),
      worker: None,
      pending: HashMap::new(),
      ready: Vec::new(),
      decoded_off_thread: 0,
      waited: false,
      memo: None,
    }
  }

  /// Whether the last draw drew an image whose picture is being decoded.
  pub(crate) fn waited(&self) -> bool {
    self.waited
  }

  /// How many jobs the worker did, and whether it runs, for tests.
  #[cfg(test)]
  pub(crate) fn worker_work(&self) -> (usize, bool) {
    let running = self.worker.as_ref().is_some_and(Worker::running);
    (self.decoded_off_thread, running)
  }

  /// Decodes image files past [`SMALL_FILE`] or [`SMALL_PIXELS`], or
  /// blurred, on a worker thread from now on.
  pub(crate) fn decode_off_thread(&mut self) {
    self.worker.get_or_insert_with(Worker::default);
  }

  /// Whether pictures are being decoded for images drawn, or came in and
  /// weren't taken yet.
  pub(crate) fn decoding(&self) -> bool {
    !self.pending.is_empty() || !self.ready.is_empty()
  }

  /// Calls `wake` once, from the worker, when a picture is decoded, or at
  /// once if one waits to be taken.
  pub(crate) fn wake_when_decoded(&mut self, wake: Box<dyn FnOnce() + Send>) {
    match &mut self.worker {
      Some(worker) if self.ready.is_empty() => worker.wake_when_done(wake),
      _ => wake(),
    }
  }

  /// Caches the pictures decoded since the last call, logging a failure
  /// once, and returns the ids of the images that wait for them.
  pub(crate) fn take_decoded(&mut self) -> Vec<u64> {
    let mut ready = std::mem::take(&mut self.ready);
    let done = self.worker.as_mut().map(Worker::take).unwrap_or_default();
    for (job, frames) in done {
      self.decoded_off_thread += 1;
      // A job taken over by a draw that needed its picture at once.
      let Some(key) = self
        .pending
        .iter()
        .find(|(_, (number, _))| *number == job)
        .map(|(key, _)| key.clone())
      else {
        continue;
      };
      let images = self
        .pending
        .remove(&key)
        .map(|(_, images)| images)
        .unwrap_or_default();
      ready.extend(&images);
      let source = match &key {
        Key::Image { path, .. } => Some(path.clone()),
        _ => None,
      };
      let path = source
        .as_ref()
        .map_or_else(String::new, |path| path.display().to_string());
      let mut frames = frames;
      // Each job had the room left when it was sent, so GIFs decoded
      // together could keep more than the cache holds.
      if let Ok(gif) = &mut frames
        && gif.frames.len() > 1
      {
        let bytes: usize = gif
          .frames
          .iter()
          .map(|frame| frame.pixmap.data().len())
          .sum();
        if bytes > MAX_CACHED_BYTES.saturating_sub(self.bytes_in_use()) {
          tracing::warn!(
            image = %path,
            "keeping a GIF's frames is past what the picture cache has left, so it shows its first"
          );
          gif.frames.truncate(1);
          gif.repeats = Some(0);
        }
      }
      let status = match &frames {
        Ok(_) => ImageStatus::Ready,
        Err(_) => ImageStatus::Failed,
      };
      self.store(key, frames, || format!("image {path}"));
      // Told now, as an image that isn't drawn would never tell it.
      if let Some(source) = &source {
        for &id in &images {
          crabshell_ui::image_status(id, source, status);
        }
      }
    }
    ready.sort_unstable();
    ready.dedup();
    ready
  }

  /// Starts a draw of the window whose damage history is `window`.
  pub(crate) fn start_frame(&mut self, window: u64) {
    self.window = window;
    self.waited = false;
  }

  /// Starts drawing the tile at `row` and `column`.
  pub(crate) fn start_tile(&mut self, tile: (u32, u32)) {
    self.tile = tile;
  }

  /// Stops each GIF this window drew whose tiles, `repainted` by this
  /// draw, all went without it: hidden, under opacity 0, scrolled or
  /// clipped away. It waits for nothing until drawn again. One drawn in
  /// another window, or whose tiles weren't all repainted, keeps
  /// waiting. `repainted` is sorted.
  pub(crate) fn repainted(&mut self, repainted: &[(u32, u32)]) {
    let (window, frame) = (self.window, self.frame);
    for playback in self.playbacks.values_mut() {
      if playback.window == window
        && playback.used < frame
        && playback
          .tiles
          .iter()
          .all(|tile| repainted.binary_search(tile).is_ok())
      {
        playback.due = None;
        playback.drawn = false;
      }
    }
  }

  pub(crate) fn set_theme(&mut self, theme: String) {
    self.lookup.set_theme(theme);
  }

  pub(crate) fn lookup(&self) -> IconLookup {
    self.lookup.clone()
  }

  /// The number of rasterized icons held, for tests.
  #[cfg(test)]
  pub(crate) fn cached(&self) -> usize {
    self.cache.len()
  }

  /// The number of frames held over every cached picture, for tests.
  #[cfg(test)]
  pub(crate) fn frames_cached(&self) -> usize {
    self
      .cache
      .values()
      .filter_map(|entry| entry.frames.as_ref())
      .map(|frames| frames.frames.len())
      .sum()
  }

  /// Drops every cached picture and nothing else, for tests.
  #[cfg(test)]
  pub(crate) fn clear_cache(&mut self) {
    self.cache.clear();
  }

  /// How many times an image's file was looked up, for tests.
  #[cfg(test)]
  pub(crate) fn lookups(&self) -> usize {
    self.lookups
  }

  /// The number of times an icon was rasterized, for tests.
  #[cfg(test)]
  pub(crate) fn rasterized(&self) -> usize {
    self.rasterized
  }

  /// The number of screencopy frames scaled and kept, for tests.
  #[cfg(test)]
  pub(crate) fn captures(&self) -> usize {
    self.captures.len()
  }

  /// The icon `source` as a `side` by `side` pixmap painted in `tint`,
  /// rasterized on first use. A name is looked up for `size` logical
  /// pixels at `scale`, a whole number. `None` if it can't be loaded; that
  /// is logged once.
  pub(crate) fn get(
    &mut self,
    source: &IconSource,
    (side, size, scale): (u32, u32, u32),
    tint: Option<Color>,
  ) -> Option<&Pixmap> {
    let key = match source {
      IconSource::Name(name) => Key::Name {
        theme: self.lookup.borrow().theme.clone(),
        name: name.clone(),
        size,
        scale,
        side,
        tint,
      },
      IconSource::File(path) => Key::File {
        path: path.clone(),
        side,
        tint,
      },
      IconSource::Pixels(pixels) => Key::Pixels {
        data: Shared(Rc::clone(&pixels.data)),
        width: pixels.width,
        height: pixels.height,
        side,
        tint,
      },
    };
    if !self.cache.contains_key(&key) {
      let rasterized = match source {
        IconSource::Name(name) => {
          let mut lookup = self.lookup.borrow();
          let Lookup { theme, themes } = &mut *lookup;
          match themes.find(theme, name, size, scale) {
            Some(path) => rasterize_file(&path, side),
            None => Err(format!("no icon named {name:?} in theme {theme:?}")),
          }
        }
        IconSource::File(path) => rasterize_file(path, side),
        IconSource::Pixels(pixels) => from_argb(pixels).and_then(|image| fit(&image, side)),
      };
      let tinted = rasterized.map(|mut pixmap| {
        if let Some(tint) = tint {
          crate::filters::tint(pixmap.data_mut(), tint);
        }
        pixmap
      });
      self.store(key.clone(), tinted.map(Frames::still), || describe(source));
    }
    self.used(&key)
  }

  /// `image`'s file scaled to `size` in pixels as its fit says and run
  /// through its filters at `scale`, decoded on first use: for an
  /// animated GIF, the frame its playback is on, every frame decoded at
  /// once. While a transition runs, the picture it leaves too, at its
  /// first frame. With a transition other than none, a source that can't
  /// be loaded draws the last one that could, and a transition to it is
  /// told to end, through [`crabshell_ui::image_source_failed`]. Its
  /// status is told Ready or Failed, through
  /// [`crabshell_ui::image_status`], once it isn't decoded off the thread.
  /// The picture a transition
  /// leaves is held only while a draw uses it, and dropped as soon as the
  /// transition ends. `None` if nothing can be loaded; a failure is
  /// logged once.
  pub(crate) fn image(
    &mut self,
    image: &ImageView<'_>,
    size: (u32, u32),
    scale: f32,
  ) -> Option<Drawn<'_>> {
    let at = (self.frame, image.id, size, scale.to_bits());
    // A picture gone from the cache is looked up and decoded again below.
    let hit = self.memo.as_ref().is_some_and(|memo| {
      memo.matches(at, image, scale) && frame(&self.cache, &memo.to, 0).is_some()
    });
    if hit {
      let memo = self.memo.as_ref()?;
      let to = frame(&self.cache, &memo.to, 0)?;
      return Some(match (&memo.from, image.leaving) {
        (Some(from), Some((_, t))) => Drawn::Two {
          from: from.as_ref().and_then(|from| frame(&self.cache, from, 0)),
          to,
          t,
        },
        _ => Drawn::One(to),
      });
    }
    let key = self.load(image.path, image, size, scale, true, Some(image.id));
    if self.pending.contains_key(&key) {
      return self.waiting(image, &key, size, scale);
    }
    // A failure is marked used too, so it isn't read and logged again.
    let entry = self.cache.get_mut(&key)?;
    entry.used = self.frame;
    let count = entry
      .frames
      .as_ref()
      .map_or(0, |frames| frames.frames.len());
    let status = match count {
      0 => ImageStatus::Failed,
      _ => ImageStatus::Ready,
    };
    crabshell_ui::image_status(image.id, image.path, status);
    let index = if count > 1 {
      self.play(image, &key, count)
    } else {
      self.playbacks.remove(&image.id);
      0
    };
    if image.transition == ImageTransition::None {
      if let Some(left) = self
        .transitions
        .remove(&image.id)
        .and_then(|shown| shown.left)
      {
        self.forget(left, image.id, Some(&key));
      }
      self.show(image.id, &key);
      if count == 1 {
        self.memo = Some(Memo::new(at, image, scale, key.clone()));
      }
      return frame(&self.cache, &key, index).map(Drawn::One);
    }
    let from = self.leaving(image, &key, size, scale);
    let shown = self.transitions.entry(image.id).or_default();
    shown.used = self.frame;
    shown.window = self.window;
    if count == 0 {
      // Keeps the old picture, and ends the transition coming in.
      if image.leaving.is_some() {
        crabshell_ui::image_source_failed(image.id, image.path);
      }
      let good = shown.good.clone()?;
      let key = self.load(&good, image, size, scale, true, Some(image.id));
      self.show(image.id, &key);
      return frame(&self.cache, &key, 0).map(Drawn::One);
    }
    shown.good = Some(image.path.to_owned());
    self.show(image.id, &key);
    if count == 1 {
      self.memo = Some(Memo {
        from: from.clone(),
        ..Memo::new(at, image, scale, key.clone())
      });
    }
    let to = frame(&self.cache, &key, index)?;
    Some(match (from, image.leaving) {
      (Some(from), Some((_, t))) => Drawn::Two {
        from: from.and_then(|from| frame(&self.cache, &from, 0)),
        to,
        t,
      },
      _ => Drawn::One(to),
    })
  }

  /// Loads each of `images`, whose status waits, at its box's size at
  /// `scale` or one pixel, and tells its status: Ready or Failed at once,
  /// or when the worker is done. One the worker already decodes from its
  /// current source waits for that job; one mid-decode on an earlier
  /// source doesn't hold it back: that job's report is dropped, as the
  /// image shows another source now. A hidden or transparent
  /// image would never load otherwise, and a config may show it only once
  /// it is Ready.
  pub(crate) fn load_unseen(&mut self, images: &[ImageView<'_>], scale: f32) {
    for image in images {
      // A draw already sent this source to the worker for it, maybe at a
      // transform's scale; loading it here too would cancel that job.
      let sent = self.pending.iter().any(|(key, (_, waiting))| {
        matches!(key, Key::Image { path, .. } if path == image.path) && waiting.contains(&image.id)
      });
      if sent {
        continue;
      }
      // `as` saturates, and NaN is 0.
      let pixels = |length: f32| ((length * scale).round() as u32).max(1);
      let size = (pixels(image.rect.width), pixels(image.rect.height));
      let key = self.load(image.path, image, size, scale, true, Some(image.id));
      if let Some(entry) = self.cache.get(&key) {
        let status = match entry.frames {
          Some(_) => ImageStatus::Ready,
          None => ImageStatus::Failed,
        };
        crabshell_ui::image_status(image.id, image.path, status);
      }
    }
  }

  /// The file at `path` scaled to `size` to mask with, unfiltered and
  /// paused on its first frame, so a GIF never wakes the loop. A failure
  /// is logged once per path, so a box that changes size logs once.
  pub(crate) fn mask(&mut self, path: &Path, size: (u32, u32), scale: f32) -> Option<&Pixmap> {
    let image = ImageView {
      id: u64::MAX,
      path,
      rect: crabshell_ui::Rect::default(),
      fit: Fit::Fill,
      filters: ImageFilters::default(),
      playing: false,
      transition: ImageTransition::None,
      leaving: None,
    };
    let log = !self.failed_masks.contains(path);
    let key = self.load(path, &image, size, scale, log, None);
    self.masked.insert(key.clone(), self.frame);
    if self
      .cache
      .get(&key)
      .is_none_or(|entry| entry.frames.is_none())
    {
      if self.failed_masks.len() > CAPACITY {
        self.failed_masks.clear();
      }
      self.failed_masks.insert(path.to_owned());
      return None;
    }
    frame(&self.cache, &key, 0)
  }

  /// What [`image`](Self::image) draws as itself, or coming in, for
  /// tests.
  #[cfg(test)]
  pub(crate) fn picture(
    &mut self,
    image: &ImageView<'_>,
    size: (u32, u32),
    scale: f32,
  ) -> Option<&Pixmap> {
    match self.image(image, size, scale)? {
      Drawn::One(pixmap) | Drawn::Two { to: pixmap, .. } => Some(pixmap),
    }
  }

  /// The bytes of every cached picture, for tests.
  #[cfg(test)]
  pub(crate) fn cached_bytes(&self) -> usize {
    self
      .cache
      .values()
      .filter_map(|entry| entry.frames.as_ref())
      .flat_map(|frames| &frames.frames)
      .map(|frame| frame.pixmap.data().len())
      .sum()
  }

  /// The key of the picture `image`'s transition leaves, `Some(None)` for
  /// nothing that loads, and `None` while none runs or it leaves `key`
  /// itself. The file it leaves is looked at once a transition, falling
  /// back to the last good one, so every tile draws the same. A picture a
  /// transition stops leaving, as it ends or another replaces it, is
  /// dropped from the cache at once.
  fn leaving(
    &mut self,
    image: &ImageView<'_>,
    key: &Key,
    size: (u32, u32),
    scale: f32,
  ) -> Option<Option<Key>> {
    let shown = self.transitions.entry(image.id).or_default();
    let Some((named, _)) = image.leaving else {
      shown.leaving = None;
      if let Some(left) = shown.left.take() {
        self.forget(left, image.id, Some(key));
      }
      return None;
    };
    let drawn = match &shown.leaving {
      Some((before, drawn)) if before == named => drawn.clone(),
      _ => {
        let good = shown.good.clone();
        let left = self.load(named, image, size, scale, true, Some(image.id));
        if self.pending.contains_key(&left) {
          // Looked at again once it is decoded.
          return Some(None);
        }
        let loads = self
          .cache
          .get(&left)
          .is_some_and(|entry| entry.frames.is_some());
        let drawn = if loads { Some(named.to_owned()) } else { good };
        let shown = self.transitions.entry(image.id).or_default();
        shown.leaving = Some((named.to_owned(), drawn.clone()));
        drawn
      }
    };
    let nothing = drawn.is_none();
    let left = match drawn {
      Some(drawn) => {
        Some(self.load(&drawn, image, size, scale, true, Some(image.id))).filter(|left| left != key)
      }
      None => None,
    };
    let shown = self.transitions.entry(image.id).or_default();
    if shown.left != left {
      let replaced = std::mem::replace(&mut shown.left, left.clone());
      if let Some(replaced) = replaced {
        self.forget(replaced, image.id, Some(key));
      }
    }
    match left {
      Some(left) => Some(Some(left)),
      None => nothing.then_some(None),
    }
  }

  /// What `image` draws while `key`, its picture, is decoded: the
  /// picture its transition leaves, or else the one it drew before,
  /// stretched to the box when its size changed; nothing if neither is
  /// cached.
  fn waiting(
    &mut self,
    image: &ImageView<'_>,
    key: &Key,
    size: (u32, u32),
    scale: f32,
  ) -> Option<Drawn<'_>> {
    self.waited = true;
    if image.leaving.is_some() {
      // Its transition waits at the start until the picture comes in.
      crabshell_ui::image_source_decoding(image.id, image.path);
    }
    let left = if image.transition == ImageTransition::None {
      None
    } else {
      let shown = self.transitions.entry(image.id).or_default();
      shown.used = self.frame;
      shown.window = self.window;
      self.leaving(image, key, size, scale).flatten()
    };
    let before = left.or_else(|| self.shows.get(&image.id).map(|(shown, _)| shown.clone()))?;
    self.cache.get_mut(&before)?.used = self.frame;
    frame(&self.cache, &before, 0).map(Drawn::One)
  }

  /// Drops the picture cached under `key`, which image `id`'s transition
  /// no longer leaves, unless `id` now shows it as `current`, another
  /// image or a mask used it within the last [`RECENT`] draws, or another
  /// transition still leaves it. The image's own earlier showing doesn't
  /// count: that is the picture it left.
  fn forget(&mut self, key: Key, id: u64, current: Option<&Key>) {
    let recent = self.frame.saturating_sub(RECENT - 1);
    let kept = current == Some(&key)
      || self.masked.get(&key).is_some_and(|&when| when >= recent)
      || self
        .shows
        .iter()
        .any(|(&other, (shown, when))| other != id && *shown == key && *when >= recent)
      || self
        .transitions
        .iter()
        .any(|(&other, shown)| other != id && shown.left.as_ref() == Some(&key));
    if !kept {
      self.cache.remove(&key);
      self.memo = None;
    }
  }

  /// Records that image `id` drew the picture under `key` as its own.
  fn show(&mut self, id: u64, key: &Key) {
    match self.shows.get_mut(&id) {
      Some((shown, when)) if shown == key => *when = self.frame,
      _ => {
        self.shows.insert(id, (key.clone(), self.frame));
      }
    }
  }

  /// The cache key of the file at `path` drawn as `image` is, and the
  /// file's length.
  fn key(
    &mut self,
    path: &Path,
    image: &ImageView<'_>,
    (width, height): (u32, u32),
    scale: f32,
  ) -> (Key, u64) {
    self.lookups += 1;
    let (modified, length) = match self.modified.get(path) {
      Some(file) => *file,
      None => {
        let metadata = std::fs::metadata(path).ok();
        let file = (
          metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok()),
          metadata.map_or(0, |metadata| metadata.len()),
        );
        self.modified.insert(path.to_owned(), file);
        file
      }
    };
    let key = Key::Image {
      path: path.to_owned(),
      modified,
      width,
      height,
      fit: image.fit,
      filters: FilterKey::new(&image.filters, scale),
    };
    (key, length)
  }

  /// The key of the file at `path` drawn as `image` is, decoded into the
  /// cache on first use, marked used by this draw. A failure is logged
  /// only with `log`. For a `waiter`, the image that draws it, a large
  /// file goes to the worker, if there is one, and stays out of the cache
  /// until it comes back; without, it is decoded at once, taking over a
  /// job the worker hasn't started.
  fn load(
    &mut self,
    path: &Path,
    image: &ImageView<'_>,
    size: (u32, u32),
    scale: f32,
    log: bool,
    waiter: Option<u64>,
  ) -> Key {
    let (key, length) = self.key(path, image, size, scale);
    if !self.cache.contains_key(&key) {
      if !log {
        // Counted as logged already.
        self.failed.insert(key.clone());
      }
      let room = MAX_CACHED_BYTES.saturating_sub(self.bytes_in_use());
      let small = length <= SMALL_FILE
        && image.filters.blur == 0.0
        && u64::from(size.0) * u64::from(size.1) <= SMALL_PIXELS;
      if let (Some(worker), Some(id), false) = (&mut self.worker, waiter, small) {
        let (_, images) = self.pending.entry(key.clone()).or_insert_with(|| {
          let job = Job {
            path: path.to_owned(),
            size,
            fit: image.fit,
            filters: image.filters,
            scale,
            room,
          };
          (worker.send(job), Vec::new())
        });
        if !images.contains(&id) {
          images.push(id);
          self.leave_other_jobs(id, &key);
        }
        return key;
      }
      if let Some((job, images)) = self.pending.remove(&key) {
        if let Some(worker) = &mut self.worker {
          worker.cancel(job);
        }
        self.ready.extend(images);
      }
      let rasterized = crate::animation::frames(path, size, image.fit, &image.filters, scale, room);
      self.store(key.clone(), rasterized, || {
        format!("image {}", path.display())
      });
    }
    if let Some(entry) = self.cache.get_mut(&key) {
      entry.used = self.frame;
    }
    key
  }

  /// Takes image `id` off the jobs for its file other than `key`'s, such
  /// as the sizes a box went through while it waited, and cancels each
  /// left with no image waiting, so the last one comes in sooner.
  fn leave_other_jobs(&mut self, id: u64, key: &Key) {
    let Key::Image { path, .. } = key else {
      return;
    };
    let mut abandoned = Vec::new();
    for (other, (job, images)) in &mut self.pending {
      if other != key && matches!(other, Key::Image { path: file, .. } if file == path) {
        images.retain(|&waiting| waiting != id);
        if images.is_empty() {
          abandoned.push((other.clone(), *job));
        }
      }
    }
    for (other, job) in abandoned {
      self.pending.remove(&other);
      // One already started still finishes, and its picture is dropped.
      if let Some(worker) = &mut self.worker {
        worker.cancel(job);
      }
    }
  }

  /// Marks `image`, an animation of `count` frames in entry `key`, drawn,
  /// and returns the frame it is on. Another file starts it over.
  fn play(&mut self, image: &ImageView<'_>, key: &Key, count: usize) -> usize {
    let playback = self
      .playbacks
      .entry(image.id)
      .or_insert_with(|| Playback::new(key.clone()));
    if !playback.key.same_file(key) {
      *playback = Playback::new(key.clone());
    }
    playback.key = key.clone();
    playback.frame = playback.frame.min(count - 1);
    playback.drawn = true;
    playback.playing = image.playing;
    if playback.used != self.frame || playback.window != self.window {
      playback.tiles.clear();
    }
    if !playback.tiles.contains(&self.tile) {
      playback.tiles.push(self.tile);
    }
    playback.window = self.window;
    playback.used = self.frame;
    if !image.playing {
      playback.due = None;
    }
    playback.frame
  }

  /// When the next frame of an animation is due. One drawn since the last
  /// call is due its frame's delay after the frame was, or after `now` if
  /// that has passed. `None` while none plays.
  pub(crate) fn next_frame(&mut self, now: Instant) -> Option<Instant> {
    let Icons {
      playbacks, cache, ..
    } = self;
    for playback in playbacks.values_mut() {
      if !std::mem::take(&mut playback.drawn)
        || !playback.playing
        || playback.finished
        || playback.due.is_some()
      {
        continue;
      }
      let delay = cache
        .get(&playback.key)
        .and_then(|entry| entry.frames.as_ref())
        .and_then(|frames| frames.frames.get(playback.frame))
        .map(|frame| frame.delay);
      if let Some(delay) = delay {
        playback.due = Some(match playback.since {
          Some(since) if since + delay > now => since + delay,
          _ => now + delay,
        });
      }
    }
    playbacks.values().filter_map(|playback| playback.due).min()
  }

  /// Moves each animation whose frame is due by `now` on a frame, looping
  /// as its GIF says, and returns their image ids to repaint.
  pub(crate) fn advance(&mut self, now: Instant) -> Vec<u64> {
    let mut moved = Vec::new();
    for (&id, playback) in &mut self.playbacks {
      let Some(due) = playback.due.filter(|due| *due <= now) else {
        continue;
      };
      playback.due = None;
      playback.since = Some(due);
      let Some(frames) = self
        .cache
        .get(&playback.key)
        .and_then(|entry| entry.frames.as_ref())
      else {
        continue;
      };
      if playback.frame + 1 < frames.frames.len() {
        playback.frame += 1;
      } else if frames
        .repeats
        .is_none_or(|repeats| playback.played < repeats)
      {
        playback.played += 1;
        playback.frame = 0;
      } else {
        playback.finished = true;
        continue;
      }
      moved.push(id);
    }
    moved
  }

  /// View `view`'s `frame` scaled to `width` by `height` pixels as `fit`
  /// says. Each view keeps one, scaled again when its frame or box
  /// changes. `None` if the frame's pixels don't fit its size.
  pub(crate) fn capture(
    &mut self,
    view: u64,
    frame: &CaptureFrame,
    (width, height): (u32, u32),
    fit: Fit,
    crop: Option<IntRect>,
  ) -> Option<&Pixmap> {
    let shared = Shared(Rc::clone(&frame.rgba));
    let kept = self.captures.get(&view).is_some_and(|kept| {
      kept.frame == shared
        && (kept.width, kept.height, kept.fit, kept.crop) == (width, height, fit, crop)
    });
    if !kept {
      let scaled = PixmapRef::from_bytes(&frame.rgba, frame.width, frame.height)
        .ok_or_else(|| {
          format!(
            "{} bytes for {}x{}",
            frame.rgba.len(),
            frame.width,
            frame.height
          )
        })
        .and_then(|source| match crop {
          // The crop is cut to the frame, so the part is never empty.
          Some(crop) => source
            .clone_rect(crop)
            .ok_or_else(|| format!("a crop of {crop:?} outside the frame"))
            .and_then(|part| crate::images::scale(part.as_ref(), width, height, fit)),
          None => crate::images::scale(source, width, height, fit),
        });
      if let Err(error) = &scaled {
        tracing::warn!(%error, "couldn't draw a captured frame");
      }
      self.captures.insert(
        view,
        Capture {
          frame: shared,
          width,
          height,
          fit,
          crop,
          pixmap: scaled.ok(),
          used: self.frame,
        },
      );
    }
    let kept = self.captures.get_mut(&view)?;
    kept.used = self.frame;
    kept.pixmap.as_ref()
  }

  /// Caches what rasterizing `key` gave. A failure is cached as nothing
  /// and logged once, with `what` it was.
  fn store(&mut self, key: Key, rasterized: Result<Frames, String>, what: impl FnOnce() -> String) {
    self.rasterized += 1;
    let frames = match rasterized {
      Ok(frames) => Some(frames),
      Err(error) => {
        if self.failed.len() > CAPACITY {
          self.failed.clear();
        }
        if self.failed.insert(key.clone()) {
          tracing::warn!(source = %what(), %error, "couldn't draw an icon or image");
        }
        None
      }
    };
    let used = self.frame;
    self.cache.insert(key, Entry { frames, used });
  }

  /// The cached pixmap for `key`, marked used by this draw.
  fn used(&mut self, key: &Key) -> Option<&Pixmap> {
    let entry = self.cache.get_mut(key)?;
    entry.used = self.frame;
    Some(&entry.frames.as_ref()?.frames.first()?.pixmap)
  }

  /// The bytes of the pictures [`end_frame`](Self::end_frame) would keep:
  /// those a recent draw used and a waiting animation's frames.
  fn bytes_in_use(&self) -> usize {
    let recent = self.frame.saturating_sub(RECENT - 1);
    let waiting: HashSet<&Key> = self
      .playbacks
      .values()
      .filter(|playback| playback.due.is_some())
      .map(|playback| &playback.key)
      .collect();
    self
      .cache
      .iter()
      .filter(|(key, entry)| entry.used >= recent || waiting.contains(key))
      .filter_map(|(_, entry)| entry.frames.as_ref())
      .flat_map(|frames| &frames.frames)
      .map(|frame| frame.pixmap.data().len())
      .sum()
  }

  /// Drops what views `gone` kept.
  pub(crate) fn forget_captures(&mut self, gone: &[u64]) {
    self.captures.retain(|view, _| !gone.contains(view));
  }

  /// Lets go of what images dropped since the last draw kept, and of the
  /// picture a transition left that its own window just drew around
  /// without drawing it: hidden, under opacity 0 or removed. Another
  /// window's draw leaves it, since frames count every window's draws.
  fn let_go_of_left_pictures(&mut self) {
    let mut gone = Vec::new();
    for id in crabshell_ui::dropped_images() {
      self.shows.remove(&id);
      self.playbacks.remove(&id);
      if let Some(left) = self.transitions.remove(&id).and_then(|shown| shown.left) {
        gone.push((id, left));
      }
    }
    let (window, frame) = (self.window, self.frame);
    for (&id, shown) in &mut self.transitions {
      if shown.window == window && shown.used < frame {
        shown.leaving = None;
        if let Some(left) = shown.left.take() {
          gone.push((id, left));
        }
      }
    }
    for (id, left) in gone {
      let current = self.shows.get(&id).map(|(key, _)| key.clone());
      self.forget(left, id, current.as_ref());
    }
  }

  /// Ends a draw: past [`CAPACITY`], icons no draw used within the last
  /// [`RECENT`] are dropped, but never the frames of an animation waiting
  /// for its next. Captured frames go after as many draws whatever their
  /// number, since each is as large as its view.
  pub(crate) fn end_frame(&mut self) {
    self.modified.clear();
    self.let_go_of_left_pictures();
    let recent = self.frame.saturating_sub(RECENT - 1);
    self.captures.retain(|_, kept| kept.used >= recent);
    for playback in self.playbacks.values() {
      if playback.due.is_some()
        && let Some(entry) = self.cache.get_mut(&playback.key)
      {
        entry.used = self.frame;
      }
    }
    if self.playbacks.len() > CAPACITY {
      self
        .playbacks
        .retain(|_, playback| playback.due.is_some() || playback.used >= recent);
    }
    if self.transitions.len() > CAPACITY {
      self.transitions.retain(|_, shown| shown.used >= recent);
    }
    if self.shows.len() > CAPACITY {
      self.shows.retain(|_, (_, when)| *when >= recent);
    }
    self.masked.retain(|_, when| *when >= recent);
    let bytes: usize = self
      .cache
      .values()
      .filter_map(|entry| entry.frames.as_ref())
      .flat_map(|frames| &frames.frames)
      .map(|frame| frame.pixmap.data().len())
      .sum();
    if self.cache.len() > CAPACITY || bytes > MAX_CACHED_BYTES {
      let oldest = self.frame.saturating_sub(RECENT - 1);
      self.cache.retain(|_, entry| entry.used >= oldest);
    }
    self.frame += 1;
  }
}

/// Frame `index` of the picture cached under `key`.
fn frame<'a>(cache: &'a HashMap<Key, Entry>, key: &Key, index: usize) -> Option<&'a Pixmap> {
  Some(&cache.get(key)?.frames.as_ref()?.frames.get(index)?.pixmap)
}

/// A short description of `source` for log lines; raw pixels aren't
/// printed.
fn describe(source: &IconSource) -> String {
  match source {
    IconSource::Name(name) => format!("name {name:?}"),
    IconSource::File(path) => format!("file {}", path.display()),
    IconSource::Pixels(pixels) => format!("{}x{} pixels", pixels.width, pixels.height),
  }
}

/// Rasterizes an SVG or PNG file, told apart by its extension. Icon files
/// can come from other programs, so only regular files under a size cap are
/// read.
fn rasterize_file(path: &Path, side: u32) -> Result<Pixmap, String> {
  let extension = path
    .extension()
    .and_then(|extension| extension.to_str())
    .map(str::to_ascii_lowercase);
  match extension.as_deref() {
    Some("svg") => rasterize_svg(&read_capped(path, MAX_SVG_BYTES)?, side),
    Some("png") => {
      let max_pixels = u64::from(MAX_PNG_SIDE) * u64::from(MAX_PNG_SIDE);
      fit(
        &decode_png(&read_capped(path, MAX_PNG_BYTES)?, MAX_PNG_SIDE, max_pixels)?,
        side,
      )
    }
    _ => Err(String::from("only .svg and .png files can be icons")),
  }
}

/// Reads a regular file of at most `cap` bytes. A pipe or device would
/// block or never end, so it is refused before it is opened.
pub(crate) fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
  let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
  if !metadata.is_file() {
    return Err(String::from("not a regular file"));
  }
  if metadata.len() > cap {
    return Err(format!(
      "the file is {} bytes, more than {cap}",
      metadata.len()
    ));
  }
  let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
  read_limited(file, cap)
}

/// Reads at most `cap` bytes from `reader`, and fails if there is more. A
/// file can grow after its size was checked, and files in `/proc` report
/// no size at all.
pub(crate) fn read_limited(reader: impl Read, cap: u64) -> Result<Vec<u8>, String> {
  let mut data = Vec::new();
  reader
    .take(cap + 1)
    .read_to_end(&mut data)
    .map_err(|error| error.to_string())?;
  if data.len() as u64 > cap {
    return Err(format!("more than {cap} bytes"));
  }
  Ok(data)
}

fn rasterize_svg(data: &[u8], side: u32) -> Result<Pixmap, String> {
  let tree = svg_tree(data)?;
  let size = tree.size();
  let mut pixmap = empty(side)?;
  let scale = side as f32 / size.width().max(size.height());
  let transform = Transform::from_scale(scale, scale).post_translate(
    (side as f32 - size.width() * scale) / 2.0,
    (side as f32 - size.height() * scale) / 2.0,
  );
  resvg::render(&tree, transform, &mut pixmap.as_mut());
  Ok(pixmap)
}

/// Parses an SVG that may only embed `data:` images and uses no filters.
pub(crate) fn svg_tree(data: &[u8]) -> Result<usvg::Tree, String> {
  // An `<image>` naming a file would read anything on the machine, pipes
  // included, so only `data:` URLs are loaded.
  let options = usvg::Options {
    image_href_resolver: usvg::ImageHrefResolver {
      resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
      resolve_string: Box::new(|_, _| None),
    },
    ..usvg::Options::default()
  };
  let tree = usvg::Tree::from_data(data, &options).map_err(|error| error.to_string())?;
  // A filter can take hours to render at a tiny size, and icons don't
  // need them. This covers masks, patterns and embedded SVG images too.
  if !tree.filters().is_empty() {
    return Err(String::from("the SVG uses a filter, which icons may not"));
  }
  Ok(tree)
}

/// Decodes a PNG of at most `max_side` pixels a side and `max_pixels` in
/// all into a premultiplied pixmap of its own size.
pub(crate) fn decode_png(data: &[u8], max_side: u32, max_pixels: u64) -> Result<Pixmap, String> {
  // Every row is checked against these too, so a lying header can't make
  // the decoder allocate more.
  let limits = png::Limits {
    bytes: max_pixels.saturating_mul(8).min(usize::MAX as u64) as usize,
  };
  let mut decoder = png::Decoder::new_with_limits(Cursor::new(data), limits);
  decoder.set_transformations(png::Transformations::normalize_to_color8());
  let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
  let (width, height) = (reader.info().width, reader.info().height);
  if width > max_side || height > max_side || u64::from(width) * u64::from(height) > max_pixels {
    return Err(format!(
      "a {width}x{height} PNG is larger than {max_side} pixels a side or {max_pixels} pixels"
    ));
  }
  let size = reader
    .output_buffer_size()
    .ok_or("the PNG is too large to decode")?;
  let mut buffer = vec![0; size];
  let info = reader
    .next_frame(&mut buffer)
    .map_err(|error| error.to_string())?;
  let rows = buffer
    .chunks_exact(info.line_size)
    .take(info.height as usize);
  let mut rgba = Vec::with_capacity(info.width as usize * info.height as usize * 4);
  for row in rows {
    match info.color_type {
      png::ColorType::Rgba => rgba.extend_from_slice(&row[..info.width as usize * 4]),
      png::ColorType::Rgb => {
        for pixel in row.as_chunks::<3>().0.iter().take(info.width as usize) {
          rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
        }
      }
      png::ColorType::GrayscaleAlpha => {
        for pixel in row.as_chunks::<2>().0.iter().take(info.width as usize) {
          rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
        }
      }
      png::ColorType::Grayscale => {
        for &gray in row.iter().take(info.width as usize) {
          rgba.extend_from_slice(&[gray, gray, gray, 255]);
        }
      }
      png::ColorType::Indexed => return Err(String::from("unexpanded palette PNG")),
    }
  }
  premultiplied(rgba, info.width, info.height)
}

/// Raw tray pixels, ARGB in network byte order, as a pixmap.
fn from_argb(pixels: &IconPixels) -> Result<Pixmap, String> {
  if pixels.width > MAX_PIXELS_SIDE || pixels.height > MAX_PIXELS_SIDE {
    return Err(format!(
      "{}x{} pixels are larger than {MAX_PIXELS_SIDE} a side",
      pixels.width, pixels.height
    ));
  }
  let expected = u64::from(pixels.width) * u64::from(pixels.height) * 4;
  if pixels.data.len() as u64 != expected {
    return Err(format!(
      "{}x{} pixels need {expected} bytes, got {}",
      pixels.width,
      pixels.height,
      pixels.data.len()
    ));
  }
  let rgba = pixels
    .data
    .as_chunks::<4>()
    .0
    .iter()
    .flat_map(|argb| [argb[1], argb[2], argb[3], argb[0]])
    .collect();
  premultiplied(rgba, pixels.width, pixels.height)
}

/// A pixmap from straight-alpha RGBA bytes.
pub(crate) fn premultiplied(mut rgba: Vec<u8>, width: u32, height: u32) -> Result<Pixmap, String> {
  for pixel in rgba.as_chunks_mut::<4>().0 {
    let alpha = u16::from(pixel[3]);
    for channel in &mut pixel[..3] {
      *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
    }
  }
  let size = tiny_skia::IntSize::from_wh(width, height).ok_or("an icon with no pixels")?;
  Pixmap::from_vec(rgba, size).ok_or_else(|| String::from("icon pixels of the wrong length"))
}

/// Scales `image` to fit a `side` by `side` pixmap, centered. An image more
/// than twice the size is first halved, averaging each 2 by 2 block, so
/// thin details shrink to faint lines instead of vanishing or flickering.
fn fit(image: &Pixmap, side: u32) -> Result<Pixmap, String> {
  let mut halved;
  let mut image = image;
  while image.width().max(image.height()) > side * 2 {
    halved = halve(image.as_ref())?;
    image = &halved;
  }
  let mut pixmap = empty(side)?;
  let scale = side as f32 / image.width().max(image.height()) as f32;
  let transform = Transform::from_scale(scale, scale).post_translate(
    (side as f32 - image.width() as f32 * scale) / 2.0,
    (side as f32 - image.height() as f32 * scale) / 2.0,
  );
  let paint = PixmapPaint {
    quality: FilterQuality::Bicubic,
    ..PixmapPaint::default()
  };
  pixmap.draw_pixmap(0, 0, image.as_ref(), &paint, transform, None);
  Ok(pixmap)
}

/// `image` at half its size, rounded up, each pixel the average of the 2 by
/// 2 block it covers. Premultiplied pixels average correctly.
pub(crate) fn halve(image: PixmapRef<'_>) -> Result<Pixmap, String> {
  let (width, height) = (image.width(), image.height());
  let (half_width, half_height) = (width.div_ceil(2), height.div_ceil(2));
  let mut half = Pixmap::new(half_width, half_height).ok_or("an icon with no pixels")?;
  let source = image.data();
  let target = half.data_mut();
  for y in 0..half_height {
    for x in 0..half_width {
      let mut sum = [0_u32; 4];
      for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        // At an odd edge, the last pixel stands in for the missing one.
        let sx = (x * 2 + dx).min(width - 1);
        let sy = (y * 2 + dy).min(height - 1);
        let index = (sy * width + sx) as usize * 4;
        for (total, &value) in sum.iter_mut().zip(&source[index..index + 4]) {
          *total += u32::from(value);
        }
      }
      let index = (y * half_width + x) as usize * 4;
      for (value, total) in target[index..index + 4].iter_mut().zip(sum) {
        *value = ((total + 2) / 4) as u8;
      }
    }
  }
  Ok(half)
}

fn empty(side: u32) -> Result<Pixmap, String> {
  Pixmap::new(side, side).ok_or_else(|| String::from("an icon with no pixels"))
}
