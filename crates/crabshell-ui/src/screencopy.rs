//! [`ScreencopyView`], a widget that shows what an output or a window
//! shows, and what the platform reads and writes of it: the views to
//! capture for, and the frames it captured.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crabshell_reactive::{Signal, untrack};

use crate::capture::Capture;
use crate::geometry::{Affine, Rect, Size};
use crate::prop::{IntoProp, Prop};
use crate::style::Style;
use crate::tree::{Change, NodeId, NodeKind, Tree, borrow, replace};
use crate::widgets::{Common, Element, ElementKind, Fit, Widget, visibility_methods};
use crate::window::Window;

/// Numbers every view mounted, in every window, so a platform's capture
/// and a renderer's cache can't mix two up.
static NEXT_VIEW: AtomicU64 = AtomicU64::new(0);

/// The most dropped views kept for the platform to hear of; a shell
/// without one stops counting there.
const MAX_DROPPED: usize = 4096;

thread_local! {
  /// The ids of views dropped since [`dropped_captures`] last ran.
  static DROPPED: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// The ids of screencopy views removed, or in windows dropped, since the
/// last call, of either kind, so the platform can have the renderer let
/// go of the frames and copies it kept for them.
#[doc(hidden)]
pub fn dropped_captures() -> Vec<u64> {
  DROPPED.with(|dropped| std::mem::take(&mut *dropped.borrow_mut()))
}

/// A number no view or capture request had before.
pub(crate) fn next_id() -> u64 {
  NEXT_VIEW.fetch_add(1, Ordering::Relaxed)
}

/// What a [`ScreencopyView`] shows.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CaptureSource {
  /// The output of this name, such as `DP-1`.
  Output(String),
  /// The window of this id: a `Toplevel::id` from `cx.toplevels()`.
  Toplevel(u64),
}

pub(crate) struct ScreencopyProps {
  /// `None` for a view of a [`Capture`].
  pub(crate) source: Option<Prop<CaptureSource>>,
  pub(crate) capture: Option<Capture>,
  pub(crate) crop: Option<Prop<Option<Rect>>>,
  pub(crate) shown: Option<Signal<bool>>,
  pub(crate) live: Option<Prop<bool>>,
  pub(crate) paint_cursors: Option<Prop<bool>>,
  pub(crate) fit: Option<Prop<Fit>>,
}

/// Shows what an output or a window shows, captured by the compositor,
/// in the box its style's `width` and `height` give it, as an
/// [`Image`](crate::Image) shows a picture. It draws nothing until the
/// first frame arrives.
///
/// It captures one frame, unless it is `live`: then each change to the
/// source brings a new frame, and a source that doesn't change sends
/// none, so it wakes nothing. A source that isn't there yet, such as an
/// output not plugged in, is waited for. When it goes, the last frame
/// stays.
///
/// ```
/// use crabshell_ui::{CaptureSource, ScreencopyView, Style};
///
/// let thumbnail = ScreencopyView::new(CaptureSource::Output(String::from("DP-1")))
///     .live(true)
///     .style(Style::new().width(192.0).height(108.0));
/// ```
pub struct ScreencopyView {
  props: ScreencopyProps,
  common: Common,
}

impl ScreencopyView {
  /// A view of `source`: a [`CaptureSource`], or a closure that returns
  /// one.
  pub fn new(source: impl IntoProp<CaptureSource>) -> ScreencopyView {
    ScreencopyView::with(Some(source.into_prop()), None)
  }

  /// A view of `capture`'s frame, which it shares with every other view
  /// of it; the view captures nothing itself, so `live` and
  /// `paint_cursors` do nothing. It draws nothing while the capture has
  /// no frame.
  pub fn of(capture: Capture) -> ScreencopyView {
    ScreencopyView::with(None, Some(capture))
  }

