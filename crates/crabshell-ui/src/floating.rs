//! [`FloatingWindow`], an ordinary window that the compositor places and
//! the user can move, and what a widget in one can ask of it.

use std::cell::RefCell;
use std::rc::Rc;

use crabshell_reactive::{Signal, untrack};

use crate::cx::{Anchor, Cx};
use crate::input::{Handler, run_isolated};
use crate::prop::{IntoProp, Prop};
use crate::region::Region;
use crate::style::Style;
use crate::tree::{Tree, borrow};
use crate::widgets::Widget;
use crate::window::{Anchors, Hints, Output, Window};

pub(crate) type CloseHandler = Rc<Handler<dyn Fn(&Cx)>>;

/// A window like an application's, which the compositor places, tiles or
/// floats: a launcher, a settings panel, a picture viewer. Show one with
/// `cx.floating` from the `crabshell` crate. Every property takes a value
/// or a closure.
///
/// ```
/// use crabshell_ui::{FloatingWindow, Text};
///
/// let about = FloatingWindow::new()
///     .title("About")
///     .width(320)
///     .height(200)
///     .child(Text::new("hello"));
/// ```
pub struct FloatingWindow {
  pub(crate) title: Option<Prop<String>>,
  pub(crate) width: Option<Prop<u32>>,
  pub(crate) height: Option<Prop<u32>>,
  pub(crate) min_size: Option<Prop<(u32, u32)>>,
  pub(crate) max_size: Option<Prop<(u32, u32)>>,
  pub(crate) maximized: Option<Prop<bool>>,
  pub(crate) fullscreen: Option<Prop<bool>>,
  pub(crate) visible: Option<Prop<bool>>,
  pub(crate) hints: Hints,
  pub(crate) on_close: Option<CloseHandler>,
  pub(crate) style: Option<Prop<Style>>,
  pub(crate) child: Option<Box<dyn Widget>>,
}

impl Default for FloatingWindow {
  fn default() -> FloatingWindow {
    FloatingWindow::new()
  }
}

impl FloatingWindow {
  /// The default width and height in logical pixels.
  pub const DEFAULT_SIZE: (u32, u32) = (400, 300);

  /// A visible, empty window of the default size.
  pub fn new() -> FloatingWindow {
    FloatingWindow {
      title: None,
      width: None,
      height: None,
      min_size: None,
      max_size: None,
      maximized: None,
      fullscreen: None,
      visible: None,
      hints: Hints::default(),
      on_close: None,
      style: None,
      child: None,
    }
  }

  /// Sets the title the compositor shows, as in a title bar or task list.
  pub fn title(mut self, title: impl IntoProp<String>) -> FloatingWindow {
    self.title = Some(title.into_prop());
    self
  }

  /// Sets the width it asks for, in logical pixels. The compositor may
  /// give it another, as when tiling it.
  pub fn width(mut self, width: impl IntoProp<u32>) -> FloatingWindow {
    self.width = Some(width.into_prop());
    self
  }

  /// Sets the height it asks for, in logical pixels.
  pub fn height(mut self, height: impl IntoProp<u32>) -> FloatingWindow {
    self.height = Some(height.into_prop());
    self
  }

  /// Sets the smallest size the user may make it, as `(width, height)`;
  /// 0 leaves a side free.
  pub fn min_size(mut self, size: impl IntoProp<(u32, u32)>) -> FloatingWindow {
    self.min_size = Some(size.into_prop());
    self
  }

  /// Sets the largest size the user may make it, as `(width, height)`;
  /// 0 leaves a side free.
  pub fn max_size(mut self, size: impl IntoProp<(u32, u32)>) -> FloatingWindow {
    self.max_size = Some(size.into_prop());
    self
  }

  /// Sets whether it asks to fill the screen but for panels.
  pub fn maximized(mut self, maximized: impl IntoProp<bool>) -> FloatingWindow {
    self.maximized = Some(maximized.into_prop());
    self
  }

  /// Sets whether it asks to cover the whole screen.
  pub fn fullscreen(mut self, fullscreen: impl IntoProp<bool>) -> FloatingWindow {
    self.fullscreen = Some(fullscreen.into_prop());
    self
  }

