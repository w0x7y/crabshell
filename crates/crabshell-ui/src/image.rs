//! The bindings of [`Image`](crate::Image) and [`Icon`](crate::Icon), an
//! image's transition to a new source, and what the platform asks of an
//! image: a repaint of one GIF's box when its next frame is due.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crabshell_reactive::{Signal, untrack};

use crate::animation::{self, Transition};
use crate::tree::{Change, ImageNode, NodeId, NodeKind, Tree, borrow, replace};
use crate::widgets::{Fit, IconProps, ImageFilters, ImageProps};
use crate::window::Window;

/// Numbers every image mounted, in every window, so the renderer's
/// playback of one GIF and the platform's repaint of it can't mix two up.
static NEXT_IMAGE: AtomicU64 = AtomicU64::new(0);

/// The most dropped images kept for the renderer to hear of; a shell
/// without one stops counting there.
const MAX_DROPPED: usize = 4096;

thread_local! {
  /// The ids of images dropped since [`dropped_images`] last ran.
  static DROPPED: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
  /// Images whose transition's new source the renderer couldn't load,
  /// with that source, until the transition's next tick.
  static FAILED: RefCell<Vec<(u64, PathBuf)>> = const { RefCell::new(Vec::new()) };
  /// Images whose transition's new source the renderer is decoding, with
  /// that source, until the picture comes in.
  static DECODING: RefCell<Vec<(u64, PathBuf)>> = const { RefCell::new(Vec::new()) };
  /// The images with a `status` signal: their id, the signal and the
  /// source they show.
  static WATCHED: RefCell<Vec<(u64, Signal<ImageStatus>, PathBuf)>> =
    const { RefCell::new(Vec::new()) };
  /// A report changed a status signal since [`image_status_changed`]
  /// last ran.
  static STATUS_CHANGED: Cell<bool> = const { Cell::new(false) };
}

/// Where an [`Image`](crate::Image)'s picture is, as Qt's `Image.status`,
/// written to the signal [`Image::status`](crate::Image::status) takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ImageStatus {
  /// The source is an empty path, so nothing is shown. The default.
  #[default]
  Empty,
  /// The source is new and its picture hasn't been drawn yet: it is read
  /// at the next draw, or decoded off the loop's thread.
  Loading,
  /// The image was drawn from its source's picture.
  Ready,
  /// The source can't be read or decoded, or is past the size limits.
  Failed,
}

/// Writes `status` to image `id`'s status signal while it shows
/// `source`, if that changes it. A renderer calls it while drawing, with
/// [`ImageStatus::Ready`] when it draws the image from its source's
/// picture and [`ImageStatus::Failed`] when that can't be loaded. A report
/// about another source, an empty one, or an image without a `status`
/// signal changes nothing. The platform then asks
/// [`image_status_changed`] and runs the reactive flush.
pub fn image_status(id: u64, source: &Path, status: ImageStatus) {
  if source.as_os_str().is_empty() {
    return;
  }
  let _ = WATCHED.try_with(|watched| {
    for (_, signal, _) in watched
      .borrow()
      .iter()
      .filter(|(at, _, shown)| *at == id && shown == source)
    {
      if write_status(*signal, status) {
        STATUS_CHANGED.with(|changed| changed.set(true));
      }
    }
  });
}

/// Whether a report since the last call changed an image's status
/// signal, so the platform runs the reactive flush for what reads it.
pub fn image_status_changed() -> bool {
  STATUS_CHANGED.with(|changed| changed.replace(false))
}

/// Writes `status` to `signal` if that changes it, and says whether it
/// did.
fn write_status(signal: Signal<ImageStatus>, status: ImageStatus) -> bool {
  untrack(|| signal.try_get()) != Some(status) && signal.try_set(status)
}

/// Watches image `id`'s status with `signal`, from its source's binding.
fn watch(id: u64, signal: Signal<ImageStatus>) {
  WATCHED.with(|watched| watched.borrow_mut().push((id, signal, PathBuf::new())));
}

/// Image `id` shows `source` now: its status goes back to Loading, or to
/// Empty for an empty path.
fn watch_source(id: u64, source: &Path) {
  WATCHED.with(|watched| {
    for (_, signal, shown) in watched.borrow_mut().iter_mut().filter(|(at, ..)| *at == id) {
      source.clone_into(shown);
      let status = match source.as_os_str().is_empty() {
        true => ImageStatus::Empty,
        false => ImageStatus::Loading,
      };
      write_status(*signal, status);
    }
  });
}

/// How many images have a status watched, for tests.
#[cfg(test)]
pub(crate) fn watched_images() -> usize {
  WATCHED.with(|watched| watched.borrow().len())
}