  fn with(source: Option<Prop<CaptureSource>>, capture: Option<Capture>) -> ScreencopyView {
    ScreencopyView {
      props: ScreencopyProps {
        source,
        capture,
        crop: None,
        shown: None,
        live: None,
        paint_cursors: None,
        fit: None,
      },
      common: Common::default(),
    }
  }

  /// Shows only this part of the frame, in the source's logical pixels,
  /// as an output's windows are placed, filling the view's box as `fit`
  /// says. The part is cut to the frame, and one wholly outside it draws
  /// nothing. `None`, the default, shows the whole frame. A view of a
  /// source with a crop asks for the source's whole frame, at its own
  /// resolution, so the part stays sharp. A window's logical pixels are
  /// taken at the largest output scale, so with outputs of mixed scales
  /// a crop of a window on a smaller-scale one misses its mark.
  pub fn crop(mut self, rect: impl IntoProp<Option<Rect>>) -> ScreencopyView {
    self.props.crop = Some(rect.into_prop());
    self
  }

  /// Writes `true` to `shown` once the picture this view shows is on
  /// screen: the compositor presented the window's frame that first drew
  /// it, as its frame callback says. It writes `false` while the view has
  /// no picture, from when it takes a new one until that is presented,
  /// and while the view is hidden, outside the window or a clipping
  /// ancestor, or its window has no surface. Give each view a signal of
  /// its own.
  pub fn shown(mut self, shown: Signal<bool>) -> ScreencopyView {
    self.props.shown = Some(shown);
    self
  }

  /// Whether each change to the source shows, rather than only the first
  /// frame. Off by default.
  pub fn live(mut self, live: impl IntoProp<bool>) -> ScreencopyView {
    self.props.live = Some(live.into_prop());
    self
  }

  /// Whether the pointer is drawn into the frames. Off by default.
  pub fn paint_cursors(mut self, paint: impl IntoProp<bool>) -> ScreencopyView {
    self.props.paint_cursors = Some(paint.into_prop());
    self
  }

  /// Sets how the frame fills the view's box when their shapes differ.
  pub fn fit(mut self, fit: impl IntoProp<Fit>) -> ScreencopyView {
    self.props.fit = Some(fit.into_prop());
    self
  }

  /// Sets the style, whose `width` and `height` give the view its size.
  pub fn style(mut self, style: impl IntoProp<Style>) -> ScreencopyView {
    self.common.style = Some(style.into_prop());
    self
  }

  visibility_methods!();
}

impl Widget for ScreencopyView {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "ScreencopyView",
      common: self.common,
      kind: ElementKind::Capture(self.props),
      children: Vec::new(),
    }
  }
}

/// A captured frame, as a view keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureFrame {
  /// The width in pixels.
  pub width: u32,
  /// The height in pixels.
  pub height: u32,
  /// The source's whole width in logical pixels, which a
  /// [`crop`](ScreencopyView::crop) is given in. A frame scaled down for
  /// a small view keeps its source's. An output's is its logical size;
  /// a window's is its pixels divided by the largest output scale, which
  /// is wrong for a window on a smaller-scale output when scales are
  /// mixed.
  pub logical_width: u32,
  /// The source's whole height in logical pixels, as for
  /// [`logical_width`](Self::logical_width).
  pub logical_height: u32,
  /// Premultiplied RGBA, row by row: `width * height * 4` bytes. Shared,
  /// so a renderer can tell a new frame from the one it already scaled.
  pub rgba: Rc<[u8]>,
}

