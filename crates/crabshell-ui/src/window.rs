//! [`PanelWindow`], the description of a bar, and [`Window`], the live tree
//! built from it that the platform lays out, draws and sends input to.

use std::cell::RefCell;
use std::ops::BitOr;
use std::rc::Rc;

use crabshell_reactive::{Owner, Runtime, Signal, untrack};

use crate::cx::{Anchor, Cx};
use crate::geometry::{Rect, Size};
use crate::input::Handler;
use crate::keyboard::{KeyEvent, KeyHandler};
use crate::prop::{IntoProp, Prop};
use crate::region::Region;
use crate::style::Style;
use crate::text::{TextSystem, fonts_added};
use crate::tree::{Tree, borrow};
use crate::view::WindowView;
use crate::widgets::Widget;

/// A monitor, as the compositor names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
  name: String,
}

impl Output {
  /// An output called `name`, such as `"DP-1"`. The platform creates
  /// these from the compositor's announcements.
  pub fn new(name: impl Into<String>) -> Output {
    Output { name: name.into() }
  }

  /// The output's name, such as `"DP-1"`. It is an owned `String` so a
  /// `move` closure can keep it.
  pub fn name(&self) -> String {
    self.name.clone()
  }
}

/// An edge of the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
  /// The top edge.
  Top,
  /// The bottom edge.
  Bottom,
  /// The left edge.
  Left,
  /// The right edge.
  Right,
}

impl Edge {
  const fn bit(self) -> u8 {
    match self {
      Edge::Top => 1,
      Edge::Bottom => 2,
      Edge::Left => 4,
      Edge::Right => 8,
    }
  }
}

/// The edges a window is anchored to. Combine edges with `|`:
/// `Edge::Top | Edge::Left | Edge::Right`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Anchors(u8);

impl Anchors {
  /// Anchored to no edge: the compositor centers the window.
  pub const NONE: Anchors = Anchors(0);

  /// Whether `edge` is one of the anchors.
  pub const fn contains(self, edge: Edge) -> bool {
    self.0 & edge.bit() != 0
  }

  /// The edge the window sits against and reserves space along: the only
  /// anchor, or the one anchor whose two neighbors are also anchors (`Top`
  /// for `Top | Left | Right`). `None` for a corner or no clear edge.
  pub fn edge(self) -> Option<Edge> {
    let [top, bottom, left, right] =
      [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right].map(|edge| self.contains(edge));
    match (top, bottom, left, right) {
      (true, false, false, false) | (true, false, true, true) => Some(Edge::Top),
      (false, true, false, false) | (false, true, true, true) => Some(Edge::Bottom),
      (false, false, true, false) | (true, true, true, false) => Some(Edge::Left),
      (false, false, false, true) | (true, true, false, true) => Some(Edge::Right),
      _ => None,
    }
  }
}

impl From<Edge> for Anchors {
  fn from(edge: Edge) -> Anchors {
    Anchors(edge.bit())
  }
}

impl BitOr for Edge {
  type Output = Anchors;

  fn bitor(self, other: Edge) -> Anchors {
    Anchors(self.bit() | other.bit())
  }
}

impl BitOr<Edge> for Anchors {
  type Output = Anchors;

  fn bitor(self, edge: Edge) -> Anchors {
    Anchors(self.0 | edge.bit())
  }
}

/// The layer-shell layer a window is drawn in, from bottom to top.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
  /// Below everything, with the wallpaper.
  Background,
  /// Below windows.
  Bottom,
  /// Above windows. The default.
  #[default]
  Top,
  /// Above everything, fullscreen windows included.
  Overlay,
}

/// Whether and when a window takes the keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyboardFocus {
  /// Never. The default.
  #[default]
  None,
  /// Always, while the window is in the top or overlay layer: nothing
  /// else gets keys. For launchers and other windows that hold the
  /// keyboard until they close.
  Exclusive,
  /// When the user clicks it, like an ordinary window. A compositor older
  /// than layer-shell version 4 can't do this, and the window then takes
  /// no keyboard focus.
  OnDemand,
}

/// Space between a window and the output edges it is anchored to, in
/// logical pixels. Edges the window isn't anchored to ignore theirs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Margins {
  /// From the top edge.
  pub top: i32,
  /// From the right edge.
  pub right: i32,
  /// From the bottom edge.
  pub bottom: i32,
  /// From the left edge.
  pub left: i32,
}

