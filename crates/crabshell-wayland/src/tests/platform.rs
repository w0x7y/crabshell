//! What the platform refuses before it starts.

use crate::platform::{Error, require_version};

#[test]
fn a_compositor_without_damage_buffer_is_refused() {
  // wl_surface.damage_buffer came in wl_compositor version 4.
  assert_eq!(
    require_version("wl_compositor", 3, 4),
    Err(Error::TooOld {
      global: "wl_compositor",
      version: 3,
      needed: 4,
    })
  );
  assert_eq!(require_version("wl_compositor", 4, 4), Ok(()));
  assert_eq!(require_version("wl_compositor", 6, 4), Ok(()));
  assert_eq!(
    require_version("wl_compositor", 1, 4)
      .map_err(|error| error.to_string())
      .unwrap_err(),
    "the compositor offers wl_compositor version 1, but Crabshell needs 4"
  );
}

#[test]
fn animations_tick_once_a_frame_of_the_fastest_output() {
  use std::time::Duration;

  use crate::platform::frame_interval;

  assert_eq!(
    frame_interval([60_000, 144_000].into_iter()),
    Duration::from_micros(6_944)
  );
  assert_eq!(
    frame_interval([].into_iter()),
    Duration::from_micros(16_666)
  );
  // Outputs with no refresh rate count as 60 Hz.
  assert_eq!(
    frame_interval([0].into_iter()),
    Duration::from_micros(16_666)
  );
}