  /// Sets whether it is shown. Hidden, it has no surface at all; shown
  /// again, it is a new window to the compositor.
  pub fn visible(mut self, visible: impl IntoProp<bool>) -> FloatingWindow {
    self.visible = Some(visible.into_prop());
    self
  }

  /// Sets the part of the window that takes pointer input, as
  /// [`PanelWindow::mask`](crate::PanelWindow::mask) does: clicks outside
  /// it reach whatever is below, and `None`, the default, is the whole
  /// window.
  pub fn mask(mut self, mask: impl IntoProp<Option<Region>>) -> FloatingWindow {
    self.hints.mask = Some(mask.into_prop());
    self
  }

  /// Asks the compositor to blur what is behind this part of the window,
  /// as [`PanelWindow::blur`](crate::PanelWindow::blur) does.
  pub fn blur(mut self, region: impl IntoProp<Option<Region>>) -> FloatingWindow {
    self.hints.blur = Some(region.into_prop());
    self
  }

  /// Keeps the screen from idling while `true` and the window is shown,
  /// as [`PanelWindow::inhibit_idle`](crate::PanelWindow::inhibit_idle)
  /// does.
  pub fn inhibit_idle(mut self, inhibit: impl IntoProp<bool>) -> FloatingWindow {
    self.hints.inhibit_idle = Some(inhibit.into_prop());
    self
  }

  /// Takes the compositor's shortcuts while `true` and the window has the
  /// keyboard, as
  /// [`PanelWindow::inhibit_shortcuts`](crate::PanelWindow::inhibit_shortcuts)
  /// does.
  pub fn inhibit_shortcuts(mut self, inhibit: impl IntoProp<bool>) -> FloatingWindow {
    self.hints.inhibit_shortcuts = Some(inhibit.into_prop());
    self
  }

  /// Has the compositor write whether it honors `inhibit_shortcuts`, as
  /// [`PanelWindow::shortcuts_inhibited`](crate::PanelWindow::shortcuts_inhibited)
  /// does.
  pub fn shortcuts_inhibited(mut self, signal: Signal<bool>) -> FloatingWindow {
    self.hints.shortcuts_inhibited = Some(signal);
    self
  }

  /// Holds the focus while `signal` is `true`, with the other windows
  /// given it, as [`PanelWindow::focus_grab`](crate::PanelWindow::focus_grab)
  /// does.
  pub fn focus_grab(mut self, signal: Signal<bool>) -> FloatingWindow {
    self.hints.focus_grab = Some(signal);
    self
  }

  /// Sets the opacity the compositor shows the whole window at, blur
  /// included, as
  /// [`PanelWindow::compositor_opacity`](crate::PanelWindow::compositor_opacity)
  /// does.
  pub fn compositor_opacity(mut self, opacity: impl IntoProp<f32>) -> FloatingWindow {
    self.hints.compositor_opacity = Some(opacity.into_prop());
    self
  }

  /// Tells the compositor which part of the window holds anything to
  /// see, as [`PanelWindow::visible_mask`](crate::PanelWindow::visible_mask)
  /// does.
  pub fn visible_mask(mut self, region: impl IntoProp<Option<Region>>) -> FloatingWindow {
    self.hints.visible_mask = Some(region.into_prop());
    self
  }

  /// Runs `handler` when the user or compositor asks the window to close,
  /// as with a title bar's close button. Without one, the window hides
  /// until `visible` turns false and true again.
  pub fn on_close(mut self, handler: impl Fn(&Cx) + 'static) -> FloatingWindow {
    self.on_close = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Sets the window's style. Its color and font are inherited by every
  /// [`Text`](crate::Text) in it that doesn't set its own.
  pub fn style(mut self, style: impl IntoProp<Style>) -> FloatingWindow {
    self.style = Some(style.into_prop());
    self
  }

  /// Sets the window's one child, which fills the window.
  pub fn child(mut self, child: impl Widget) -> FloatingWindow {
    self.child = Some(Box::new(child));
    self
  }
}

/// What the platform needs to show a floating window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FloatingConfig {
  /// The title.
  pub title: String,
  /// The size asked for, in logical pixels.
  pub width: u32,
  /// The size asked for, in logical pixels.
  pub height: u32,
  /// The smallest size; 0 leaves a side free.
  pub min_size: (u32, u32),
  /// The largest size; 0 leaves a side free.
  pub max_size: (u32, u32),
  /// Whether it asks to be maximized.
  pub maximized: bool,
  /// Whether it asks to be fullscreen.
  pub fullscreen: bool,
  /// Whether it should have a surface: visible, and not closed since.
  pub shown: bool,
}