/// The ids of images removed, or in windows dropped, since the last call,
/// so a renderer can let go of what it kept for them, such as the picture
/// a transition was leaving.
pub fn dropped_images() -> Vec<u64> {
  DROPPED.with(|dropped| std::mem::take(&mut *dropped.borrow_mut()))
}

/// Tells image `id`'s transition that the renderer can't load its new
/// `source`, so at its next tick it ends at once, leaving the old picture
/// shown and its box still. The reverse of [`dropped_images`]: a renderer
/// calls it while drawing. A report for an image with no transition
/// running, or one to another source, changes nothing.
pub fn image_source_failed(id: u64, source: &Path) {
  FAILED.with(|failed| {
    let mut failed = failed.borrow_mut();
    if failed.len() < MAX_DROPPED && !failed.iter().any(|(at, path)| *at == id && path == source) {
      failed.push((id, source.to_owned()));
    }
  });
}

/// Tells image `id`'s transition that the renderer is still decoding its
/// new `source`, so its clock waits at the start, the old picture shown,
/// until [`Window::repaint_image`] says the picture came in. A renderer
/// calls it while drawing. A report for another source holds nothing.
pub fn image_source_decoding(id: u64, source: &Path) {
  DECODING.with(|decoding| {
    let mut decoding = decoding.borrow_mut();
    if decoding.len() < MAX_DROPPED
      && !decoding
        .iter()
        .any(|(at, path)| *at == id && path == source)
    {
      decoding.push((id, source.to_owned()));
    }
  });
}

/// Whether the renderer said it is decoding `source` for image `id`.
fn source_decoding(id: u64, source: Option<&PathBuf>) -> bool {
  DECODING.with(|decoding| {
    decoding
      .borrow()
      .iter()
      .any(|(at, path)| *at == id && Some(path) == source)
  })
}

/// Forgets what the renderer said it decodes for image `id`.
fn decoded(id: u64) {
  let _ = DECODING.try_with(|decoding| decoding.borrow_mut().retain(|(at, _)| *at != id));
}

/// How many decoding reports are held, for tests.
#[cfg(test)]
pub(crate) fn decoding_reports() -> usize {
  DECODING.with(|decoding| decoding.borrow().len())
}

/// How many reports wait for a transition's tick, for tests.
#[cfg(test)]
pub(crate) fn failed_reports() -> usize {
  FAILED.with(|failed| failed.borrow().len())
}

/// Whether the renderer said image `id` can't load `source`, using up
/// every report about `id`.
fn source_failed(id: u64, source: Option<&PathBuf>) -> bool {
  FAILED.with(|failed| {
    let mut found = false;
    failed.borrow_mut().retain(|(at, path)| {
      let mine = *at == id;
      found |= mine && Some(path) == source;
      !mine
    });
    found
  })
}

impl Drop for ImageNode {
  fn drop(&mut self) {
    // At thread exit the list may be gone already; nobody is left to ask.
    let _ = DROPPED.try_with(|dropped| {
      let mut dropped = dropped.borrow_mut();
      if dropped.len() < MAX_DROPPED {
        dropped.push(self.id);
      }
    });
    // A report about it would wait for a tick that never comes.
    let _ = FAILED.try_with(|failed| failed.borrow_mut().retain(|(id, _)| *id != self.id));
    decoded(self.id);
    let _ = WATCHED.try_with(|watched| watched.borrow_mut().retain(|(id, ..)| *id != self.id));
  }
}

impl ImageNode {
  pub(crate) fn new() -> ImageNode {
    ImageNode {
      id: NEXT_IMAGE.fetch_add(1, Ordering::Relaxed),
      source: None,
      width: crate::Image::DEFAULT_SIZE,
      height: crate::Image::DEFAULT_SIZE,
      fit: Fit::default(),
      filters: ImageFilters::default(),
      playing: true,
      transition: ImageTransition::None,
      leaving: None,
      transitions: 0,
    }
  }
}

/// How an [`Image`](crate::Image) shows a new source: at once, or coming
/// in over the old picture over a [`Transition`], which gives its time,
/// curve and delay. Both pictures are drawn at the image's size, fit and
/// filters.
///
/// ```
/// use crabshell_ui::{Easing, ImageTransition, Transition};
///
/// const WALLPAPER: ImageTransition =
///     ImageTransition::wipe(Transition::new(800).easing(Easing::InOutCubic), 90.0);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ImageTransition {
  /// The new picture replaces the old at once. The default.
  #[default]
  None,
  /// The new picture fades in as the old one fades out.
  Crossfade(Transition),
  /// An edge sweeps across the box in the direction of the angle, in
  /// degrees as CSS's gradient angle: 0 toward the top, 90 toward the
  /// right. The new picture is behind it. An angle that isn't finite
  /// counts as 90.
  Wipe(Transition, f32),
  /// A circle grows from the middle until it covers the box, the new
  /// picture inside it.
  Circle(Transition),
  /// The old picture breaks into growing blocks and the new one sharpens
  /// out of them.
  Pixelate(Transition),
}