impl Margins {
  /// The same margin on every edge.
  pub const fn all(margin: i32) -> Margins {
    Margins::new(margin, margin, margin, margin)
  }

  /// Each edge's margin, clockwise from the top, as in CSS.
  pub const fn new(top: i32, right: i32, bottom: i32, left: i32) -> Margins {
    Margins {
      top,
      right,
      bottom,
      left,
    }
  }

  /// The margin on `edge`.
  pub const fn on(self, edge: Edge) -> i32 {
    match edge {
      Edge::Top => self.top,
      Edge::Right => self.right,
      Edge::Bottom => self.bottom,
      Edge::Left => self.left,
    }
  }
}

/// A bar: a layer-shell surface on one output with one child.
///
/// This is a description; [`Window::new`] builds it. By default it takes
/// no keyboard focus. Every property takes a value or a closure.
///
/// ```
/// use crabshell_ui::{Edge, Output, PanelWindow, Text};
///
/// let output = Output::new("DP-1");
/// let bar = PanelWindow::new(&output)
///     .anchors(Edge::Top | Edge::Left | Edge::Right)
///     .height(30)
///     .child(Text::new("hello"));
/// ```
pub struct PanelWindow {
  pub(crate) output: Output,
  pub(crate) anchors: Option<Prop<Anchors>>,
  pub(crate) width: Option<Prop<u32>>,
  pub(crate) height: Option<Prop<u32>>,
  pub(crate) exclusive_zone: Option<Prop<i32>>,
  pub(crate) layer: Option<Prop<Layer>>,
  pub(crate) margins: Option<Prop<Margins>>,
  pub(crate) keyboard_focus: Option<Prop<KeyboardFocus>>,
  pub(crate) namespace: Option<Prop<String>>,
  pub(crate) hints: Hints,
  pub(crate) on_key: Option<KeyHandler>,
  pub(crate) style: Option<Prop<Style>>,
  pub(crate) child: Option<Box<dyn Widget>>,
}

impl PanelWindow {
  /// A window on `output`, anchored to no edge, in the top layer.
  pub fn new(output: &Output) -> PanelWindow {
    PanelWindow {
      output: output.clone(),
      anchors: None,
      width: None,
      height: None,
      exclusive_zone: None,
      layer: None,
      margins: None,
      keyboard_focus: None,
      namespace: None,
      hints: Hints::default(),
      on_key: None,
      style: None,
      child: None,
    }
  }

  /// Sets the edges the window is anchored to.
  pub fn anchors(mut self, anchors: impl IntoProp<Anchors>) -> PanelWindow {
    self.anchors = Some(anchors.into_prop());
    self
  }

  /// Sets the width in logical pixels. 0, the default, lets the compositor
  /// choose, which it does when the window is anchored left and right.
  pub fn width(mut self, width: impl IntoProp<u32>) -> PanelWindow {
    self.width = Some(width.into_prop());
    self
  }

  /// Sets the height in logical pixels. 0, the default, lets the
  /// compositor choose, which it does when the window is anchored top and
  /// bottom.
  pub fn height(mut self, height: impl IntoProp<u32>) -> PanelWindow {
    self.height = Some(height.into_prop());
    self
  }

  /// Sets how much space, in logical pixels, other windows keep clear
  /// along the anchored edge. The default is the window's height for a top
  /// or bottom bar and its width for a left or right one. wlroots
  /// compositors add the margin on that edge to the zone. 0 keeps clear
  /// of other windows' zones without reserving any; -1 ignores them and
  /// covers them.
  pub fn exclusive_zone(mut self, zone: impl IntoProp<i32>) -> PanelWindow {
    self.exclusive_zone = Some(zone.into_prop());
    self
  }

  /// Sets the layer.
  pub fn layer(mut self, layer: impl IntoProp<Layer>) -> PanelWindow {
    self.layer = Some(layer.into_prop());
    self
  }

  /// Sets the space between the window and the edges it is anchored to.
  pub fn margins(mut self, margins: impl IntoProp<Margins>) -> PanelWindow {
    self.margins = Some(margins.into_prop());
    self
  }