impl CaptureFrame {
  /// The part of the frame a view with `crop` draws, in the frame's
  /// pixels: left, top, width and height. The crop, in the source's
  /// logical pixels, is rounded to whole pixels and cut to the frame.
  /// `None` when the view draws nothing: a crop wholly outside the frame
  /// or not finite, or pixels that don't fill the frame's size.
  #[doc(hidden)]
  pub fn drawn_part(&self, crop: Option<Rect>) -> Option<[u32; 4]> {
    let pixels = u64::from(self.width) * u64::from(self.height) * 4;
    if self.width == 0 || self.height == 0 || self.rgba.len() as u64 != pixels {
      return None;
    }
    let Some(crop) = crop else {
      return Some([0, 0, self.width, self.height]);
    };
    let sides = [crop.x, crop.y, crop.width, crop.height];
    if !sides.iter().all(|side| side.is_finite()) {
      return None;
    }
    let (width, height) = (self.width as f32, self.height as f32);
    let across = width / self.logical_width.max(1) as f32;
    let down = height / self.logical_height.max(1) as f32;
    let left = (crop.x * across).round().max(0.0);
    let top = (crop.y * down).round().max(0.0);
    let right = (crop.right() * across).round().min(width);
    let bottom = (crop.bottom() * down).round().min(height);
    if right <= left || bottom <= top {
      return None;
    }
    // Within the frame, so `as` keeps every value.
    let [left, top, right, bottom] = [left, top, right, bottom].map(|side| side as u32);
    Some([left, top, right - left, bottom - top])
  }
}

/// One view the platform captures for, from [`Window::captures`].
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureRequest {
  /// Names the view while it is in a window. Never reused.
  pub id: u64,
  /// What it shows.
  pub source: CaptureSource,
  /// Whether it shows each new frame.
  pub live: bool,
  /// Whether the pointer is drawn into the frames.
  pub paint_cursors: bool,
  /// The view's content box at the last layout, in logical pixels, times
  /// the zoom the renderer draws it at inside transforms that scale it
  /// up, from [`Affine::layer_zoom`]: a frame needn't be larger. Infinite
  /// for a view with a crop, which needs the source's whole frame.
  pub size: Size,
}

pub(crate) struct CaptureNode {
  pub(crate) id: u64,
  /// `None` until the source's binding first runs.
  pub(crate) source: Option<CaptureSource>,
  pub(crate) live: bool,
  pub(crate) paint_cursors: bool,
  pub(crate) fit: Fit,
  pub(crate) crop: Option<Rect>,
  pub(crate) frame: Option<CaptureFrame>,
  pub(crate) shown: Option<Signal<bool>>,
  pub(crate) seen: Seen,
}

/// How far the picture a view shows got to the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Seen {
  /// No commit drew it yet, or the view isn't shown.
  #[default]
  Unseen,
  /// The window's last commit drew it.
  Drawn,
  /// The compositor presented that commit.
  Shown,
}

impl Drop for CaptureNode {
  fn drop(&mut self) {
    // At thread exit the list may be gone already; nobody is left to ask.
    let _ = DROPPED.try_with(|dropped| {
      let mut dropped = dropped.borrow_mut();
      if dropped.len() < MAX_DROPPED {
        dropped.push(self.id);
      }
    });
  }
}

impl CaptureNode {
  pub(crate) fn new() -> CaptureNode {
    CaptureNode {
      id: next_id(),
      source: None,
      live: false,
      paint_cursors: false,
      fit: Fit::default(),
      crop: None,
      frame: None,
      shown: None,
      seen: Seen::Unseen,
    }
  }

  /// The view left its window: a `shown` it wrote `true` reads `false`.
  pub(crate) fn gone(&self) {
    if let Some(shown) = self.shown {
      write(shown, false);
    }
  }

  /// Shows `frame`, and returns whether that changed what shows. A new
  /// picture isn't on screen until a commit draws it.
  fn show(&mut self, frame: Option<CaptureFrame>) -> bool {
    if self.frame == frame {
      return false;
    }
    self.frame = frame;
    if self.seen == Seen::Shown
      && let Some(shown) = self.shown
    {
      shown.try_set(false);
    }
    self.seen = Seen::Unseen;
    true
  }
}