/// What a widget asked of the floating window it is in. Moving and
/// resizing need a pointer button held, so ask from a button's
/// `on_press`; asked otherwise, they are ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRequest {
  /// Let the user drag the window, as by its title bar.
  Move,
  /// Let the user resize the window from these edges.
  Resize(Anchors),
  /// Minimize it; the compositor may do nothing.
  Minimize,
}

/// A floating window's settings in its tree.
pub(crate) struct FloatingState {
  title: String,
  width: u32,
  height: u32,
  min_size: (u32, u32),
  max_size: (u32, u32),
  maximized: bool,
  fullscreen: bool,
  visible: bool,
  /// Closed with no `on_close`: hidden until `visible` turns false.
  dismissed: bool,
  /// Kept in the reactive runtime, which drops the user's closure when the
  /// window's owner goes.
  on_close: Signal<Option<CloseHandler>>,
  pub(crate) requests: Vec<WindowRequest>,
}

impl FloatingState {
  /// Shown now: `visible`, and not closed by the compositor since.
  pub(crate) fn shown(&self) -> bool {
    self.visible && !self.dismissed
  }

  pub(crate) fn title(&self) -> &str {
    &self.title
  }
}

impl Window {
  /// Builds `window` under a new child owner of `cx`, as
  /// [`Window::new`] does for a panel.
  pub fn floating(cx: &Cx, mut window: FloatingWindow) -> Window {
    let child = untrack(|| window.child.take().map(|child| child.into_element()));
    let path: Rc<str> = "FloatingWindow".into();
    let cx = cx
      .child()
      .named(path.to_string())
      .at(Rc::new(Anchor::default()));
    let owner = cx.owner();
    let on_close = cx.signal(window.on_close.take());
    let tree = Rc::new_cyclic(|this| {
      RefCell::new(Tree::new(
        this.clone(),
        "FloatingWindow",
        Output::new(""),
        cx,
        path,
      ))
    });
    borrow(&tree).mount_floating(window, child, on_close);
    Window { tree, owner }
  }

  /// A floating window's settings; `None` for a panel or popup. Read
  /// them only after the first flush, as [`surface`](Self::surface).
  pub fn floating_config(&self) -> Option<FloatingConfig> {
    let tree = self.tree.borrow();
    let floating = tree.floating.as_ref()?;
    let (width, height) = FloatingWindow::DEFAULT_SIZE;
    Some(FloatingConfig {
      title: floating.title.clone(),
      width: if floating.width == 0 {
        width
      } else {
        floating.width
      },
      height: if floating.height == 0 {
        height
      } else {
        floating.height
      },
      min_size: floating.min_size,
      max_size: floating.max_size,
      maximized: floating.maximized,
      fullscreen: floating.fullscreen,
      shown: floating.visible && !floating.dismissed,
    })
  }

  /// The user or compositor asked this floating window to close: its
  /// `on_close` runs, or without one it hides.
  pub fn request_close(&self) {
    let call = {
      let mut tree = borrow(&self.tree);
      let root = tree.root;
      let Some(floating) = tree.floating.as_mut() else {
        return;
      };
      let handler = untrack(|| floating.on_close.try_with(Option::clone)).flatten();
      let Some(handler) = handler else {
        floating.dismissed = true;
        tree.surface.changed = true;
        return;
      };
      let Some(node) = tree.node(root) else {
        return;
      };
      (node.cx.clone(), Rc::clone(&node.path), handler)
    };
    let (cx, path, handler) = call;
    run_isolated(&path, "on_close", handler, |f| f(&cx));
  }

