//! Outputs' current modes, from what the compositor sent.

use smithay_client_toolkit::output;

use crate::outputs::{Mode, current_mode};

fn mode(dimensions: (i32, i32), refresh_rate: i32, current: bool) -> output::Mode {
  output::Mode {
    dimensions,
    refresh_rate,
    current,
    preferred: false,
  }
}

#[test]
fn the_current_mode_is_reported() {
  let modes = [
    mode((1280, 720), 60_000, false),
    mode((1920, 1080), 59_950, true),
  ];
  assert_eq!(
    current_mode(&modes),
    Some(Mode {
      width: 1920,
      height: 1080,
      refresh_mhz: 59_950,
    })
  );
}

#[test]
fn modes_that_make_no_sense_are_left_out() {
  assert_eq!(current_mode(&[]), None);
  assert_eq!(current_mode(&[mode((1920, 1080), 60_000, false)]), None);
  assert_eq!(current_mode(&[mode((0, 1080), 60_000, true)]), None);
  assert_eq!(current_mode(&[mode((1920, -1), 60_000, true)]), None);
  // A virtual output may have no refresh rate.
  assert_eq!(
    current_mode(&[mode((800, 600), -5, true)]).map(|mode| mode.refresh_mhz),
    Some(0)
  );
}
