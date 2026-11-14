//! [`ScreencopyView`], a widget that shows what an output or a window
//! shows, and what the platform reads and writes of it: the views to
//! capture for, and the frames it captured.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::geometry::Size;
use crate::prop::{IntoProp, Prop};
use crate::style::Style;
use crate::tree::{Change, NodeId, NodeKind, Tree, borrow, replace};
use crate::widgets::{Element, ElementKind, Fit, Widget};
use crate::window::Window;

/// Numbers every view mounted, in every window, so a platform's capture
/// and a renderer's cache can't mix two up.
static NEXT_VIEW: AtomicU64 = AtomicU64::new(0);

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
  pub(crate) source: Prop<CaptureSource>,
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
  style: Option<Prop<Style>>,
}

impl ScreencopyView {
  /// A view of `source`: a [`CaptureSource`], or a closure that returns
  /// one.
  pub fn new(source: impl IntoProp<CaptureSource>) -> ScreencopyView {
    ScreencopyView {
      props: ScreencopyProps {
        source: source.into_prop(),
        live: None,
        paint_cursors: None,
        fit: None,
      },
      style: None,
    }
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
    self.style = Some(style.into_prop());
    self
  }
}

impl Widget for ScreencopyView {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "ScreencopyView",
      style: self.style,
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
  /// Premultiplied RGBA, row by row: `width * height * 4` bytes. Shared,
  /// so a renderer can tell a new frame from the one it already scaled.
  pub rgba: Rc<[u8]>,
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
  /// The view's content box at the last layout, in logical pixels: a
  /// frame needn't be larger.
  pub size: Size,
}

pub(crate) struct CaptureNode {
  pub(crate) id: u64,
  /// `None` until the source's binding first runs.
  pub(crate) source: Option<CaptureSource>,
  pub(crate) live: bool,
  pub(crate) paint_cursors: bool,
  pub(crate) fit: Fit,
  pub(crate) frame: Option<CaptureFrame>,
}

impl CaptureNode {
  pub(crate) fn new() -> CaptureNode {
    CaptureNode {
      id: NEXT_VIEW.fetch_add(1, Ordering::Relaxed),
      source: None,
      live: false,
      paint_cursors: false,
      fit: Fit::default(),
      frame: None,
    }
  }
}

impl Tree {
  /// Applies a view's properties. Only the source and fit change what
  /// shows; the platform reads the rest when it next captures.
  pub(crate) fn bind_capture(&mut self, id: NodeId, props: ScreencopyProps) {
    self.captures.push(id);
    self.bind(id, props.source, "source", |tree, id, value| {
      tree.update_capture(id, |capture| {
        if capture.source.as_ref() == Some(&value) {
          return Change::None;
        }
        capture.source = Some(value);
        // The old source's frame isn't this one's.
        match capture.frame.take() {
          Some(_) => Change::Paint,
          None => Change::None,
        }
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

  fn update_capture(&mut self, id: NodeId, f: impl FnOnce(&mut CaptureNode) -> Change) {
    self.update(id, |node| match &mut node.kind {
      NodeKind::Capture(capture) => f(capture),
      _ => Change::None,
    });
  }
}

impl Window {
  /// The views to capture for, in the order they were built: those with
  /// a source and a box that isn't empty at the last layout.
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
        if node.rect.is_none() || content.is_empty() {
          return None;
        }
        Some(CaptureRequest {
          id: capture.id,
          source: capture.source.clone()?,
          live: capture.live,
          paint_cursors: capture.paint_cursors,
          size: Size::new(content.width, content.height),
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
    tree.update(node, |node| match &mut node.kind {
      NodeKind::Capture(capture) if capture.frame.as_ref() != Some(&frame) => {
        capture.frame = Some(frame);
        shown = true;
        Change::Paint
      }
      _ => Change::None,
    });
    shown
  }
}
