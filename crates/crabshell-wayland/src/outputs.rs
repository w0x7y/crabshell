//! Outputs as the compositor announces them through `wl_output` version 4.

use smithay_client_toolkit::output;

/// An output the compositor announced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputInfo {
    /// The name, such as `DP-1` or `HEADLESS-1`. Windows name their output
    /// with it.
    pub name: String,
    /// The current mode, if the compositor gave one.
    pub mode: Option<Mode>,
    /// The output's whole scale, from `wl_output.scale`. A fractional scale
    /// reaches each window through `wp_fractional_scale_v1` instead.
    pub scale: i32,
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
/// find their output by name, so one without can't hold any.
pub(crate) fn describe(info: &output::OutputInfo) -> Option<OutputInfo> {
    Some(OutputInfo {
        name: info.name.clone()?,
        mode: current_mode(&info.modes),
        scale: info.scale_factor,
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
