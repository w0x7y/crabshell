//! Turning logical damage into buffer pixels, and planning what each frame
//! repaints and reports.

use crabshell_ui::Rect;

use crate::damage::{Plan, merge, to_pixels};
use crate::{BufferAge, DamageHistory, PixelRect};

#[test]
fn logical_damage_rounds_outward_with_a_margin() {
  // 1..3 logical is 1.5..4.5 at scale 1.5: pixels 1 to 4, plus one around.
  let rect = to_pixels(Rect::new(1.0, 1.0, 2.0, 2.0), 1.5, 100, 100);
  assert_eq!(rect, Some(PixelRect::new(0, 0, 6, 6)));
  let rect = to_pixels(Rect::new(10.0, 4.0, 5.0, 2.0), 1.0, 100, 100);
  assert_eq!(rect, Some(PixelRect::new(9, 3, 7, 4)));
}

#[test]
fn logical_damage_is_clipped_to_the_buffer() {
  // -10..30 by 80..120 in pixels, plus the margin, cut to 0..100.
  let rect = to_pixels(Rect::new(-5.0, 40.0, 20.0, 20.0), 2.0, 100, 100);
  assert_eq!(rect, Some(PixelRect::new(0, 79, 31, 21)));
  assert_eq!(
    to_pixels(Rect::new(-5.0, 90.0, 20.0, 50.0), 2.0, 100, 100),
    None
  );
  assert_eq!(
    to_pixels(Rect::new(0.0, 0.0, 0.0, 5.0), 1.0, 100, 100),
    None
  );
  assert_eq!(
    to_pixels(Rect::new(200.0, 0.0, 5.0, 5.0), 1.0, 100, 100),
    None
  );
  assert_eq!(
    to_pixels(Rect::new(0.0, 0.0, f32::NAN, 5.0), 1.0, 100, 100),
    None
  );
}

#[test]
fn overlapping_rectangles_merge_and_touching_ones_stay_apart() {
  let merged = merge(vec![
    PixelRect::new(0, 0, 10, 10),
    PixelRect::new(5, 5, 10, 10),
    PixelRect::new(15, 0, 5, 5),
    PixelRect::new(40, 0, 5, 5),
    PixelRect::new(0, 0, 0, 5),
  ]);
  // The first two merge into 0..15, which then overlaps nothing: 15..20
  // only touches it.
  assert_eq!(
    merged,
    vec![
      PixelRect::new(0, 0, 15, 15),
      PixelRect::new(40, 0, 5, 5),
      PixelRect::new(15, 0, 5, 5),
    ]
  );
  // A merge can make a rectangle overlap one it missed before.
  let merged = merge(vec![
    PixelRect::new(20, 0, 5, 5),
    PixelRect::new(0, 0, 5, 5),
    PixelRect::new(3, 0, 20, 2),
  ]);
  assert_eq!(merged, vec![PixelRect::new(0, 0, 25, 5)]);
}

/// Plans a frame of `damage` in a 100 by 20 buffer at scale 1.
fn plan(history: &mut DamageHistory, damage: &[Rect], age: &mut BufferAge) -> Plan {
  history.plan(damage, 1.0, 100, 20, age)
}

const FULL: PixelRect = PixelRect::new(0, 0, 100, 20);

#[test]
fn a_new_buffer_and_the_first_frame_repaint_everything() {
  let mut history = DamageHistory::new();
  let mut age = BufferAge::default();
  let small = [Rect::new(10.0, 5.0, 4.0, 4.0)];
  let first = plan(&mut history, &small, &mut age);
  assert_eq!(first.repaint, vec![FULL]);
  assert_eq!(first.report, vec![FULL]);

  // The same buffer next frame repaints only the damage.
  let second = plan(&mut history, &small, &mut age);
  assert_eq!(second.repaint, vec![PixelRect::new(9, 4, 6, 6)]);
  assert_eq!(second.report, second.repaint);

  // Another, new buffer repaints everything but reports only the damage.
  let third = plan(&mut history, &small, &mut BufferAge::default());
  assert_eq!(third.repaint, vec![FULL]);
  assert_eq!(third.report, vec![PixelRect::new(9, 4, 6, 6)]);
}