impl Tree {
  /// Applies a view's properties. Only the source and fit change what
  /// shows; the platform reads the rest when it next captures.
  pub(crate) fn bind_capture(&mut self, id: NodeId, props: ScreencopyProps) {
    self.captures.push(id);
    if let Some(shown) = props.shown {
      write(shown, false);
      self.update_capture(id, |capture| {
        capture.shown = Some(shown);
        Change::None
      });
    }
    if let Some(capture) = props.capture {
      let frame = (move || capture.frame()).into_prop();
      self.bind(id, frame, "frame", |tree, id, frame| {
        tree.update_capture(id, |capture| match capture.show(frame) {
          true => Change::Paint,
          false => Change::None,
        });
      });
    }
    self.bind_opt(id, props.source, "source", |tree, id, value| {
      tree.update_capture(id, |capture| {
        if capture.source.as_ref() == Some(&value) {
          return Change::None;
        }
        capture.source = Some(value);
        // The old source's frame isn't this one's.
        match capture.show(None) {
          true => Change::Paint,
          false => Change::None,
        }
      });
    });
    self.bind_opt(id, props.crop, "crop", |tree, id, value| {
      tree.update_capture(id, |capture| match replace(&mut capture.crop, value) {
        Change::None => Change::None,
        _ => Change::Paint,
      });
    });
    self.bind_opt(id, props.live, "live", |tree, id, value| {
      tree.update_capture(id, |capture| {
        capture.live = value;
        Change::None
      });
    });
    self.bind_opt(
      id,
      props.paint_cursors,
      "paint_cursors",
      |tree, id, value| {
        tree.update_capture(id, |capture| {
          capture.paint_cursors = value;
          Change::None
        });
      },
    );
    self.bind_opt(id, props.fit, "fit", |tree, id, value| {
      tree.update_capture(id, |capture| match replace(&mut capture.fit, value) {
        Change::None => Change::None,
        _ => Change::Paint,
      });
    });
  }

  /// Whether any of what node `id` paints shows inside the window and
  /// every clipping ancestor, such as a scroll view, at the last layout.
  /// An ancestor's painted area stands for its clip, so a shadow around
  /// it counts as inside.
  fn in_sight(&self, id: NodeId) -> bool {
    let Some(node) = self.node(id) else {
      return false;
    };
    let mut seen = node.window_painted;
    let mut at = id;
    while !seen.is_empty()
      && let Some(parent) = self.taffy.parent(at)
    {
      if let Some(above) = self.node(parent)
        && (above.clips || parent == self.root)
      {
        seen = seen.intersect(above.window_painted);
      }
      at = parent;
    }
    !seen.is_empty()
  }

  /// How many times its own resolution node `id` is drawn at, through
  /// its transform and every one around it, as the renderer draws a
  /// transformed layer: [`Affine::layer_zoom`] of them all.
  fn zoom(&self, id: NodeId) -> f32 {
    let mut linear: Option<Affine> = None;
    let mut at = Some(id);
    while let Some(node) = at {
      if let Some(transform) = self.node(node).and_then(|node| node.transform) {
        linear = Some(match linear {
          Some(inner) => transform.after(&inner),
          None => transform,
        });
      }
      at = self.taffy.parent(node);
    }
    linear.map_or(1.0, |linear| linear.layer_zoom())
  }

  fn update_capture(&mut self, id: NodeId, f: impl FnOnce(&mut CaptureNode) -> Change) {
    self.update(id, |node| match &mut node.kind {
      NodeKind::Capture(capture) => f(capture),
      _ => Change::None,
    });
  }
}