  /// Sets whether the window takes the keyboard. The default is never.
  pub fn keyboard_focus(mut self, focus: impl IntoProp<KeyboardFocus>) -> PanelWindow {
    self.keyboard_focus = Some(focus.into_prop());
    self
  }

  /// Sets the layer-shell namespace, which compositors use to tell
  /// windows apart, as in Sway's `layer_effects`. The default is
  /// `"crabshell"`, which also replaces a name holding a NUL byte.
  /// Compositors read it once, so a change after the window is shown
  /// applies only when it is shown again, as on a new output.
  pub fn namespace(mut self, namespace: impl IntoProp<String>) -> PanelWindow {
    self.namespace = Some(namespace.into_prop());
    self
  }

  /// Sets the part of the window that takes pointer input; clicks outside
  /// it reach whatever is below. `None`, the default, is the whole window,
  /// and [`Region::empty`] lets every click through.
  pub fn mask(mut self, mask: impl IntoProp<Option<Region>>) -> PanelWindow {
    self.hints.mask = Some(mask.into_prop());
    self
  }

  /// Asks the compositor to blur what is behind this part of the window,
  /// so a translucent background looks frosted. `None`, the default,
  /// blurs nothing. It needs `ext-background-effect-v1`; a compositor
  /// without it shows the window unblurred, and the shell logs that once.
  ///
  /// ```
  /// use crabshell_ui::{Output, PanelWindow, Region};
  ///
  /// // The whole of a 30 pixel bar, however wide the output.
  /// let bar = PanelWindow::new(&Output::new("DP-1"))
  ///     .height(30)
  ///     .blur(Some(Region::rect(0, 0, u32::MAX, 30)));
  /// ```
  pub fn blur(mut self, region: impl IntoProp<Option<Region>>) -> PanelWindow {
    self.hints.blur = Some(region.into_prop());
    self
  }

  /// Keeps the screen from blanking or locking for idleness while `true`
  /// and the window is shown, as a video player does. It needs
  /// `idle-inhibit-unstable-v1`; without it, that is logged once.
  pub fn inhibit_idle(mut self, inhibit: impl IntoProp<bool>) -> PanelWindow {
    self.hints.inhibit_idle = Some(inhibit.into_prop());
    self
  }

  /// While `true` and the window has the keyboard, the compositor's own
  /// shortcuts, such as Sway's `bindsym`s, go to the window as keys
  /// instead, as a remote desktop or a game wants. The compositor may
  /// keep some, such as one that takes them back. It needs
  /// `keyboard-shortcuts-inhibit-unstable-v1`; without it, that is logged
  /// once.
  pub fn inhibit_shortcuts(mut self, inhibit: impl IntoProp<bool>) -> PanelWindow {
    self.hints.inhibit_shortcuts = Some(inhibit.into_prop());
    self
  }

  /// The compositor writes `true` to `signal` while it honors
  /// [`inhibit_shortcuts`](Self::inhibit_shortcuts), and `false` once it
  /// stops, as when the user takes its shortcuts back, or the window
  /// stops asking or goes. Give each window a signal of its own.
  pub fn shortcuts_inhibited(mut self, signal: Signal<bool>) -> PanelWindow {
    self.hints.shortcuts_inhibited = Some(signal);
    self
  }

  /// While `signal` is `true`, this window and the shown windows given
  /// the same signal, with the popups they open, hold the keyboard and
  /// pointer focus: a click anywhere else ends the grab, and the
  /// compositor writes `false` to `signal`, as it does when every one of
  /// them hides. Writing `false` ends the grab, and `true` asks again. It
  /// needs Hyprland's `hyprland-focus-grab-v1`; without it, that is
  /// logged once and `signal` is left as it is.
  pub fn focus_grab(mut self, signal: Signal<bool>) -> PanelWindow {
    self.hints.focus_grab = Some(signal);
    self
  }

  /// Sets the opacity the compositor shows the whole window at, from 0.0,
  /// unseen, to 1.0, the default. It fades the blur behind the window
  /// along with its pixels. A style's `opacity` is different: Crabshell
  /// paints that into the window's pixels, and a blur behind them stays
  /// as strong. A value outside 0.0 to 1.0 is clamped and NaN reads as
  /// 1.0, logged once with the window's path. It needs Hyprland's
  /// `hyprland-surface-v1`, from Hyprland 0.47; without it, that is
  /// logged once and the window shows as painted.
  pub fn compositor_opacity(mut self, opacity: impl IntoProp<f32>) -> PanelWindow {
    self.hints.compositor_opacity = Some(opacity.into_prop());
    self
  }

