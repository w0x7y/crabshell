//! Input regions: shapes, the operations joining them, and cutting them to
//! the window.

use crate::Region;

#[test]
fn a_rect_is_itself_cut_to_the_window() {
  assert_eq!(
    Region::rect(10, 5, 20, 10).rects(200, 30),
    [[10, 5, 20, 10]]
  );
  assert_eq!(
    Region::rect(-10, -10, 50, 20).rects(30, 30),
    [[0, 0, 30, 10]]
  );
  assert_eq!(
    Region::rect(300, 0, 20, 10).rects(200, 30),
    Vec::<[i32; 4]>::new()
  );
  assert_eq!(Region::empty().rects(200, 30), Vec::<[i32; 4]>::new());
}

#[test]
fn rects_join_union_subtract_intersect_and_xor() {
  let a = || Region::rect(0, 0, 20, 10);
  let b = || Region::rect(10, 0, 20, 10);
  assert_eq!(a().union(b()).rects(100, 10), [[0, 0, 30, 10]]);
  assert_eq!(a().subtract(b()).rects(100, 10), [[0, 0, 10, 10]]);
  assert_eq!(a().intersect(b()).rects(100, 10), [[10, 0, 10, 10]]);
  assert_eq!(
    a().xor(b()).rects(100, 10),
    [[0, 0, 10, 10], [20, 0, 10, 10]]
  );
  // In call order: (a + b) - a.
  assert_eq!(
    a().union(b()).subtract(a()).rects(100, 10),
    [[20, 0, 10, 10]]
  );
  // A hole splits the rows into three bands.
  let holed = Region::rect(0, 0, 30, 30).subtract(Region::rect(10, 10, 10, 10));
  assert_eq!(
    holed.rects(100, 100),
    [
      [0, 0, 30, 10],
      [0, 10, 10, 10],
      [20, 10, 10, 10],
      [0, 20, 30, 10]
    ]
  );
}

/// The covered columns on each row of `region` in a 100 by 100 window.
fn widths(region: &Region) -> Vec<i32> {
  let mut widths = vec![0; 100];
  for [_, y, width, height] in region.rects(100, 100) {
    for row in &mut widths[y as usize..(y + height) as usize] {
      *row += width;
    }
  }
  widths
}

#[test]
fn an_ellipse_is_widest_at_its_middle_and_symmetric() {
  let widths = widths(&Region::ellipse(0, 0, 40, 20));
  assert_eq!(widths[10], 40);
  assert!(widths[0] > 0 && widths[0] < 20, "{widths:?}");
  assert_eq!(
    widths[..20],
    widths[..20].iter().rev().copied().collect::<Vec<_>>()[..]
  );
  assert!(widths[20..].iter().all(|width| *width == 0));
}

#[test]
fn rounded_corners_cut_only_their_rows_and_sides() {
  let rounded = widths(&Region::rect(0, 0, 40, 40).radii(10.0, 0.0, 0.0, 0.0));
  assert!(rounded[0] < 40 && rounded[0] > 30, "{rounded:?}");
  assert!(
    rounded[10..40].iter().all(|width| *width == 40),
    "{rounded:?}"
  );
  let rects = Region::rect(0, 0, 40, 40)
    .radii(10.0, 0.0, 0.0, 0.0)
    .rects(100, 100);
  // Only the left edge moves: every rect ends at the right side.
  assert!(
    rects.iter().all(|[x, _, width, _]| x + width == 40),
    "{rects:?}"
  );
  // A radius larger than half a side is clamped, so a square becomes a
  // circle, not something wider.
  assert_eq!(
    widths(&Region::rect(0, 0, 20, 20).radius(50.0)),
    widths(&Region::ellipse(0, 0, 20, 20))
  );
  // An ellipse ignores radii.
  assert_eq!(
    Region::ellipse(0, 0, 20, 20).radius(5.0),
    Region::ellipse(0, 0, 20, 20)
  );
}

#[test]
fn huge_shapes_stop_at_the_window() {
  let region = Region::rect(-1_000_000, -1_000_000, u32::MAX, u32::MAX).union(Region::ellipse(
    0,
    0,
    u32::MAX,
    u32::MAX,
  ));
  assert_eq!(region.rects(50, 20), [[0, 0, 50, 20]]);
}
