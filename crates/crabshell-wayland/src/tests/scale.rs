//! Buffer sizes for whole and fractional scales.

use crate::scale::Scale;

#[test]
fn buffers_are_the_logical_size_times_the_scale_rounded_halfway_away_from_zero() {
  let one_and_a_half = Scale::fractional(180).expect("1.5 is in range");
  assert_eq!(one_and_a_half.buffer_size(1280, 30), Some((1920, 45)));
  // 1501.5 and 46.5 round up.
  assert_eq!(one_and_a_half.buffer_size(1001, 31), Some((1502, 47)));
  // wp_fractional_scale_v1 rounds halfway away from zero, and so does the
  // compositor when it sizes the surface on screen: 41.25 is 41 and
  // 31.25 is 31. A buffer a pixel larger would be scaled down and blur.
  let five_quarters = Scale::fractional(150).expect("1.25 is in range");
  assert_eq!(five_quarters.buffer_size(33, 25), Some((41, 31)));
  // 54.25 rounds down, 50.75 up.
  let seven_quarters = Scale::fractional(210).expect("1.75 is in range");
  assert_eq!(seven_quarters.buffer_size(31, 29), Some((54, 51)));
  // 7/6 times 1200 is exactly 1400, which floating point would miss.
  let seven_sixths = Scale::fractional(140).expect("in range");
  assert_eq!(seven_sixths.buffer_size(1200, 6), Some((1400, 7)));
  let two = Scale::integer(2).expect("2 is in range");
  assert_eq!(two.buffer_size(1920, 30), Some((3840, 60)));
  assert_eq!(Scale::Integer(1).buffer_size(1, 1), Some((1, 1)));
}

#[test]
fn sizes_and_scales_out_of_range_are_refused() {
  assert_eq!(Scale::fractional(0), None);
  assert_eq!(Scale::fractional(961), None);
  assert_eq!(Scale::fractional(960), Some(Scale::Fractional(960)));
  assert_eq!(Scale::integer(0), None);
  assert_eq!(Scale::integer(-1), None);
  assert_eq!(Scale::integer(9), None);
  assert_eq!(Scale::Integer(1).buffer_size(0, 30), None);
  assert_eq!(Scale::Integer(1).buffer_size(30, 0), None);
  assert_eq!(Scale::Integer(1).buffer_size(16_384, 1), Some((16_384, 1)));
  assert_eq!(Scale::Integer(2).buffer_size(8_193, 1), None);
  assert_eq!(Scale::Fractional(960).buffer_size(u32::MAX, 1), None);
  // A side that comes out under half a pixel is 0.
  assert_eq!(Scale::Fractional(1).buffer_size(59, 120), None);
  assert_eq!(Scale::Fractional(1).buffer_size(60, 120), Some((1, 1)));
}

#[test]
fn factors_and_conversions() {
  assert_eq!(Scale::Fractional(180).factor(), 1.5);
  assert_eq!(Scale::Integer(3).factor(), 3.0);
  assert_eq!(Scale::Integer(2).as_fractional(), Scale::Fractional(240));
  assert_eq!(
    Scale::Fractional(150).as_fractional(),
    Scale::Fractional(150)
  );
}