  /// Tells the compositor which part of the window holds anything to
  /// see, so it can skip drawing the rest, which helps most when the
  /// window is blurred. It is only a hint, and it must cover every pixel
  /// that isn't fully transparent: what it leaves out may not show.
  /// `None`, the default, is the whole window, and so is a region with
  /// nothing inside the window. It needs version 2 of Hyprland's
  /// `hyprland-surface-v1`; without it, that is logged once.
  pub fn visible_mask(mut self, region: impl IntoProp<Option<Region>>) -> PanelWindow {
    self.hints.visible_mask = Some(region.into_prop());
    self
  }

  /// Runs `handler` for each key the window gets that its focused button
  /// didn't use. It returns `true` when it used the key; otherwise Tab,
  /// Enter and Space do what they do by default. The window gets keys
  /// only with a [`keyboard_focus`](Self::keyboard_focus).
  pub fn on_key(mut self, handler: impl Fn(&Cx, &KeyEvent) -> bool + 'static) -> PanelWindow {
    self.on_key = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Sets the window's style. Its color and font are inherited by every
  /// [`Text`](crate::Text) in the window that doesn't set its own.
  pub fn style(mut self, style: impl IntoProp<Style>) -> PanelWindow {
    self.style = Some(style.into_prop());
    self
  }

  /// Sets the window's one child, which fills the window.
  pub fn child(mut self, child: impl Widget) -> PanelWindow {
    self.child = Some(Box::new(child));
    self
  }
}

/// What a window asks of the compositor besides its layer-shell, toplevel
/// or popup settings. A popup sets only its mask, compositor opacity and
/// visible mask.
#[derive(Default)]
pub(crate) struct Hints {
  pub(crate) mask: Option<Prop<Option<Region>>>,
  pub(crate) compositor_opacity: Option<Prop<f32>>,
  pub(crate) visible_mask: Option<Prop<Option<Region>>>,
  pub(crate) blur: Option<Prop<Option<Region>>>,
  pub(crate) inhibit_idle: Option<Prop<bool>>,
  pub(crate) inhibit_shortcuts: Option<Prop<bool>>,
  pub(crate) shortcuts_inhibited: Option<Signal<bool>>,
  pub(crate) focus_grab: Option<Signal<bool>>,
}

/// What the platform needs to set up the window's layer surface.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceConfig {
  /// The anchored edges.
  pub anchors: Anchors,
  /// The requested width; 0 lets the compositor choose.
  pub width: u32,
  /// The requested height; 0 lets the compositor choose.
  pub height: u32,
  /// The exclusive zone, with the default worked out.
  pub exclusive_zone: i32,
  /// The layer.
  pub layer: Layer,
  /// The margins from the anchored edges.
  pub margins: Margins,
  /// Whether the window takes the keyboard.
  pub keyboard_focus: KeyboardFocus,
  /// The layer-shell namespace.
  pub namespace: String,
  /// The part that takes pointer input; `None` is all of it.
  pub mask: Option<Region>,
}

impl Default for SurfaceConfig {
  /// A layer-shell surface's defaults: no anchors, sizes or zone, in the
  /// top layer, without the keyboard.
  fn default() -> SurfaceConfig {
    SurfaceConfig {
      anchors: Anchors::NONE,
      width: 0,
      height: 0,
      exclusive_zone: 0,
      layer: Layer::Top,
      margins: Margins::default(),
      keyboard_focus: KeyboardFocus::None,
      namespace: String::from(DEFAULT_NAMESPACE),
      mask: None,
    }
  }
}

/// The layer-shell namespace unless a window sets its own.
pub(crate) const DEFAULT_NAMESPACE: &str = "crabshell";

