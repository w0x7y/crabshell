//! The bindings of [`Image`](crate::Image) and [`Icon`](crate::Icon), and
//! what the platform asks of an image: a repaint of one GIF's box when its
//! next frame is due.

use std::sync::atomic::{AtomicU64, Ordering};

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
    }
  }
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
      // A new picture of the same size only needs a repaint.
      tree.update_image(id, |image| repaint(&mut image.source, Some(value)));
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
