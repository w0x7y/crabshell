//! Blur behind windows, through `ext-background-effect-v1`.

use crabshell_ui::Region;
use smithay_client_toolkit::compositor::{self, CompositorState};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::{
  self, Capability, ExtBackgroundEffectManagerV1,
};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::{
  self, ExtBackgroundEffectSurfaceV1,
};

use crate::platform::Platform;
use crate::surface::Surface;

/// The compositor's background effects.
pub(crate) struct Effects {
  /// Present when the compositor offers them.
  pub(crate) manager: Option<ExtBackgroundEffectManagerV1>,
  /// Whether it can blur, as its last `capabilities` event said.
  pub(crate) blur: bool,
  /// Whether a window asked for a blur it couldn't have, which is logged
  /// once.
  pub(crate) warned: bool,
}

/// A surface's background effect object, destroyed with the surface.
pub(crate) struct Blur(ExtBackgroundEffectSurfaceV1);

impl Drop for Blur {
  fn drop(&mut self) {
    self.0.destroy();
  }
}

impl Surface {
  /// Sends `region`, cut to the surface's size, as the area to blur
  /// behind, unless that was sent last. The next commit applies it.
  /// Nothing is sent before the first configure, which gives the size.
  pub(crate) fn send_blur(
    &mut self,
    effects: &mut Effects,
    compositor: &CompositorState,
    region: Option<&Region>,
    qh: &QueueHandle<Platform>,
  ) {
    let Some(size) = self.size else {
      return;
    };
    if self
      .blur_sent
      .as_ref()
      .is_some_and(|(sent, sent_size)| sent.as_ref() == region && *sent_size == size)
    {
      return;
    }
    self.blur_sent = Some((region.cloned(), size));
    let Some(region) = region else {
      if let Some(blur) = &self.blur {
        blur.0.set_blur_region(None);
      }
      return;
    };
    let Some(manager) = effects.manager.as_ref().filter(|_| effects.blur) else {
      if !std::mem::replace(&mut effects.warned, true) {
        tracing::warn!(
          "the compositor can't blur behind windows (it lacks ext-background-effect-v1), so \
           they show unblurred"
        );
      }
      return;
    };
    let surface = self.wl_surface().clone();
    let blur = self
      .blur
      .get_or_insert_with(|| Blur(manager.get_background_effect(&surface, qh, ())));
    match compositor::Region::new(compositor) {
      Ok(wl_region) => {
        for [x, y, width, height] in region.rects(size.0, size.1) {
          wl_region.add(x, y, width, height);
        }
        blur.0.set_blur_region(Some(wl_region.wl_region()));
      }
      Err(error) => tracing::warn!(%error, "couldn't create a region for a window's blur"),
    }
  }

  /// Whether the blur was cut to another size than the surface's, or not
  /// sent at all.
  pub(crate) fn blur_outdated(&self) -> bool {
    self.size.is_some() && self.blur_sent.as_ref().map(|(_, size)| *size) != self.size
  }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    _: &ExtBackgroundEffectManagerV1,
    event: ext_background_effect_manager_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let ext_background_effect_manager_v1::Event::Capabilities { flags } = event else {
      return;
    };
    let blur = matches!(flags, WEnum::Value(flags) if flags.contains(Capability::Blur));
    let effects = &mut platform.effects;
    if blur && !effects.blur {
      // Blur that couldn't be given before is sent again.
      for entry in &mut platform.windows.entries {
        if let Some(surface) = &mut entry.surface {
          surface.blur_sent = None;
        }
      }
    }
    effects.blur = blur;
  }
}

impl Dispatch<ExtBackgroundEffectSurfaceV1, ()> for Platform {
  fn event(
    _: &mut Platform,
    _: &ExtBackgroundEffectSurfaceV1,
    _: ext_background_effect_surface_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
  }
}