  /// Takes what the window's widgets asked of it since the last call.
  pub fn take_requests(&self) -> Vec<WindowRequest> {
    let mut tree = borrow(&self.tree);
    tree
      .floating
      .as_mut()
      .map(|floating| std::mem::take(&mut floating.requests))
      .unwrap_or_default()
  }
}

impl Cx {
  /// Lets the user drag the floating window this widget is in, as by a
  /// title bar. Call it from a button's `on_press`, while the pointer
  /// button is held.
  pub fn start_move(&self) {
    self.request(WindowRequest::Move);
  }

  /// Lets the user resize the floating window this widget is in from
  /// `edges`, such as `Edge::Bottom | Edge::Right`. Call it from a
  /// button's `on_press`.
  pub fn start_resize(&self, edges: impl Into<Anchors>) {
    self.request(WindowRequest::Resize(edges.into()));
  }

  /// Asks to minimize the floating window this widget is in.
  pub fn minimize(&self) {
    self.request(WindowRequest::Minimize);
  }

  fn request(&self, request: WindowRequest) {
    let Some((tree, _)) = self.anchor.as_ref().and_then(|anchor| anchor.get()) else {
      return tracing::warn!("{request:?} needs a widget's context in a floating window");
    };
    let mut tree = borrow(&tree);
    match tree.floating.as_mut() {
      Some(floating) => floating.requests.push(request),
      None => tracing::warn!("{request:?} works only in a floating window"),
    }
  }
}

impl Tree {
  /// Applies a floating window's own properties and adds its child.
  fn mount_floating(
    &mut self,
    window: FloatingWindow,
    child: Option<crate::widgets::Element>,
    on_close: Signal<Option<CloseHandler>>,
  ) {
    let (width, height) = FloatingWindow::DEFAULT_SIZE;
    self.floating = Some(FloatingState {
      title: String::new(),
      width,
      height,
      min_size: (0, 0),
      max_size: (0, 0),
      maximized: false,
      fullscreen: false,
      visible: true,
      dismissed: false,
      on_close,
      requests: Vec::new(),
    });
    let root = self.root;
    let FloatingWindow {
      title,
      width,
      height,
      min_size,
      max_size,
      maximized,
      fullscreen,
      visible,
      hints,
      on_close: _,
      style,
      child: _,
    } = window;
    self.bind_opt(root, title, "title", |tree, _, value| {
      tree.set_floating(|f| &mut f.title, value);
    });
    self.bind_opt(root, width, "width", |tree, _, value| {
      tree.set_floating(|f| &mut f.width, value);
    });
    self.bind_opt(root, height, "height", |tree, _, value| {
      tree.set_floating(|f| &mut f.height, value);
    });
    self.bind_opt(root, min_size, "min_size", |tree, _, value| {
      tree.set_floating(|f| &mut f.min_size, value);
    });
    self.bind_opt(root, max_size, "max_size", |tree, _, value| {
      tree.set_floating(|f| &mut f.max_size, value);
    });
    self.bind_opt(root, maximized, "maximized", |tree, _, value| {
      tree.set_floating(|f| &mut f.maximized, value);
    });
    self.bind_opt(root, fullscreen, "fullscreen", |tree, _, value| {
      tree.set_floating(|f| &mut f.fullscreen, value);
    });
    self.bind_opt(root, visible, "visible", |tree, _, value| {
      // Hiding forgets a close, so showing again shows.
      if !value {
        tree.set_floating(|f| &mut f.dismissed, false);
      }
      tree.set_floating(|f| &mut f.visible, value);
    });
    self.bind_compositor_hints(hints);
    self.mount_root(style, child);
    self.surface.changed = true;
  }

  /// Sets one floating setting and remembers that the settings changed.
  fn set_floating<T: PartialEq>(
    &mut self,
    field: impl FnOnce(&mut FloatingState) -> &mut T,
    value: T,
  ) {
    let Some(floating) = self.floating.as_mut() else {
      return;
    };
    let slot = field(floating);
    if *slot != value {
      *slot = value;
      self.surface.changed = true;
    }
  }
}
