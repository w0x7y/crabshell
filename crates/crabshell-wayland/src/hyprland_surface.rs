//! The opacity Hyprland shows a window at, and the part of it that holds
//! anything to see, through `hyprland-surface-v1`.

use crabshell_ui::{Region, Window};
use smithay_client_toolkit::compositor::{self, CompositorState};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::registry::RegistryState;

use crate::hyprland::surface::hyprland_surface_manager_v1::{self, HyprlandSurfaceManagerV1};
use crate::hyprland::surface::hyprland_surface_v1::{self, HyprlandSurfaceV1};
use crate::platform::Platform;
use crate::scale::Scale;
use crate::surface::Surface;

/// Hyprland's surface manager, bound the first time a window asks.
#[derive(Default)]
pub(crate) struct HyprlandSurfaces {
  /// Whether a window asked, which binds the manager, or logs once that
  /// the compositor lacks it.
  asked: bool,
  manager: Option<HyprlandSurfaceManagerV1>,
  /// Whether a visible mask was asked of version 1, which has none; that
  /// is logged once.
  warned: bool,
}

impl HyprlandSurfaces {
  /// The manager's version, binding it at the first call.
  fn version(&mut self, registry: &RegistryState, qh: &QueueHandle<Platform>) -> Option<u32> {
    if !std::mem::replace(&mut self.asked, true) {
      self.manager = registry.bind_one(qh, 1..=2, ()).ok();
      if self.manager.is_none() {
        tracing::warn!(
          "the compositor lacks hyprland-surface-v1, so windows' compositor_opacity and \
           visible_mask do nothing"
        );
      }
    }
    self.manager.as_ref().map(Proxy::version)
  }
}

/// A surface's Hyprland object, destroyed with its surface, as the
/// protocol asks.
pub(crate) struct HyprlandSurface(HyprlandSurfaceV1);

impl Drop for HyprlandSurface {
  fn drop(&mut self) {
    self.0.destroy();
  }
}

/// What a surface asks of Hyprland: its opacity, and the part that holds
/// anything to see in its buffer's pixels, `None` for all of it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shown {
  pub(crate) opacity: f32,
  pub(crate) visible: Option<Vec<[i32; 4]>>,
}

impl Default for Shown {
  /// The protocol's defaults, which need no object.
  fn default() -> Shown {
    Shown {
      opacity: 1.0,
      visible: None,
    }
  }
}

/// What a surface last told Hyprland, and the size and scale its visible
/// mask was cut at.
#[derive(Debug, Default)]
pub(crate) struct Told {
  pub(crate) shown: Shown,
  cut: Option<((u32, u32), Scale)>,
}

impl Told {
  /// Whether the visible mask was cut at another size or scale than
  /// these, or nothing was told yet.
  pub(crate) fn outdated(&self, size: (u32, u32), scale: Scale) -> bool {
    self.cut != Some((size, scale))
  }

  /// Takes a window's `opacity` and `mask`, cut at `size` and `scale`, as
  /// told. Returns what was told before when that changed, for
  /// [`requests`], and `None` when there is nothing to send.
  pub(crate) fn take(
    &mut self,
    opacity: f32,
    mask: Option<Region>,
    size: (u32, u32),
    scale: Scale,
  ) -> Option<Shown> {
    self.cut = Some((size, scale));
    let wanted = Shown {
      opacity,
      visible: mask.and_then(|region| buffer_rects(&region, size, scale)),
    };
    (wanted != self.shown).then(|| std::mem::replace(&mut self.shown, wanted))
  }

  /// The visible region couldn't be sent, so Hyprland still has `before`;
  /// it is cut and sent again next turn, as a mask is.
  pub(crate) fn unsent_visible(&mut self, before: Option<Vec<[i32; 4]>>) {
    self.shown.visible = before;
    self.cut = None;
  }
}

/// A request to a surface's Hyprland object.
#[derive(Debug, PartialEq)]
pub(crate) enum Request {
  Opacity(f32),
  Visible(Option<Vec<[i32; 4]>>),
}

/// The requests that take a surface from `sent` to `wanted` on `version`
/// of the protocol: none when they agree, so a window that asks only for
/// the defaults never gets an object. Visible regions came in version 2,
/// and before it are left out.
pub(crate) fn requests(sent: &Shown, wanted: &Shown, version: u32) -> Vec<Request> {
  let mut requests = Vec::new();
  if sent.opacity != wanted.opacity {
    requests.push(Request::Opacity(wanted.opacity));
  }
  if version >= 2 && sent.visible != wanted.visible {
    requests.push(Request::Visible(wanted.visible.clone()));
  }
  requests
}

