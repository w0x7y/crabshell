//! The scale a window is drawn at, and the buffer size that gives.

/// The largest scale accepted from the compositor, in 120ths: 8.
const MAX_SCALE_120: u32 = 8 * 120;

/// The widest or tallest buffer drawn, in pixels.
const MAX_BUFFER_SIDE: u64 = 16_384;

/// How a window's buffer pixels map to its logical size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scale {
    /// The compositor's preferred scale from `wp_fractional_scale_v1`, in
    /// 120ths (180 is 1.5). `wp_viewporter` gives the surface its logical
    /// size.
    Fractional(u32),
    /// A whole scale, set with `wl_surface.set_buffer_scale`.
    Integer(u32),
}

impl Scale {
    /// A preferred scale from `wp_fractional_scale_v1`; `None` if it is 0
    /// or above 8.
    pub(crate) fn fractional(scale_120: u32) -> Option<Scale> {
        (1..=MAX_SCALE_120)
            .contains(&scale_120)
            .then_some(Scale::Fractional(scale_120))
    }

    /// A whole scale from `wl_output` or `wl_surface`; `None` if it is
    /// below 1 or above 8.
    pub(crate) fn integer(factor: i32) -> Option<Scale> {
        u32::try_from(factor)
            .ok()
            .filter(|factor| (1..=MAX_SCALE_120 / 120).contains(factor))
            .map(Scale::Integer)
    }

    /// The same scale in 120ths, for a window that scales fractionally but
    /// hasn't heard its preferred scale yet.
    pub(crate) fn as_fractional(self) -> Scale {
        match self {
            Scale::Fractional(_) => self,
            Scale::Integer(factor) => Scale::Fractional(factor * 120),
        }
    }

    /// Physical pixels per logical pixel.
    pub(crate) fn factor(self) -> f32 {
        match self {
            Scale::Fractional(scale_120) => scale_120 as f32 / 120.0,
            Scale::Integer(factor) => factor as f32,
        }
    }

    /// The buffer for a window `width` by `height` logical pixels: each
    /// side times the scale, rounded halfway away from zero, as
    /// `wp_fractional_scale_v1` says and compositors size the surface on
    /// screen. A buffer a pixel larger would be scaled down and blurred.
    /// `None` if a side comes out 0 or over 16384 pixels.
    pub(crate) fn buffer_size(self, width: u32, height: u32) -> Option<(u32, u32)> {
        // In whole numbers, so 7/6 times 1200 can't come out as 1400.0001.
        let (numerator, denominator) = match self {
            Scale::Fractional(scale_120) => (u64::from(scale_120), 120),
            Scale::Integer(factor) => (u64::from(factor), 1),
        };
        let side = |logical: u32| {
            let pixels = (u64::from(logical) * numerator + denominator / 2) / denominator;
            (1..=MAX_BUFFER_SIDE)
                .contains(&pixels)
                .then_some(pixels as u32)
        };
        Some((side(width)?, side(height)?))
    }
}