impl Window {
  /// The views to capture for, in the order they were built: those with
  /// a source and a box that isn't empty at the last layout. A live view
  /// wholly outside the window or a clipping ancestor, such as one
  /// scrolled out of its scroll view's box, is left out until it shows
  /// again; a one-shot view is kept, so its frame is there when it does.
  pub fn captures(&self) -> Vec<CaptureRequest> {
    let tree = self.tree.borrow();
    tree
      .captures
      .iter()
      .filter_map(|&id| {
        let node = tree.node(id)?;
        let NodeKind::Capture(capture) = &node.kind else {
          return None;
        };
        let content = node.content;
        if node.rect.is_none() || content.is_empty() || capture.live && !tree.in_sight(id) {
          return None;
        }
        let zoom = tree.zoom(id);
        let size = match capture.crop {
          Some(_) => Size::new(f32::INFINITY, f32::INFINITY),
          None => Size::new(content.width * zoom, content.height * zoom),
        };
        Some(CaptureRequest {
          id: capture.id,
          source: capture.source.clone()?,
          live: capture.live,
          paint_cursors: capture.paint_cursors,
          size,
        })
      })
      .collect()
  }

  /// Shows `frame` in view `id`, repainting only that view, and returns
  /// whether it did. A frame the same as the one shown repaints nothing,
  /// so a live view of its own output settles. A view no longer in the
  /// window is ignored.
  pub fn show_capture(&self, id: u64, frame: CaptureFrame) -> bool {
    let mut tree = borrow(&self.tree);
    let found = tree.captures.iter().copied().find(|&node| {
      matches!(tree.node(node).map(|node| &node.kind),
        Some(NodeKind::Capture(capture)) if capture.id == id)
    });
    let Some(node) = found else {
      return false;
    };
    let mut shown = false;
    tree.update_capture(node, |capture| {
      shown = capture.show(Some(frame));
      match shown {
        true => Change::Paint,
        false => Change::None,
      }
    });
    shown
  }

  /// The window committed a frame. Each view in it with a picture no
  /// commit drew yet counts as drawn, and each hidden, outside the window
  /// or a clipping ancestor, or without a picture, as off screen.
  pub fn frame_committed(&self) {
    self.each_view(|capture, placed| match (placed, capture.seen) {
      (false, _) => Seen::Unseen,
      (true, Seen::Unseen) => Seen::Drawn,
      (true, seen) => seen,
    });
  }

  /// The compositor presented the window's last commit, as its frame
  /// callback says: each view that commit drew shows its picture.
  pub fn frame_presented(&self) {
    self.each_view(|capture, _| match capture.seen {
      Seen::Drawn => Seen::Shown,
      seen => seen,
    });
  }

  /// The window's surface went, as a hidden floating window's does: no
  /// view in it is on screen.
  pub fn unmapped(&self) {
    self.each_view(|_, _| Seen::Unseen);
  }

  /// Moves each view's picture to the state `next` gives, from whether
  /// it has one placed in sight, and writes `shown` where that changes.
  fn each_view(&self, mut next: impl FnMut(&CaptureNode, bool) -> Seen) {
    let mut writes = Vec::new();
    let mut tree = borrow(&self.tree);
    let views = tree.captures.clone();
    for id in views {
      let placed = tree
        .node(id)
        .is_some_and(|node| node.rect.is_some() && !node.content.is_empty())
        && tree.in_sight(id);
      let Some(node) = tree.node_mut(id) else {
        continue;
      };
      let NodeKind::Capture(capture) = &mut node.kind else {
        continue;
      };
      // What the renderer draws of it, which may be nothing.
      let frame = capture.frame.as_ref();
      let picture = frame.is_some_and(|frame| frame.drawn_part(capture.crop).is_some());
      let seen = next(capture, placed && picture);
      if let Some(shown) = capture.shown
        && (seen == Seen::Shown) != (capture.seen == Seen::Shown)
      {
        writes.push((shown, seen == Seen::Shown));
      }
      capture.seen = seen;
    }
    drop(tree);
    for (shown, value) in writes {
      write(shown, value);
    }
  }
}

/// Writes `value` to `signal` only when it changes, since a signal
/// notifies on every write.
fn write(signal: Signal<bool>, value: bool) {
  if untrack(|| signal.try_get()).is_some_and(|was| was != value) {
    signal.try_set(value);
  }
}
