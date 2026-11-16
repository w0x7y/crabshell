//! Paths: the shapes they are built from.

use crate::{Path, PathCommand, Point, Rect};

#[test]
fn shapes_are_built_from_plain_commands() {
  assert_eq!(
    Path::rect(Rect::new(1.0, 2.0, 3.0, 4.0)).commands(),
    [
      PathCommand::MoveTo(Point::new(1.0, 2.0)),
      PathCommand::LineTo(Point::new(4.0, 2.0)),
      PathCommand::LineTo(Point::new(4.0, 6.0)),
      PathCommand::LineTo(Point::new(1.0, 6.0)),
      PathCommand::Close,
    ]
  );
  // The radius is held to half the shorter side.
  let pill = Path::rounded_rect(Rect::new(0.0, 0.0, 40.0, 10.0), 50.0);
  assert!(pill.commands().contains(&PathCommand::Arc {
    center: Point::new(35.0, 5.0),
    radius: 5.0,
    start: -90.0,
    sweep: 90.0,
  }));
  assert_eq!(
    Path::rounded_rect(Rect::new(0.0, 0.0, 4.0, 4.0), 0.0),
    Path::rect(Rect::new(0.0, 0.0, 4.0, 4.0))
  );
}