impl ImageTransition {
  /// No transition: a new source shows at once.
  pub const fn none() -> ImageTransition {
    ImageTransition::None
  }

  /// A crossfade over `transition`.
  pub const fn crossfade(transition: Transition) -> ImageTransition {
    ImageTransition::Crossfade(transition)
  }

  /// A wipe toward `angle` degrees over `transition`.
  pub const fn wipe(transition: Transition, angle: f32) -> ImageTransition {
    ImageTransition::Wipe(transition, angle)
  }

  /// A circle reveal over `transition`.
  pub const fn circle(transition: Transition) -> ImageTransition {
    ImageTransition::Circle(transition)
  }

  /// A pixelation over `transition`.
  pub const fn pixelate(transition: Transition) -> ImageTransition {
    ImageTransition::Pixelate(transition)
  }

  /// Its time and curve, `None` for none.
  pub const fn transition(self) -> Option<Transition> {
    match self {
      ImageTransition::None => None,
      ImageTransition::Crossfade(transition)
      | ImageTransition::Wipe(transition, _)
      | ImageTransition::Circle(transition)
      | ImageTransition::Pixelate(transition) => Some(transition),
    }
  }
}

/// The picture a running transition leaves.
pub(crate) struct Leaving {
  pub(crate) source: PathBuf,
  /// How far along the transition is, eased.
  pub(crate) t: f32,
  /// Which transition of the image it is, so one replaced stops.
  generation: u64,
}

/// `value` if it is finite and above 0, 0 if it is finite and not, and
/// `unset` otherwise. Never -0, so equal filters hash alike.
fn filter_value(value: f32, unset: f32) -> f32 {
  if !value.is_finite() {
    unset
  } else if value > 0.0 {
    value
  } else {
    0.0
  }
}

/// Sets `slot` to `value`: a change repaints, never lays out.
fn repaint<T: PartialEq>(slot: &mut T, value: T) -> Change {
  match replace(slot, value) {
    Change::None => Change::None,
    _ => Change::Paint,
  }
}

impl Tree {
  pub(crate) fn bind_image(&mut self, id: NodeId, props: ImageProps) {
    self.images.push(id);
    if let (Some(signal), Some(NodeKind::Image(image))) =
      (props.status, self.node(id).map(|node| &node.kind))
    {
      watch(image.id, signal);
    }
    self.bind(id, props.source, "image", |tree, id, value| {
      let mut started = None;
      // A new picture of the same size only needs a repaint.
      tree.update(id, |node| {
        let drawn = node.rect.is_some();
        let NodeKind::Image(image) = &mut node.kind else {
          return Change::None;
        };
        if image.source.as_ref() == Some(&value) {
          return Change::None;
        }
        watch_source(image.id, &value);
        let old = image.source.replace(value);
        let transition = image
          .transition
          .transition()
          .filter(|transition| !transition.is_instant());
        // Mid-transition, it leaves the picture most of the box shows.
        let old = match image.leaving.take() {
          Some(leaving) if leaving.t < 0.5 => Some(leaving.source),
          _ => old,
        };
        // One not drawn yet has nothing to leave, and one going back to
        // what it shows needs no transition.
        let old = old.filter(|old| image.source.as_ref() != Some(old));
        if let (Some(old), Some(transition), true) = (old, transition, drawn) {
          image.transitions += 1;
          image.leaving = Some(Leaving {
            source: old,
            t: 0.0,
            generation: image.transitions,
          });
          started = Some((image.transitions, transition));
        }
        Change::Paint
      });
      if let Some((generation, transition)) = started {
        tree.start_image_transition(id, generation, transition);
      }
    });
    self.bind_opt(id, props.width, "width", |tree, id, value| {
      tree.update_image(id, |image| replace(&mut image.width, value));
    });
    self.bind_opt(id, props.height, "height", |tree, id, value| {
      tree.update_image(id, |image| replace(&mut image.height, value));
    });
    self.bind_opt(id, props.fit, "fit", |tree, id, value| {
      tree.update_image(id, |image| repaint(&mut image.fit, value));
    });
    self.bind_opt(id, props.tint, "tint", |tree, id, value| {
      tree.update_image(id, |image| repaint(&mut image.filters.tint, value));
    });
    self.bind_opt(id, props.brightness, "brightness", |tree, id, value| {
      let value = filter_value(value, 1.0);
      tree.update_image(id, |image| repaint(&mut image.filters.brightness, value));
    });
    self.bind_opt(id, props.contrast, "contrast", |tree, id, value| {
      let value = filter_value(value, 1.0);
      tree.update_image(id, |image| repaint(&mut image.filters.contrast, value));
    });
    self.bind_opt(id, props.saturation, "saturation", |tree, id, value| {
      let value = filter_value(value, 1.0);
      tree.update_image(id, |image| repaint(&mut image.filters.saturation, value));
    });
    self.bind_opt(id, props.blur, "blur", |tree, id, value| {
      let value = filter_value(value, 0.0);
      tree.update_image(id, |image| repaint(&mut image.filters.blur, value));
    });
    self.bind_opt(id, props.playing, "playing", |tree, id, value| {
      tree.update_image(id, |image| repaint(&mut image.playing, value));
    });
    self.bind_opt(id, props.transition, "transition", |tree, id, value| {
      tree.update_image(id, |image| repaint(&mut image.transition, value));
    });
  }