/// A live window: the tree of nodes built from a [`PanelWindow`].
///
/// The platform owns one per layer surface. Each event-loop iteration it
/// runs the reactive flush, then calls [`layout`](Self::layout) if
/// [`needs_layout`](Self::needs_layout), then draws
/// [`take_damage`](Self::take_damage) from [`view`](Self::view). When
/// nothing changed, there is nothing to lay out or draw.
///
/// Dropping the window disposes its owner, which frees every node,
/// binding and handler in it.
pub struct Window {
  pub(crate) tree: Rc<RefCell<Tree>>,
  pub(crate) owner: Owner,
}

impl Window {
  /// Builds `panel` under a new child owner of `cx`.
  ///
  /// Properties set to values take effect now. Bindings are effects, so
  /// they first run at the next [`Runtime::flush`]; run it before the
  /// first layout.
  ///
  /// [`Runtime::flush`]: crabshell_reactive::Runtime::flush
  pub fn new(cx: &Cx, mut panel: PanelWindow) -> Window {
    // Describe the whole tree first: that runs the user's `into_element`
    // code, which must not see the tree borrowed or subscribe whatever
    // effect is building this window.
    let child = untrack(|| panel.child.take().map(|child| child.into_element()));
    let path: Rc<str> = format!("PanelWindow[{}]", panel.output.name).into();
    let cx = cx
      .child()
      .named(path.to_string())
      .at(Rc::new(Anchor::default()));
    let owner = cx.owner();
    let output = panel.output.clone();
    let tree =
      Rc::new_cyclic(|this| RefCell::new(Tree::new(this.clone(), "PanelWindow", output, cx, path)));
    borrow(&tree).mount_panel(panel, child);
    Window { tree, owner }
  }

  /// The output the window is on.
  pub fn output(&self) -> Output {
    self.tree.borrow().output.clone()
  }

  /// The layer-surface settings. Read them only after the first
  /// [`Runtime::flush`]: settings given as closures are bindings, which
  /// first run then, and until they do these are the defaults.
  ///
  /// [`Runtime::flush`]: crabshell_reactive::Runtime::flush
  pub fn surface(&self) -> SurfaceConfig {
    let tree = self.tree.borrow();
    let surface = &tree.surface;
    let exclusive_zone = surface.exclusive_zone.unwrap_or_else(|| {
      let thickness = match surface.anchors.edge() {
        Some(Edge::Top | Edge::Bottom) => surface.height,
        Some(Edge::Left | Edge::Right) => surface.width,
        None => 0,
      };
      i32::try_from(thickness).unwrap_or(i32::MAX)
    });
    SurfaceConfig {
      anchors: surface.anchors,
      width: surface.width,
      height: surface.height,
      exclusive_zone,
      layer: surface.layer,
      margins: surface.margins,
      keyboard_focus: surface.keyboard_focus,
      namespace: surface.namespace.clone(),
      mask: surface.mask.clone(),
    }
  }

  /// The region a panel or floating window asks the compositor to blur
  /// behind, as its `blur` property says.
  pub fn blur(&self) -> Option<Region> {
    self.tree.borrow().surface.blur.clone()
  }

  /// Whether a panel or floating window asks to keep the screen from
  /// idling, as its `inhibit_idle` property says.
  pub fn inhibits_idle(&self) -> bool {
    self.tree.borrow().surface.inhibit_idle
  }

  /// Whether a panel or floating window asks for the compositor's
  /// shortcuts while it has the keyboard, as its `inhibit_shortcuts`
  /// property says.
  pub fn inhibits_shortcuts(&self) -> bool {
    self.tree.borrow().surface.inhibit_shortcuts
  }

  /// The signal a panel or floating window was given with
  /// `shortcuts_inhibited`, which the platform writes.
  pub fn shortcuts_inhibited(&self) -> Option<Signal<bool>> {
    self.tree.borrow().surface.shortcuts_inhibited
  }

  /// The signal a panel or floating window was given with `focus_grab`:
  /// windows given the same one share a grab.
  pub fn focus_grab(&self) -> Option<Signal<bool>> {
    self.tree.borrow().surface.focus_grab
  }

  /// The opacity the window asks the compositor to show it at, as its
  /// `compositor_opacity` property says: always from 0.0 to 1.0.
  pub fn compositor_opacity(&self) -> f32 {
    self.tree.borrow().surface.compositor_opacity
  }

  /// The part of the window it tells the compositor holds anything to
  /// see, as its `visible_mask` property says; `None` is all of it.
  pub fn visible_mask(&self) -> Option<Region> {
    self.tree.borrow().surface.visible_mask.clone()
  }

