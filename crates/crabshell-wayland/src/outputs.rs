//! Outputs as the compositor announces them through `wl_output` version 4,
//! with their logical place and size from `xdg-output`, which sctk binds.

use crabshell_render::Turn;
use smithay_client_toolkit::output;
use smithay_client_toolkit::reexports::client::protocol::wl_output::Transform;

/// An output the compositor announced, as Quickshell's `ShellScreen`
/// describes a screen. The compositor tells a change at any time, as when
/// the user sets a new scale.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputInfo {
  /// The name, such as `DP-1` or `HEADLESS-1`. Windows name their output
  /// with it.
  pub name: String,
  /// The compositor's words for it, such as `Dell Inc. DELL U2720Q`, or
  /// `""` if it gives none.
  pub description: String,
  /// The maker, as the compositor gives it, often from the monitor's EDID.
  pub make: String,
  /// The model, as the compositor gives it.
  pub model: String,
  /// The current mode, if the compositor gave one.
  pub mode: Option<Mode>,
  /// The output's whole scale, from `wl_output.scale`: 2 for a scale of
  /// 1.5. See [`fractional_scale`](Self::fractional_scale).
  pub scale: i32,
  /// The scale windows on it draw at, such as 1.5: the mode's width over
  /// the logical width, to the nearest 120th, as `wp_fractional_scale_v1`
  /// tells windows their scale. Where windows can't draw at fractional
  /// scales, because the compositor lacks that protocol or
  /// `wp_viewporter`, it is the whole [`scale`](Self::scale).
  pub fractional_scale: f64,
  /// Where its left edge is in the compositor's layout, in logical pixels.
  pub x: i32,
  /// Where its top edge is, in logical pixels.
  pub y: i32,
  /// Its width in logical pixels, as turned: what a window across it gets.
  pub width: u32,
  /// Its height in logical pixels, as turned.
  pub height: u32,
  /// Its physical width in millimeters; 0 if it has none, as virtual
  /// outputs and projectors may not.
  pub width_mm: u32,
  /// Its physical height in millimeters; 0 if it has none.
  pub height_mm: u32,
  /// How the compositor turns what it shows, from `wl_output.transform`.
  pub transform: Turn,
}

/// An output's mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
  /// The width in pixels.
  pub width: u32,
  /// The height in pixels.
  pub height: u32,
  /// The refresh rate in millihertz; 0 if the output has none, as virtual
  /// outputs may not.
  pub refresh_mhz: u32,
}

/// What sctk knows about an output, or `None` if it has no name: windows
/// find their output by name, so one without can't hold any. `fractional`
/// says whether windows draw at fractional scales.
pub(crate) fn describe(info: &output::OutputInfo, fractional: bool) -> Option<OutputInfo> {
  let mode = current_mode(&info.modes);
  let transform = turn(info.transform);
  let (width, height) = logical_size(info.logical_size, mode, transform, info.scale_factor);
  let whole = f64::from(info.scale_factor);
  let (x, y) = info.logical_position.unwrap_or(info.location);
  let millimeters = |side: i32| u32::try_from(side).unwrap_or(0);
  Some(OutputInfo {
    name: info.name.clone()?,
    description: info.description.clone().unwrap_or_default(),
    make: info.make.clone(),
    model: info.model.clone(),
    mode,
    scale: info.scale_factor,
    fractional_scale: fractional
      .then(|| fractional_scale(mode, transform, width))
      .flatten()
      .unwrap_or(whole),
    x,
    y,
    width,
    height,
    width_mm: millimeters(info.physical_size.0),
    height_mm: millimeters(info.physical_size.1),
    transform,
  })
}

/// The mode marked current, if it has a size.
pub(crate) fn current_mode(modes: &[output::Mode]) -> Option<Mode> {
  let mode = modes.iter().find(|mode| mode.current)?;
  let (width, height) = mode.dimensions;
  Some(Mode {
    width: u32::try_from(width).ok().filter(|&width| width > 0)?,
    height: u32::try_from(height).ok().filter(|&height| height > 0)?,
    refresh_mhz: u32::try_from(mode.refresh_rate).unwrap_or(0),
  })
}

/// The size in logical pixels: `xdg-output`'s, or without a usable one the
/// mode, turned, over the whole `scale`; 0 by 0 without a mode either.
pub(crate) fn logical_size(
  logical: Option<(i32, i32)>,
  mode: Option<Mode>,
  transform: Turn,
  scale: i32,
) -> (u32, u32) {
  let side = |side: i32| u32::try_from(side).ok().filter(|&side| side > 0);
  if let Some((width, height)) = logical
    && let (Some(width), Some(height)) = (side(width), side(height))
  {
    return (width, height);
  }
  let scale = u32::try_from(scale).unwrap_or(1).max(1);
  shown(mode, transform).map_or((0, 0), |(width, height)| (width / scale, height / scale))
}

/// The scale a window `width` logical pixels across the output draws at:
/// the mode's width, turned, over `width`, to the nearest 120th, as
/// `wp_fractional_scale_v1` gives scales. `None` without a mode or a
/// width, or outside the 1/120 to 8 windows take.
pub(crate) fn fractional_scale(mode: Option<Mode>, transform: Turn, width: u32) -> Option<f64> {
  let (pixels, _) = shown(mode, transform)?;
  let scale_120 = (u64::from(pixels) * 120 + u64::from(width) / 2).checked_div(u64::from(width))?;
  (1..=960)
    .contains(&scale_120)
    .then(|| scale_120 as f64 / 120.0)
}

/// The mode's size as it shows, turned.
fn shown(mode: Option<Mode>, transform: Turn) -> Option<(u32, u32)> {
  let mode = mode?;
  Some(if transform.quarters % 2 == 1 {
    (mode.height, mode.width)
  } else {
    (mode.width, mode.height)
  })
}

/// A `wl_output.transform` as render takes it.
pub(crate) fn turn(transform: Transform) -> Turn {
  let (quarters, flipped) = match transform {
    Transform::_90 => (1, false),
    Transform::_180 => (2, false),
    Transform::_270 => (3, false),
    Transform::Flipped => (0, true),
    Transform::Flipped90 => (1, true),
    Transform::Flipped180 => (2, true),
    Transform::Flipped270 => (3, true),
    _ => (0, false),
  };
  Turn { quarters, flipped }
}