/// `region` cut to a surface `size` logical pixels big, in the pixels of
/// its buffer at `scale`, which is what the protocol takes. Each rectangle
/// is grown by a logical pixel each way, then out to whole buffer pixels
/// and cut to the buffer: curves are cut at whole pixels, and the soft
/// edges drawn along them reach past, so all that shows is covered.
/// `None` when nothing is left, which Hyprland reads as all of it.
pub(crate) fn buffer_rects(
  region: &Region,
  size: (u32, u32),
  scale: Scale,
) -> Option<Vec<[i32; 4]>> {
  let (width, height) = scale.buffer_size(size.0, size.1)?;
  let (numerator, denominator) = match scale {
    Scale::Fractional(scale_120) => (i64::from(scale_120), 120),
    Scale::Integer(factor) => (i64::from(factor), 1),
  };
  // The region's rectangles are cut to the surface, so never negative.
  let down = |logical: i32| i64::from(logical) * numerator / denominator;
  let up = |logical: i64, side: u32| {
    ((logical * numerator + denominator - 1) / denominator).min(i64::from(side))
  };
  let rects: Vec<[i32; 4]> = region
    .rects(size.0, size.1)
    .into_iter()
    .map(|[x, y, rect_width, rect_height]| {
      let (left, top) = (down((x - 1).max(0)), down((y - 1).max(0)));
      let right = up(i64::from(x) + i64::from(rect_width) + 1, width);
      let bottom = up(i64::from(y) + i64::from(rect_height) + 1, height);
      // Within a buffer of at most 16384 pixels a side.
      [left, top, right - left, bottom - top].map(|value| value as i32)
    })
    .filter(|[_, _, width, height]| *width > 0 && *height > 0)
    .collect();
  (!rects.is_empty()).then_some(rects)
}

impl Surface {
  /// Sends Hyprland `window`'s compositor opacity and visible mask, cut
  /// to the surface's size at its scale, where they differ from what it
  /// was last told, and says whether it sent anything. The next commit
  /// applies them. The surface gets its object, and the shell the
  /// manager, the first time one differs from the default. Nothing is
  /// sent before the first configure, which gives the size.
  pub(crate) fn send_hyprland(
    &mut self,
    hyprland: &mut HyprlandSurfaces,
    registry: &RegistryState,
    compositor: &CompositorState,
    window: &Window,
    qh: &QueueHandle<Platform>,
  ) -> bool {
    let Some(size) = self.size else {
      return false;
    };
    let (opacity, mask) = (window.compositor_opacity(), window.visible_mask());
    let told = &mut self.hyprland_told;
    let Some(before) = told.take(opacity, mask, size, self.frames.scale) else {
      return false;
    };
    let Some(version) = hyprland.version(registry, qh) else {
      return false;
    };
    let wanted = &told.shown;
    if version < 2 && wanted.visible.is_some() && !std::mem::replace(&mut hyprland.warned, true) {
      tracing::warn!(
        "the compositor's hyprland-surface-v1 is version 1, which has no visible regions, so \
         windows' visible_mask does nothing"
      );
    }
    let requests = requests(&before, wanted, version);
    let Some(manager) = hyprland.manager.as_ref().filter(|_| !requests.is_empty()) else {
      return false;
    };
    let surface = self.wl_surface().clone();
    let object = self
      .hyprland
      .get_or_insert_with(|| HyprlandSurface(manager.get_hyprland_surface(&surface, qh, ())));
    let mut sent = false;
    for request in requests {
      match request {
        Request::Opacity(opacity) => object.0.set_opacity(f64::from(opacity)),
        Request::Visible(None) => object.0.set_visible_region(None),
        Request::Visible(Some(rects)) => match compositor::Region::new(compositor) {
          Ok(region) => {
            for [x, y, width, height] in rects {
              region.add(x, y, width, height);
            }
            object.0.set_visible_region(Some(region.wl_region()));
          }
          Err(error) => {
            tracing::warn!(%error, "couldn't create a region for a visible mask");
            self.hyprland_told.unsent_visible(before.visible.clone());
            continue;
          }
        },
      }
      sent = true;
    }
    sent
  }

  /// Whether the visible mask was cut at another size or scale than the
  /// surface's, or nothing was sent at all.
  pub(crate) fn hyprland_outdated(&self) -> bool {
    let scale = self.frames.scale;
    let told = &self.hyprland_told;
    self.size.is_some_and(|size| told.outdated(size, scale))
  }
}

impl Dispatch<HyprlandSurfaceManagerV1, ()> for Platform {
  fn event(
    _: &mut Platform,
    _: &HyprlandSurfaceManagerV1,
    _: hyprland_surface_manager_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
  }
}

impl Dispatch<HyprlandSurfaceV1, ()> for Platform {
  fn event(
    _: &mut Platform,
    _: &HyprlandSurfaceV1,
    _: hyprland_surface_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
  }
}