  /// Moves image `id`'s transition number `generation` with the animation
  /// clock, repainting its box each tick, until it ends, when the old
  /// picture is let go, or another replaces it.
  fn start_image_transition(&self, id: NodeId, generation: u64, transition: Transition) {
    let tree = self.this.clone();
    let mut started = animation::start_time();
    let label = self.node(id).map_or_else(
      || Rc::from("an image transition"),
      |node| format!("{}.transition", node.path).into(),
    );
    let busy = self.node(id).and_then(|node| node.cx.busy());
    animation::start(
      label,
      busy,
      Box::new(move |now| {
        let Some(tree) = tree.upgrade() else {
          return false;
        };
        let Ok(mut tree) = tree.try_borrow_mut() else {
          return true;
        };
        // While its new picture is decoded, the clock waits at the start.
        let held = tree.node(id).is_some_and(|node| {
          matches!(&node.kind, NodeKind::Image(image)
            if image.leaving.as_ref().is_some_and(|leaving| leaving.generation == generation)
              && source_decoding(image.id, image.source.as_ref()))
        });
        if held {
          started = None;
          return true;
        }
        let begun = *started.get_or_insert(now);
        let waiting = transition.waiting(begun, now);
        let (t, mut ended) = transition.progress(begun, now);
        let mut mine = false;
        tree.update_image(id, |image| {
          let failed = source_failed(image.id, image.source.as_ref());
          let Some(leaving) = image
            .leaving
            .as_mut()
            .filter(|leaving| leaving.generation == generation)
          else {
            return Change::None;
          };
          mine = true;
          ended |= failed;
          if ended {
            image.leaving = None;
          } else if waiting {
            // Nothing on screen changes until the delay is over.
            return Change::None;
          } else {
            leaving.t = t;
          }
          Change::Paint
        });
        mine && !ended
      }),
    );
  }

  pub(crate) fn bind_icon(&mut self, id: NodeId, props: IconProps) {
    self.bind(id, props.source, "icon", |tree, id, value| {
      // A new picture of the same size only needs a repaint.
      tree.update_icon(id, |icon| repaint(&mut icon.source, Some(value)));
    });
    self.bind_opt(id, props.fallback, "fallback", |tree, id, value| {
      tree.update_icon(id, |icon| repaint(&mut icon.fallback, Some(value)));
    });
    self.bind_opt(id, props.size, "size", |tree, id, value| {
      tree.update_icon(id, |icon| replace(&mut icon.size, value));
    });
    self.bind_opt(id, props.tint, "tint", |tree, id, value| {
      tree.update_icon(id, |icon| repaint(&mut icon.tint, value));
    });
  }
}

impl Window {
  /// Repaints the box of image `id`, as when the renderer says a GIF's
  /// next frame is due or a picture it decoded came in, which also ends
  /// its transition's wait for it, and returns whether it did. An image in another
  /// window, removed, hidden or not laid out yet repaints nothing.
  pub fn repaint_image(&self, id: u64) -> bool {
    let mut tree = borrow(&self.tree);
    let found = tree.images.iter().copied().find(|&node| {
      matches!(tree.node(node), Some(node) if node.rect.is_some()
        && matches!(&node.kind, NodeKind::Image(image) if image.id == id))
    });
    // A picture that came in ends any wait for it.
    decoded(id);
    let Some(node) = found else {
      return false;
    };
    tree.invalidate(node, Change::Paint);
    true
  }
}
