//! What outputs say of themselves: their current modes, logical sizes,
//! scales and turns, from what the compositor sent.

use crabshell_render::Turn;
use smithay_client_toolkit::output;
use smithay_client_toolkit::reexports::client::protocol::wl_output::Transform;

use crate::outputs::{
  Mode, OutputInfo, current_mode, fractional_scale, logical_size, replace, turn,
};

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

const UPRIGHT: Turn = Turn {
  quarters: 0,
  flipped: false,
};
const SIDEWAYS: Turn = Turn {
  quarters: 1,
  flipped: false,
};

fn mode_of(width: u32, height: u32) -> Option<Mode> {
  Some(Mode {
    width,
    height,
    refresh_mhz: 60_000,
  })
}

#[test]
fn the_logical_size_is_xdg_outputs_or_the_mode_over_the_whole_scale() {
  let mode = mode_of(1920, 1080);
  assert_eq!(
    logical_size(Some((1280, 720)), mode, UPRIGHT, 2),
    (1280, 720)
  );
  // Without one, the turned mode over the whole scale.
  assert_eq!(logical_size(None, mode, UPRIGHT, 2), (960, 540));
  assert_eq!(logical_size(None, mode, SIDEWAYS, 1), (1080, 1920));
  assert_eq!(logical_size(Some((0, -4)), mode, UPRIGHT, 1), (1920, 1080));
  assert_eq!(logical_size(None, mode, UPRIGHT, 0), (1920, 1080));
  assert_eq!(logical_size(None, None, UPRIGHT, 1), (0, 0));
}

#[test]
fn the_fractional_scale_is_the_one_that_gives_back_the_logical_size() {
  let at = |width, height, logical| fractional_scale(mode_of(width, height), UPRIGHT, logical);
  assert_eq!(at(1920, 1080, (1280, 720)), Some(1.5));
  assert_eq!(at(1920, 1080, (1536, 864)), Some(1.25));
  assert_eq!(at(1920, 1080, (960, 540)), Some(2.0));
  // wlroots cuts 1920 / 1.75 down to 1097 logical pixels; another
  // compositor may round 1366 / 1.5 up to 911, or 1366 / 3.1 up to 441.
  assert_eq!(at(1920, 1080, (1097, 617)), Some(1.75));
  assert_eq!(at(1366, 768, (911, 512)), Some(1.5));
  assert_eq!(at(1366, 768, (441, 248)), Some(3.1));
  // On a small logical size the cut moves the mode over it past the
  // next 120th, so the height decides.
  assert_eq!(at(1280, 800, (426, 266)), Some(3.0));
  assert_eq!(at(1366, 768, (455, 256)), Some(3.0));
  assert_eq!(at(1080, 2340, (392, 850)), Some(2.75));
  assert_eq!(at(720, 1440, (378, 757)), Some(1.9));
  assert_eq!(at(720, 1440, (342, 685)), Some(2.1));
  assert_eq!(at(720, 1440, (293, 587)), Some(2.45));
  assert_eq!(at(720, 1440, (261, 523)), Some(2.75));
  // Turned a quarter, the logical width is the mode's height.
  assert_eq!(
    fractional_scale(mode_of(1280, 720), SIDEWAYS, (480, 853)),
    Some(1.5)
  );
  // Nothing to work from, or a scale no window would take.
  assert_eq!(fractional_scale(None, UPRIGHT, (1280, 720)), None);
  assert_eq!(at(1920, 1080, (0, 0)), None);
  assert_eq!(at(1920, 1080, (1, 1)), None);
}

#[test]
fn only_what_differs_is_told_as_a_change() {
  let mut known = OutputInfo {
    name: String::from("DP-1"),
    description: String::new(),
    make: String::new(),
    model: String::new(),
    mode: mode_of(1920, 1080),
    scale: 1,
    fractional_scale: 1.0,
    x: 0,
    y: 0,
    width: 1920,
    height: 1080,
    width_mm: 0,
    height_mm: 0,
    transform: UPRIGHT,
  };
  // A `done` that says what was said before changes nothing.
  let same = known.clone();
  assert_eq!(replace(&mut known, same), None);
  let scaled = OutputInfo {
    scale: 2,
    fractional_scale: 1.5,
    ..known.clone()
  };
  assert_eq!(replace(&mut known, scaled.clone()), Some(scaled.clone()));
  assert_eq!(known, scaled);
}

#[test]
fn every_transform_is_a_turn() {
  let all = [
    Transform::Normal,
    Transform::_90,
    Transform::_180,
    Transform::_270,
    Transform::Flipped,
    Transform::Flipped90,
    Transform::Flipped180,
    Transform::Flipped270,
  ];
  let turns = all.map(|transform| {
    let Turn { quarters, flipped } = turn(transform);
    (quarters, flipped)
  });
  assert_eq!(
    turns,
    [
      (0, false),
      (1, false),
      (2, false),
      (3, false),
      (0, true),
      (1, true),
      (2, true),
      (3, true),
    ]
  );
}