  /// Whether the settings in [`surface`](Self::surface) changed since the
  /// last call. The first call returns `true`, even before the first flush,
  /// when [`surface`](Self::surface) still holds the defaults.
  pub fn take_surface_changed(&self) -> bool {
    std::mem::take(&mut borrow(&self.tree).surface.changed)
  }

  /// Whether the settings changed since the last
  /// [`take_surface_changed`](Self::take_surface_changed), leaving that
  /// to tell.
  pub fn surface_changed(&self) -> bool {
    self.tree.borrow().surface.changed
  }

  /// The window's size in logical pixels: the size the compositor gave a
  /// panel, or the size a popup's content wants within any limit, from
  /// its last layout.
  pub fn size(&self) -> Size {
    self.tree.borrow().size
  }

  /// Sets the size the compositor gave the window, from a configure
  /// event. A new size needs a layout.
  pub fn set_size(&self, size: Size) {
    let mut tree = borrow(&self.tree);
    if tree.size != size {
      tree.size = size;
      tree.needs_layout = true;
    }
  }

  /// Whether something changed that needs [`layout`](Self::layout).
  pub fn needs_layout(&self) -> bool {
    let tree = self.tree.borrow();
    tree.needs_layout || tree.needs_place || tree.fonts_added != fonts_added()
  }

  /// Lays the window out at its size, measuring text with `text`, and
  /// records the areas that moved or changed as damage.
  ///
  /// Then it writes each moved widget's `geometry` signal, tells lists
  /// their rows' sizes and runs the effects that follow, which may change the layout, and lays out
  /// again, until nothing moves or 8 layouts have run.
  pub fn layout(&self, text: &mut TextSystem) {
    for _ in 0..GEOMETRY_ROUNDS {
      let (moved, measured, offsets) = {
        let mut tree = borrow(&self.tree);
        // Both borrows end before the effects below run.
        tree.layout(&mut text.fonts());
        (
          std::mem::take(&mut tree.geometry),
          std::mem::take(&mut tree.measured),
          std::mem::take(&mut tree.offsets),
        )
      };
      // Nothing new to tell, so no effects run here.
      if moved.is_empty() && measured.is_empty() && offsets.is_empty() {
        return;
      }
      // Written with the tree released, so an effect may lay it out.
      for (signal, rect) in moved {
        signal.try_set(rect);
      }
      for (signal, offset) in offsets {
        signal.try_set(offset);
      }
      for (measure, size) in measured {
        measure(size);
      }
      Runtime::current().flush();
      if !self.needs_layout() {
        return;
      }
    }
    let tree = self.tree.borrow();
    let path = tree.node(tree.root).map_or("", |node| &node.path);
    tracing::warn!(
      "{path} still moved after {GEOMETRY_ROUNDS} layouts that wrote `geometry`, so it is laid \
       out again at the next event; check the effects that size widgets from it"
    );
  }

  /// Whether any area needs repainting.
  pub fn has_damage(&self) -> bool {
    !self.tree.borrow().damage.is_empty()
  }

  /// Takes the areas to repaint since the last call, in logical pixels.
  /// They may overlap.
  pub fn take_damage(&self) -> Vec<Rect> {
    std::mem::take(&mut borrow(&self.tree).damage)
  }

  /// A read-only view of the laid-out tree, for drawing. Release it before
  /// the next flush or pointer event.
  pub fn view(&self) -> WindowView<'_> {
    WindowView::new(self.tree.borrow())
  }
}

/// The most layouts one [`Window::layout`] runs while effects reading
/// `geometry` keep moving widgets.
const GEOMETRY_ROUNDS: usize = 8;

impl Drop for Window {
  fn drop(&mut self) {
    // A `hovered` or `shown` signal owned elsewhere mustn't stay `true`,
    // and a drag on a window going away ends without running anything.
    self.unmapped();
    let mut tree = borrow(&self.tree);
    tree.access_event(|_| crate::AccessEvent::WindowsChanged);
    drop(tree.leave());
    // A drag out with the platform outlives `leave`; nothing of it runs.
    if let Some(press) = tree.pointer.pressed.take() {
      press.write(false);
    }
    drop(tree);
    self.owner.dispose();
  }
}
