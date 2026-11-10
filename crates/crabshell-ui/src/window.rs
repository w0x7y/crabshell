//! [`PanelWindow`], the description of a bar, and [`Window`], the live tree
//! built from it that the platform lays out and draws.

use std::cell::RefCell;
use std::ops::BitOr;
use std::rc::Rc;

use crabshell_reactive::{Owner, untrack};

use crate::cx::Cx;
use crate::geometry::{Rect, Size};
use crate::prop::{IntoProp, Prop};
use crate::style::Style;
use crate::text::TextSystem;
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

/// A bar: a layer-shell surface on one output with one child.
///
/// This is a description; [`Window::new`] builds it. It takes no keyboard
/// focus. Every property takes a value or a closure.
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
    /// or bottom bar and its width for a left or right one.
    pub fn exclusive_zone(mut self, zone: impl IntoProp<i32>) -> PanelWindow {
        self.exclusive_zone = Some(zone.into_prop());
        self
    }

    /// Sets the layer.
    pub fn layer(mut self, layer: impl IntoProp<Layer>) -> PanelWindow {
        self.layer = Some(layer.into_prop());
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

/// What the platform needs to set up the window's layer surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

/// A live window: the tree of nodes built from a [`PanelWindow`].
///
/// The platform owns one per layer surface. Each event-loop iteration it
/// runs the reactive flush, then calls [`layout`](Self::layout) if
/// [`needs_layout`](Self::needs_layout), then draws
/// [`take_damage`](Self::take_damage) from [`view`](Self::view). When
/// nothing changed, there is nothing to lay out or draw.
///
/// Dropping the window disposes its owner, which frees every node and
/// binding in it.
pub struct Window {
    pub(crate) tree: Rc<RefCell<Tree>>,
    owner: Owner,
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
        let cx = cx.child().named(path.to_string());
        let owner = cx.owner();
        let output = panel.output.clone();
        let tree = Rc::new_cyclic(|this| RefCell::new(Tree::new(this.clone(), output, cx, path)));
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
        }
    }

    /// Whether the settings in [`surface`](Self::surface) changed since the
    /// last call. The first call returns `true`, even before the first flush,
    /// when [`surface`](Self::surface) still holds the defaults.
    pub fn take_surface_changed(&self) -> bool {
        std::mem::take(&mut borrow(&self.tree).surface.changed)
    }

    /// The size the compositor gave the window, in logical pixels.
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
        self.tree.borrow().needs_layout
    }

    /// Lays the window out at its size, measuring text with `text`, and
    /// records the areas that moved or changed as damage.
    pub fn layout(&self, text: &mut TextSystem) {
        borrow(&self.tree).layout(text);
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
    /// the next flush.
    pub fn view(&self) -> WindowView<'_> {
        WindowView::new(self.tree.borrow())
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.owner.dispose();
    }
}