#[test]
fn a_buffer_that_skipped_frames_repaints_what_changed_in_them() {
  let mut history = DamageHistory::new();
  let (mut a, mut b) = (BufferAge::default(), BufferAge::default());
  plan(&mut history, &[], &mut a);
  plan(&mut history, &[Rect::new(10.0, 0.0, 4.0, 4.0)], &mut b);
  plan(&mut history, &[Rect::new(30.0, 0.0, 4.0, 4.0)], &mut b);
  // `a` missed the last two frames.
  let frame = plan(&mut history, &[Rect::new(60.0, 0.0, 4.0, 4.0)], &mut a);
  let mut repaint = frame.repaint.clone();
  repaint.sort_by_key(|rect| rect.x);
  assert_eq!(
    repaint,
    vec![
      PixelRect::new(9, 0, 6, 5),
      PixelRect::new(29, 0, 6, 5),
      PixelRect::new(59, 0, 6, 5),
    ]
  );
  assert_eq!(frame.report, vec![PixelRect::new(59, 0, 6, 5)]);
}

#[test]
fn a_buffer_older_than_the_history_repaints_everything() {
  let mut history = DamageHistory::new();
  let mut old = BufferAge::default();
  plan(&mut history, &[], &mut old);
  let mut other = BufferAge::default();
  for _ in 0..6 {
    plan(&mut history, &[Rect::new(10.0, 0.0, 4.0, 4.0)], &mut other);
  }
  let frame = plan(&mut history, &[Rect::new(10.0, 0.0, 4.0, 4.0)], &mut old);
  assert_eq!(frame.repaint, vec![FULL]);
}

#[test]
fn more_than_half_dirty_repaints_the_whole_buffer() {
  let mut history = DamageHistory::new();
  let mut age = BufferAge::default();
  plan(&mut history, &[], &mut age);
  // 48 of 100 columns, plus the margin: 50 columns, exactly half.
  let half = plan(&mut history, &[Rect::new(1.0, 0.0, 48.0, 20.0)], &mut age);
  assert_eq!(half.repaint, vec![PixelRect::new(0, 0, 50, 20)]);
  // One more column tips it over.
  let more = plan(&mut history, &[Rect::new(1.0, 0.0, 49.0, 20.0)], &mut age);
  assert_eq!(more.repaint, vec![FULL]);
  assert_eq!(more.report, vec![FULL]);
}

#[test]
fn missed_frames_can_tip_a_repaint_over_half() {
  let mut history = DamageHistory::new();
  let (mut a, mut b) = (BufferAge::default(), BufferAge::default());
  plan(&mut history, &[], &mut a);
  plan(&mut history, &[Rect::new(0.0, 0.0, 30.0, 20.0)], &mut b);
  let frame = plan(&mut history, &[Rect::new(60.0, 0.0, 30.0, 20.0)], &mut a);
  assert_eq!(frame.repaint, vec![FULL]);
  // Only this frame's change is reported.
  assert_eq!(frame.report, vec![PixelRect::new(59, 0, 32, 20)]);
}

#[test]
fn a_new_size_or_scale_repaints_everything_and_forgets_earlier_frames() {
  let mut history = DamageHistory::new();
  let (mut a, mut b) = (BufferAge::default(), BufferAge::default());
  plan(&mut history, &[], &mut a);
  plan(&mut history, &[], &mut b);
  let bigger = history.plan(&[], 1.0, 120, 20, &mut a);
  assert_eq!(bigger.repaint, vec![PixelRect::new(0, 0, 120, 20)]);
  assert_eq!(bigger.report, bigger.repaint);
  let scaled = history.plan(&[], 2.0, 120, 20, &mut b);
  assert_eq!(scaled.report, vec![PixelRect::new(0, 0, 120, 20)]);
  // `a` last held a frame at scale 1, which the history forgot.
  let next = history.plan(&[], 2.0, 120, 20, &mut a);
  assert_eq!(next.repaint, vec![PixelRect::new(0, 0, 120, 20)]);
  // `b` holds the latest frame, so nothing needs repainting.
  let quiet = history.plan(&[], 2.0, 120, 20, &mut b);
  assert!(
    quiet.repaint.is_empty() && quiet.report.is_empty(),
    "{quiet:?}"
  );
}

#[test]
fn a_buffer_from_another_history_is_repainted_whole() {
  let mut first = DamageHistory::new();
  let mut age = BufferAge::default();
  for _ in 0..10 {
    plan(&mut first, &[], &mut age);
  }
  // The buffer moves to another window, whose history is at frame 3.
  let mut second = DamageHistory::new();
  let mut other = BufferAge::default();
  for _ in 0..3 {
    plan(&mut second, &[], &mut other);
  }
  let frame = plan(&mut second, &[Rect::new(10.0, 0.0, 4.0, 4.0)], &mut age);
  assert_eq!(frame.repaint, vec![FULL]);
}
