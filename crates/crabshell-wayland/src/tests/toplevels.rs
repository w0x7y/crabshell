//! Floating windows' limits and edges, and popups' placement, as the
//! protocol takes them.

use crabshell_ui::{Adjust, Anchors, Edge};
use wayland_protocols::xdg::shell::client::xdg_positioner::{
  Anchor, ConstraintAdjustment, Gravity,
};
use wayland_protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;

use crate::popups::{adjustment, corner};
use crate::toplevels::{limits, resize_edge};

#[test]
fn size_limits_never_send_a_protocol_error() {
  assert_eq!(limits((0, 0), (0, 0)), (None, None));
  assert_eq!(
    limits((100, 50), (800, 0)),
    (Some((100, 50)), Some((800, 0)))
  );
  // A maximum below the minimum is raised to it, side by side.
  assert_eq!(limits((400, 0), (300, 0)), (Some((400, 0)), Some((400, 0))));
  // Huge sides are capped.
  assert_eq!(
    limits((u32::MAX, 1), (u32::MAX, 0)),
    (Some((16_384, 1)), Some((16_384, 0)))
  );
}

#[test]
fn edges_map_to_the_protocols_corners_and_opposites_cancel() {
  assert_eq!(
    corner(Edge::Top | Edge::Left),
    (Anchor::TopLeft, Gravity::TopLeft)
  );
  assert_eq!(
    corner(Edge::Bottom.into()),
    (Anchor::Bottom, Gravity::Bottom)
  );
  assert_eq!(
    corner(Edge::Top | Edge::Bottom | Edge::Right),
    (Anchor::Right, Gravity::Right)
  );
  assert_eq!(corner(Anchors::NONE), (Anchor::None, Gravity::None));
  assert_eq!(
    resize_edge(Edge::Bottom | Edge::Right),
    ResizeEdge::BottomRight
  );
  assert_eq!(resize_edge(Edge::Left.into()), ResizeEdge::Left);
  assert_eq!(resize_edge(Edge::Left | Edge::Right), ResizeEdge::None);
}

#[test]
fn adjustments_map_flag_by_flag() {
  assert_eq!(adjustment(Adjust::NONE), ConstraintAdjustment::None);
  assert_eq!(
    adjustment(Adjust::FLIP | Adjust::SLIDE_Y),
    ConstraintAdjustment::FlipX | ConstraintAdjustment::FlipY | ConstraintAdjustment::SlideY
  );
}
