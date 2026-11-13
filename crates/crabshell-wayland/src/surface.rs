//! One window's surface, a panel's layer surface or a popup's
//! `xdg_popup`: its settings, buffers, scale and frame pacing, and drawing
//! a frame into it.

use crabshell_render::{BufferAge, Canvas, DamageHistory, PixelFormat, PixelRect, Renderer};
use crabshell_ui::{Anchors, Edge, Layer, Size, SurfaceConfig, TextSystem, Window};
use smithay_client_toolkit::compositor::FrameCallbackData;
use smithay_client_toolkit::reexports::client::protocol::{
  wl_callback::WlCallback, wl_shm, wl_surface::WlSurface,
};
use smithay_client_toolkit::reexports::client::{Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{self, Anchor, LayerSurface, SurfaceKind};
use smithay_client_toolkit::shell::xdg::XdgPositioner;
use smithay_client_toolkit::shell::xdg::popup::Popup;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use crate::buffers::{Buffers, ShmBuffer};
use crate::pacing::Pacing;
use crate::scale::Scale;

/// The largest logical size accepted from a configure.
const MAX_LOGICAL_SIDE: u32 = 16_384;

/// The layer-shell namespace, which compositors use to tell apps apart.
pub(crate) const NAMESPACE: &str = "crabshell";

/// An sctk buffer and its size, which sctk doesn't keep.
pub(crate) struct ShmSlot {
  shm: Buffer,
  width: u32,
  height: u32,
}

impl ShmBuffer for ShmSlot {
  fn size(&self) -> (u32, u32) {
    (self.width, self.height)
  }

  fn busy(&self) -> bool {
    self.shm.slot().has_active_buffers()
  }
}

/// A window's frames: what its buffers hold (the damage history), when it
/// draws next (pacing) and at what scale. Apart from the protocol objects,
/// so it can be tested without a compositor.
pub(crate) struct Frames {
  pub(crate) history: DamageHistory,
  pub(crate) pacing: Pacing,
  pub(crate) scale: Scale,
}

impl Frames {
  pub(crate) fn new(scale: Scale) -> Frames {
    Frames {
      history: DamageHistory::new(),
      pacing: Pacing::default(),
      scale,
    }
  }

  /// A new scale from the compositor. Every pixel changes, so the next
  /// frame repaints the whole buffer, which the damage history sees for
  /// itself.
  pub(crate) fn set_scale(&mut self, scale: Scale) {
    if scale != self.scale {
      self.scale = scale;
      self.pacing.redraw();
    }
  }

  /// Draws `window`'s damage into a `width` by `height` buffer holding
  /// `pixels`, whose last frame `age` records. `None` if the renderer
  /// refused the buffer: the damage taken is lost then, so the history
  /// starts over, which repaints everything, and another frame is asked
  /// for.
  pub(crate) fn draw(
    &mut self,
    window: &Window,
    renderer: &mut Renderer,
    text: &mut TextSystem,
    (pixels, width, height): (&mut [u8], u32, u32),
    age: &mut BufferAge,
  ) -> Option<Vec<PixelRect>> {
    let damage = window.take_damage();
    let drawn = renderer.draw(
      &window.view(),
      &damage,
      text,
      &mut self.history,
      Canvas {
        pixels,
        width,
        height,
        scale: self.scale.factor(),
        format: PixelFormat::Argb8888,
        age,
      },
    );
    match drawn {
      Ok(report) => Some(report),
      Err(error) => {
        tracing::warn!(%error, "couldn't draw a window");
        self.lost();
        None
      }
    }
  }

  /// A frame was drawn but won't be shown, as when its buffer couldn't be
  /// attached: the history counted it, so it starts over, and another
  /// frame is asked for.
  pub(crate) fn lost(&mut self) {
    self.repaint_whole();
  }

  /// Every pixel may change, as when the icon theme does: the history
  /// starts over, so the next frame, asked for now, repaints everything.
  pub(crate) fn repaint_whole(&mut self) {
    self.history = DamageHistory::new();
    self.pacing.redraw();
  }
}

/// What a surface is to the compositor.
pub(crate) enum Role {
  /// A panel's layer surface.
  Layer(LayerSurface),
  /// A popup.
  Popup(PopupRole),
}

/// A popup's `xdg_popup` and how it was placed.
pub(crate) struct PopupRole {
  pub(crate) popup: Popup,
  /// Kept to send a new size with: its anchor rectangle and gravity stay.
  pub(crate) positioner: XdgPositioner,
  /// The size the compositor last placed it at.
  pub(crate) placed: (u32, u32),
  /// Whether it took a pointer grab. A popup opened from it may take one
  /// only if it did.
  pub(crate) grabbed: bool,
  /// Where the compositor placed it, from the last configure, relative to
  /// the window that opened it.
  pub(crate) position: Option<(i32, i32)>,
}

/// A window's surface and everything needed to draw into it.
pub(crate) struct Surface {
  pub(crate) role: Role,
  /// Present when the compositor scales fractionally.
  pub(crate) fractional: Option<(WpFractionalScaleV1, WpViewport)>,
  pool: SlotPool,
  buffers: Buffers<ShmSlot>,
  pub(crate) frames: Frames,
  /// The logical size from the last configure.
  pub(crate) size: Option<(u32, u32)>,
  /// The logical size and scale the surface was last told, so they are
  /// only sent when they change.
  told: Option<((u32, u32), Scale)>,
  /// Frames committed.
  pub(crate) commits: u64,
  /// What the last frame reported with `damage_buffer`.
  pub(crate) last_damage: Vec<PixelRect>,
  /// The size of the last buffer committed.
  pub(crate) buffer_size: Option<(u32, u32)>,
}

impl Surface {
  pub(crate) fn new(
    role: Role,
    fractional: Option<(WpFractionalScaleV1, WpViewport)>,
    pool: SlotPool,
    scale: Scale,
  ) -> Surface {
    Surface {
      role,
      fractional,
      pool,
      buffers: Buffers::new(),
      frames: Frames::new(scale),
      size: None,
      told: None,
      commits: 0,
      last_damage: Vec::new(),
      buffer_size: None,
    }
  }

  pub(crate) fn wl_surface(&self) -> &WlSurface {
    match &self.role {
      Role::Layer(layer) => layer.wl_surface(),
      Role::Popup(popup) => popup.popup.wl_surface(),
    }
  }

  /// The popup role, if this is a popup.
  pub(crate) fn popup(&self) -> Option<&PopupRole> {
    match &self.role {
      Role::Popup(popup) => Some(popup),
      Role::Layer(_) => None,
    }
  }

  /// A popup is the size its content wants, which its layout just worked
  /// out.
  pub(crate) fn fit(&mut self, size: Size) {
    if let Role::Popup(_) = self.role {
      self.size = Some((size.width as u32, size.height as u32));
      self.place_again();
    }
  }

  /// Sends a popup's new size so the compositor places it again, once it
  /// is mapped: `reposition` is only for a mapped popup. A no-op before
  /// `xdg_wm_base` version 3, where the new size still shows.
  fn place_again(&mut self) {
    let (Role::Popup(popup), Some(size)) = (&mut self.role, self.size) else {
      return;
    };
    if self.commits == 0 || popup.placed == size {
      return;
    }
    popup.placed = size;
    let [width, height] = [size.0, size.1].map(|side| i32::try_from(side).unwrap_or(i32::MAX));
    popup.positioner.set_size(width, height);
    popup.popup.reposition(&popup.positioner, 0);
  }

  /// Takes a configure's size. Returns the size, or `None` if it can't be
  /// drawn at.
  pub(crate) fn configure(
    &mut self,
    new_size: (u32, u32),
    config: &SurfaceConfig,
  ) -> Option<(u32, u32)> {
    let size = configured_size(new_size, (config.width, config.height))?;
    self.size = Some(size);
    self.frames.pacing.configure();
    Some(size)
  }

  /// Sends a panel's new settings from the window's description, which
  /// must have passed [`size_problem`]. The compositor answers with a
  /// configure.
  pub(crate) fn reconfigure(&self, config: &SurfaceConfig) {
    let Role::Layer(surface) = &self.role else {
      return;
    };
    apply(surface, config);
    if let SurfaceKind::Wlr(wlr) = surface.kind() {
      // Changing the layer later needs version 2.
      if wlr.version() >= 2 {
        surface.set_layer(layer(config.layer));
      }
    }
    surface.commit();
  }

  /// Draws `window`'s damage into a free buffer and commits it, with a
  /// frame callback. Returns whether it committed: not if every buffer is
  /// busy, which a release will end, or if something failed, which is
  /// logged and asks for another frame.
  ///
  /// `D` is the event loop's state, which hears the frame callback.
  pub(crate) fn draw<D>(
    &mut self,
    window: &Window,
    renderer: &mut Renderer,
    text: &mut TextSystem,
    qh: &QueueHandle<D>,
  ) -> bool
  where
    D: Dispatch<WlCallback, FrameCallbackData> + 'static,
  {
    let Some((width, height)) = self.size else {
      return false;
    };
    let Some((buffer_width, buffer_height)) = self.frames.scale.buffer_size(width, height) else {
      tracing::warn!(
        width,
        height,
        scale = self.frames.scale.factor(),
        "a window too large to draw"
      );
      return false;
    };
    // Split the borrows: the pool hands out the canvas while a slot is
    // borrowed from the buffers.
    let Surface {
      pool,
      buffers,
      frames,
      ..
    } = self;
    let slot = buffers.next((buffer_width, buffer_height), || {
      let stride = buffer_width.checked_mul(4)?;
      match pool.create_buffer(
        i32::try_from(buffer_width).ok()?,
        i32::try_from(buffer_height).ok()?,
        i32::try_from(stride).ok()?,
        wl_shm::Format::Argb8888,
      ) {
        Ok((shm, _)) => Some(ShmSlot {
          shm,
          width: buffer_width,
          height: buffer_height,
        }),
        Err(error) => {
          tracing::warn!(%error, buffer_width, buffer_height, "couldn't allocate a buffer");
          None
        }
      }
    });
    let Some(slot) = slot else {
      return false;
    };
    let Some(pixels) = slot.buffer.shm.canvas(pool) else {
      return false;
    };
    let canvas = (pixels, buffer_width, buffer_height);
    let Some(report) = frames.draw(window, renderer, text, canvas, &mut slot.age) else {
      return false;
    };
    // Not `self.wl_surface()`: `frames` is borrowed from `self`.
    let surface = match &self.role {
      Role::Layer(layer) => layer.wl_surface(),
      Role::Popup(popup) => popup.popup.wl_surface(),
    };
    if let Err(error) = slot.buffer.shm.attach_to(surface) {
      tracing::warn!(%error, "couldn't attach a buffer");
      frames.lost();
      return false;
    }
    for rect in &report {
      let [x, y, width, height] = [rect.x, rect.y, rect.width, rect.height]
        .map(|value| i32::try_from(value).unwrap_or(i32::MAX));
      surface.damage_buffer(x, y, width, height);
    }
    let told = ((width, height), frames.scale);
    if self.told != Some(told) {
      match (&self.fractional, frames.scale) {
        (Some((_, viewport)), _) => {
          let [width, height] =
            [width, height].map(|value| i32::try_from(value).unwrap_or(i32::MAX));
          viewport.set_destination(width, height);
        }
        (None, Scale::Integer(factor)) => {
          surface.set_buffer_scale(i32::try_from(factor).unwrap_or(1));
        }
        // Fractional scales come only with the viewport.
        (None, Scale::Fractional(_)) => {}
      }
      self.told = Some(told);
    }
    surface.frame(qh, FrameCallbackData(surface.clone()));
    surface.commit();
    frames.pacing.committed();
    self.commits += 1;
    self.last_damage = report;
    self.buffer_size = Some((buffer_width, buffer_height));
    // A popup that grew before its first frame is mapped only now.
    self.place_again();
    true
  }
}

impl Drop for Surface {
  fn drop(&mut self) {
    // The role and its wl_surface go when `role` drops; these two are
    // ours to destroy.
    if let Some((fractional, viewport)) = self.fractional.take() {
      fractional.destroy();
      viewport.destroy();
    }
  }
}

/// Why layer-shell would refuse `config`'s size, if it would. A side of 0
/// leaves it to the compositor, which is allowed only when the window is
/// anchored to both edges across that side; anything else is a protocol
/// error that ends the connection, and every window with it.
pub(crate) fn size_problem(config: &SurfaceConfig) -> Option<&'static str> {
  let anchored = |one, other| config.anchors.contains(one) && config.anchors.contains(other);
  if config.width == 0 && !anchored(Edge::Left, Edge::Right) {
    Some("a width of 0 needs anchors on both the left and right edges")
  } else if config.height == 0 && !anchored(Edge::Top, Edge::Bottom) {
    Some("a height of 0 needs anchors on both the top and bottom edges")
  } else {
    None
  }
}

