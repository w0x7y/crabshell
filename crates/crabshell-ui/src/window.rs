//! [`PanelWindow`], the description of a bar.

use std::ops::BitOr;

use crate::prop::{IntoProp, Prop};
use crate::style::Style;
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
/// This is a description. It takes no keyboard
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
