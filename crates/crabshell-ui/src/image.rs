//! The bindings of [`Image`](crate::Image) and [`Icon`](crate::Icon), an
//! image's transition to a new source, and what the platform asks of an
//! image: a repaint of one GIF's box when its next frame is due.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::animation::{self, Transition};
use crate::tree::{Change, ImageNode, NodeId, NodeKind, Tree, borrow, replace};
use crate::widgets::{Fit, IconProps, ImageFilters, ImageProps};
use crate::window::Window;

/// Numbers every image mounted, in every window, so the renderer's
/// playback of one GIF and the platform's repaint of it can't mix two up.
static NEXT_IMAGE: AtomicU64 = AtomicU64::new(0);

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
    animation::start(
      label,
      Box::new(move |now| {
        let Some(tree) = tree.upgrade() else {
          return false;
        };
        let Ok(mut tree) = tree.try_borrow_mut() else {
          return true;
        };
        let begun = *started.get_or_insert(now);
        let waiting = transition.waiting(begun, now);
        let (t, ended) = transition.progress(begun, now);
        let mut mine = false;
        tree.update_image(id, |image| {
          let Some(leaving) = image
            .leaving
            .as_mut()
            .filter(|leaving| leaving.generation == generation)
          else {
            return Change::None;
          };
          mine = true;
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
  /// next frame is due, and returns whether it did. An image in another
  /// window, removed, hidden or not laid out yet repaints nothing.
  pub fn repaint_image(&self, id: u64) -> bool {
    let mut tree = borrow(&self.tree);
    let found = tree.images.iter().copied().find(|&node| {
      matches!(tree.node(node), Some(node) if node.rect.is_some()
        && matches!(&node.kind, NodeKind::Image(image) if image.id == id))
    });
    let Some(node) = found else {
      return false;
    };
    tree.invalidate(node, Change::Paint);
    true
  }
}