/// The size to draw at after a configure: the compositor's, or the
/// requested one where the compositor sent 0 to leave the choice to the
/// client. `None` if a side is still 0 or larger than 16384.
pub(crate) fn configured_size(new_size: (u32, u32), requested: (u32, u32)) -> Option<(u32, u32)> {
  let side = |given: u32, requested: u32| {
    let side = if given == 0 { requested } else { given };
    (1..=MAX_LOGICAL_SIDE).contains(&side).then_some(side)
  };
  Some((
    side(new_size.0, requested.0)?,
    side(new_size.1, requested.1)?,
  ))
}

/// Sends the settings a commit applies, all but the layer.
pub(crate) fn apply(surface: &LayerSurface, config: &SurfaceConfig) {
  surface.set_anchor(anchor(config.anchors));
  surface.set_size(config.width, config.height);
  surface.set_exclusive_zone(config.exclusive_zone);
}

/// The layer-shell anchors for `anchors`.
pub(crate) fn anchor(anchors: Anchors) -> Anchor {
  [
    (Edge::Top, Anchor::TOP),
    (Edge::Bottom, Anchor::BOTTOM),
    (Edge::Left, Anchor::LEFT),
    (Edge::Right, Anchor::RIGHT),
  ]
  .into_iter()
  .filter(|(edge, _)| anchors.contains(*edge))
  .fold(Anchor::empty(), |all, (_, anchor)| all | anchor)
}

/// The layer-shell layer for `layer`.
pub(crate) fn layer(layer: Layer) -> wlr_layer::Layer {
  match layer {
    Layer::Background => wlr_layer::Layer::Background,
    Layer::Bottom => wlr_layer::Layer::Bottom,
    Layer::Top => wlr_layer::Layer::Top,
    Layer::Overlay => wlr_layer::Layer::Overlay,
  }
}
