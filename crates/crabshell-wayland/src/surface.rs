//! One window's surface, a panel's layer surface or a popup's
//! `xdg_popup`: its settings, buffers, scale and frame pacing, and drawing
//! a frame into it.

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crabshell_render::{BufferAge, Canvas, DamageHistory, PixelFormat, PixelRect, Renderer};
use crabshell_ui::{
  Anchors, Edge, KeyboardFocus, Layer, Region, Size, SurfaceConfig, TextSystem, Window,
};
use rustix::fs::FallocateFlags;
use smithay_client_toolkit::compositor::{self, CompositorState, FrameCallbackData};
use smithay_client_toolkit::reexports::client::backend::protocol::Message;
use smithay_client_toolkit::reexports::client::backend::{Backend, ObjectData, ObjectId};
use smithay_client_toolkit::reexports::client::protocol::wl_buffer::WlBuffer;
use smithay_client_toolkit::reexports::client::protocol::{
  wl_callback::WlCallback, wl_shm, wl_surface::WlSurface,
};
use smithay_client_toolkit::reexports::client::{Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
  self, Anchor, KeyboardInteractivity, LayerSurface, SurfaceKind,
};
use smithay_client_toolkit::shell::xdg::XdgPositioner;
use smithay_client_toolkit::shell::xdg::popup::Popup;
use smithay_client_toolkit::shell::xdg::window::Window as ToplevelWindow;
use smithay_client_toolkit::shm::Shm;
use smithay_client_toolkit::shm::raw::RawPool;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use crate::buffers::{Buffers, ShmBuffer};
use crate::pacing::Pacing;
use crate::scale::Scale;

/// The largest logical size accepted from a configure.
const MAX_LOGICAL_SIDE: u32 = 16_384;

/// A buffer in a shared-memory pool of its own, so its memory can be
/// given back while the buffer is kept, and its size. sctk's slot pools
/// keep their memory until dropped, and dropping a buffer makes the
/// compositor answer, which wakes the loop.
pub(crate) struct ShmSlot {
  buffer: WlBuffer,
  read: Arc<Read>,
  pool: RawPool,
  width: u32,
  height: u32,
}

impl ShmSlot {
  /// A `width` by `height` ARGB8888 buffer in a new pool.
  fn new(shm: &Shm, width: u32, height: u32) -> Result<ShmSlot, String> {
    let stride = width.checked_mul(4).ok_or("too wide")?;
    let bytes = usize::try_from(u64::from(stride) * u64::from(height)).map_err(|_| "too large")?;
    let [width_i32, height_i32, stride] =
      [width, height, stride].map(|value| i32::try_from(value).map_err(|_| "too large"));
    let mut pool = RawPool::new(bytes, shm).map_err(|error| error.to_string())?;
    let read = Arc::new(Read::default());
    let buffer = pool.create_buffer_raw(
      0,
      width_i32?,
      height_i32?,
      stride?,
      wl_shm::Format::Argb8888,
      Arc::clone(&read) as Arc<dyn ObjectData>,
    );
    Ok(ShmSlot {
      buffer,
      read,
      pool,
      width,
      height,
    })
  }
}

impl ShmBuffer for ShmSlot {
  fn size(&self) -> (u32, u32) {
    (self.width, self.height)
  }

  fn busy(&self) -> bool {
    self.read.0.load(Ordering::Relaxed)
  }

  fn empty(&mut self) {
    let flags = FallocateFlags::PUNCH_HOLE | FallocateFlags::KEEP_SIZE;
    let length = self.pool.len() as u64;
    if let Err(error) = rustix::fs::fallocate(&self.pool, flags, 0, length) {
      tracing::warn!(%error, "couldn't give a spare buffer's memory back");
    }
  }
}

impl Drop for ShmSlot {
  fn drop(&mut self) {
    // The pool goes with its own drop.
    self.buffer.destroy();
  }
}

/// Whether the compositor reads a buffer: set as it is attached, cleared
/// by its `release`. Object data must be `Send` and `Sync`, hence atomic.
#[derive(Default)]
struct Read(AtomicBool);

impl ObjectData for Read {
  fn event(
    self: Arc<Self>,
    _: &Backend,
    _: Message<ObjectId, OwnedFd>,
  ) -> Option<Arc<dyn ObjectData>> {
    // `release`, a buffer's only event.
    self.0.store(false, Ordering::Relaxed);
    None
  }

  fn destroyed(&self, _: ObjectId) {}
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

/// How long a window's first frame waits for the pictures its first draw
/// sends to be decoded, at most.
pub(crate) const FIRST_FRAME_HOLD: std::time::Duration = std::time::Duration::from_millis(250);

/// A window's first frame held back for the pictures its draw sent to be
/// decoded.
pub(crate) struct Hold {
  pub(crate) since: std::time::Instant,
  /// The one-shot timer that ends the wait, while it runs.
  pub(crate) timer: Option<calloop::RegistrationToken>,
  /// The timer fired: the next draw commits whatever it shows.
  pub(crate) ended: bool,
}

/// What a surface is to the compositor.
pub(crate) enum Role {
  /// A panel's layer surface.
  Layer(LayerSurface),
  /// A popup.
  Popup(PopupRole),
  /// A floating window's `xdg_toplevel`.
  Toplevel(ToplevelWindow),
  /// A lock surface, while the session locks.
  Lock(crate::lock::LockRole),
}

/// A popup's `xdg_popup` and how it was placed.
pub(crate) struct PopupRole {
  pub(crate) popup: Popup,
  /// Kept to send a new size with: its anchor rectangle and gravity stay.
  pub(crate) positioner: XdgPositioner,
  /// The size last sent for the compositor to place it at.
  pub(crate) placed: (u32, u32),
  /// The token of the last `reposition`, which its configure carries.
  pub(crate) token: u32,
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
  buffers: Buffers<ShmSlot>,
  pub(crate) frames: Frames,
  /// The logical size from the last configure.
  pub(crate) size: Option<(u32, u32)>,
  /// The logical size and scale the surface was last told, so they are
  /// only sent when they change.
  told: Option<((u32, u32), Scale)>,
  /// Frames committed.
  pub(crate) commits: u64,
  /// The first frame's wait for pictures being decoded, once held.
  pub(crate) held: Option<Hold>,
  /// What the last frame reported with `damage_buffer`.
  pub(crate) last_damage: Vec<PixelRect>,
  /// The size of the last buffer committed.
  pub(crate) buffer_size: Option<(u32, u32)>,
  /// A panel's layer-shell namespace, which can't change once sent.
  pub(crate) namespace: String,
  /// A panel's layer-shell settings last sent.
  pub(crate) layer_sent: Option<SurfaceConfig>,
  /// The size the last configure gave, 0 where it left a side free.
  pub(crate) given: (u32, u32),
  /// A floating window's settings last sent.
  pub(crate) floating: Option<crabshell_ui::FloatingConfig>,
  /// The input mask last sent and the size it was cut to.
  mask_sent: Option<(Option<Region>, (u32, u32))>,
  /// The blur object, once the window asked for a blur.
  pub(crate) blur: Option<crate::blur::Blur>,
  /// The blur region last sent and the size it was cut to.
  pub(crate) blur_sent: Option<(Option<Region>, (u32, u32))>,
  /// The idle inhibitor, while the window asks for one.
  pub(crate) inhibitor: Option<crate::idle::Inhibitor>,
  /// The shortcuts inhibitor, while the window asks for one.
  pub(crate) shortcuts_inhibitor: Option<crate::shortcuts_inhibit::ShortcutsInhibitor>,
  /// Hyprland's object, once the window asked for a compositor opacity
  /// or a visible mask.
  pub(crate) hyprland: Option<crate::hyprland_surface::HyprlandSurface>,
  /// What Hyprland was last told, and the size and scale it was cut at.
  pub(crate) hyprland_told: crate::hyprland_surface::Told,
}

impl Surface {
  pub(crate) fn new(
    role: Role,
    fractional: Option<(WpFractionalScaleV1, WpViewport)>,
    scale: Scale,
  ) -> Surface {
    Surface {
      role,
      fractional,
      buffers: Buffers::new(),
      frames: Frames::new(scale),
      size: None,
      told: None,
      commits: 0,
      held: None,
      last_damage: Vec::new(),
      buffer_size: None,
      namespace: String::new(),
      layer_sent: None,
      given: (0, 0),
      floating: None,
      mask_sent: None,
      blur: None,
      blur_sent: None,
      inhibitor: None,
      shortcuts_inhibitor: None,
      hyprland: None,
      hyprland_told: crate::hyprland_surface::Told::default(),
    }
  }

  /// How many buffers hold memory.
  pub(crate) fn buffers(&self) -> usize {
    self.buffers.held()
  }

  /// Gives back the buffers the compositor isn't reading, but the one
  /// drawn last, which holds the frame shown.
  pub(crate) fn trim_buffers(&mut self) {
    self.buffers.trim();
  }

  pub(crate) fn wl_surface(&self) -> &WlSurface {
    match &self.role {
      Role::Layer(layer) => layer.wl_surface(),
      Role::Popup(popup) => popup.popup.wl_surface(),
      Role::Toplevel(window) => window.wl_surface(),
      Role::Lock(lock) => &lock.wl_surface,
    }
  }

  /// The popup role, if this is a popup.
  pub(crate) fn popup(&self) -> Option<&PopupRole> {
    match &self.role {
      Role::Popup(popup) => Some(popup),
      Role::Layer(_) | Role::Toplevel(_) | Role::Lock(_) => None,
    }
  }

  /// A popup is drawn at the size its layout just worked out, and asks
  /// the compositor for the size its content wants.
  pub(crate) fn fit(&mut self, window: &Window) {
    if let Role::Popup(_) = self.role {
      self.size = Some(whole(window.size()));
      self.place_again(window);
    }
  }

  /// Sends the size a popup's content wants so the compositor places it
  /// again, once it is mapped: `reposition` is only for a mapped popup. A
  /// no-op before `xdg_wm_base` version 3, where the new size still shows.
  fn place_again(&mut self, window: &Window) {
    let Role::Popup(popup) = &mut self.role else {
      return;
    };
    let wanted = whole(window.wanted_size());
    if self.commits == 0 || popup.placed == wanted {
      return;
    }
    popup.placed = wanted;
    popup.token = popup.token.wrapping_add(1);
    let [width, height] = [wanted.0, wanted.1].map(|side| i32::try_from(side).unwrap_or(i32::MAX));
    popup.positioner.set_size(width, height);
    popup.popup.reposition(&popup.positioner, popup.token);
  }

  /// Takes a configure's size. A panel keeps each side it last sent as
  /// non-zero, since layer-shell's size is a hint, so a configure for an
  /// older size can't undo a newer resize; other sides and roles take the
  /// compositor's. Returns the size, or `None` if it can't be drawn at.
  pub(crate) fn configure(
    &mut self,
    new_size: (u32, u32),
    config: &SurfaceConfig,
  ) -> Option<(u32, u32)> {
    let size = match (&self.role, &self.layer_sent) {
      (Role::Layer(_), Some(sent)) => configured_size((sent.width, sent.height), new_size)?,
      _ => configured_size(new_size, (config.width, config.height))?,
    };
    self.size = Some(size);
    self.frames.pacing.configure();
    Some(size)
  }

  /// Sends a panel's new layer-shell settings from the window's
  /// description, which must have passed [`size_problem`], and commits.
  /// The compositor answers with a configure. Returns whether it sent
  /// anything: not when only the mask or other hints changed, since
  /// layer-shell would have the compositor arrange the output again.
  pub(crate) fn reconfigure(&mut self, config: &SurfaceConfig) -> bool {
    let sent = self.send_layer(config);
    if sent && let Role::Layer(surface) = &self.role {
      surface.commit();
    }
    sent
  }

  /// Takes the size a panel's settings ask for once a layout changed it,
  /// so the next frame is drawn at that size and its commit carries the
  /// settings. A side left to the compositor keeps its size. Returns
  /// whether the size changed, which needs another layout. Settings
  /// layer-shell would refuse are left for the next pass to log.
  pub(crate) fn resize(&mut self, window: &Window, config: &SurfaceConfig) -> bool {
    let (Role::Layer(_), Some(size), Some(sent)) = (&self.role, self.size, &self.layer_sent) else {
      return false;
    };
    // Only the window's own ask, never over a size the compositor chose.
    if (sent.width, sent.height) == (config.width, config.height) || size_problem(config).is_some()
    {
      return false;
    }
    // The same size has no frame to carry the settings: `reconfigure`
    // commits them.
    let Some(asked) =
      configured_size((config.width, config.height), size).filter(|&asked| asked != size)
    else {
      return false;
    };
    self.send_layer(config);
    self.size = Some(asked);
    window.set_size(Size::new(asked.0 as f32, asked.1 as f32));
    true
  }

  /// Sends what [`reconfigure`](Self::reconfigure) sends, without the
  /// commit.
  fn send_layer(&mut self, config: &SurfaceConfig) -> bool {
    let Role::Layer(surface) = &self.role else {
      return false;
    };
    if self.namespace != namespace(&config.namespace) {
      tracing::warn!(
        shown = self.namespace,
        new = config.namespace,
        "a window's namespace changed; it applies when the window is shown again"
      );
      self.namespace = namespace(&config.namespace);
    }
    if self
      .layer_sent
      .as_ref()
      .is_some_and(|sent| !layer_changed(sent, config))
    {
      return false;
    }
    apply(surface, config);
    if let SurfaceKind::Wlr(wlr) = surface.kind() {
      // Changing the layer later needs version 2.
      if wlr.version() >= 2 {
        surface.set_layer(layer(config.layer));
      }
    }
    self.layer_sent = Some(config.clone());
    true
  }

  /// Whether the input mask was cut to another size than the surface's.
  pub(crate) fn mask_outdated(&self) -> bool {
    self.size.is_some() && self.mask_sent.as_ref().map(|(_, size)| *size) != self.size
  }

  /// Sends `mask`, cut to the surface's size, unless that was sent last,
  /// and says whether it did. The next commit applies it. Nothing is sent
  /// before the first configure, which gives the size.
  pub(crate) fn send_mask(&mut self, compositor: &CompositorState, mask: Option<&Region>) -> bool {
    let Some(size) = self.size else {
      return false;
    };
    if self
      .mask_sent
      .as_ref()
      .is_some_and(|(sent, sent_size)| sent.as_ref() == mask && *sent_size == size)
    {
      return false;
    }
    let surface = self.wl_surface();
    match mask {
      None => surface.set_input_region(None),
      Some(mask) => match compositor::Region::new(compositor) {
        Ok(region) => {
          for [x, y, width, height] in mask.rects(size.0, size.1) {
            region.add(x, y, width, height);
          }
          surface.set_input_region(Some(region.wl_region()));
        }
        Err(error) => {
          tracing::warn!(%error, "couldn't create a region for a window's mask");
          return false;
        }
      },
    }
    self.mask_sent = Some((mask.cloned(), size));
    true
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
    shm: &Shm,
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
    let Surface {
      buffers,
      frames,
      held,
      commits,
      ..
    } = self;
    let slot = buffers.next((buffer_width, buffer_height), || {
      ShmSlot::new(shm, buffer_width, buffer_height)
        .inspect_err(|error| {
          tracing::warn!(%error, buffer_width, buffer_height, "couldn't allocate a buffer");
        })
        .ok()
    });
    let Some(slot) = slot else {
      return false;
    };
    let pixels = &mut slot.buffer.pool.mmap()[..];
    let canvas = (pixels, buffer_width, buffer_height);
    let Some(report) = frames.draw(window, renderer, text, canvas, &mut slot.age) else {
      return false;
    };
    // A first frame without its pictures would flash: a lock surface's
    // locks the screen, and a wallpaper's shows only its background.
    if *commits == 0 && renderer.waits_for_pictures() {
      let hold = held.get_or_insert_with(|| Hold {
        since: std::time::Instant::now(),
        timer: None,
        ended: false,
      });
      if !hold.ended && hold.since.elapsed() < FIRST_FRAME_HOLD {
        // Not shown, so the next draw, when they come in, repaints all.
        frames.history = DamageHistory::new();
        frames.pacing.held();
        return false;
      }
    }
    // Not `self.wl_surface()`: `frames` is borrowed from `self`.
    let surface = match &self.role {
      Role::Layer(layer) => layer.wl_surface(),
      Role::Popup(popup) => popup.popup.wl_surface(),
      Role::Toplevel(window) => window.wl_surface(),
      Role::Lock(lock) => &lock.wl_surface,
    };
    surface.attach(Some(&slot.buffer.buffer), 0, 0);
    slot.buffer.read.0.store(true, Ordering::Relaxed);
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
    self.place_again(window);
    true
  }
}

/// A popup's size in the whole pixels its layout rounded it to.
fn whole(size: Size) -> (u32, u32) {
  (size.width as u32, size.height as u32)
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

/// `config` as a shell sends it while it isn't alone: a positive
/// exclusive zone becomes -1, so the panel takes no space beside the old
/// shell's.
pub(crate) fn beside(mut config: SurfaceConfig, alone: bool) -> SurfaceConfig {
  if !alone && config.exclusive_zone > 0 {
    config.exclusive_zone = -1;
  }
  config
}

/// Whether `config` changes a layer-shell setting `sent` didn't have.
/// The mask goes with a frame, and the namespace is read once.
pub(crate) fn layer_changed(sent: &SurfaceConfig, config: &SurfaceConfig) -> bool {
  let settings = |config: &SurfaceConfig| {
    (
      config.anchors,
      config.width,
      config.height,
      config.exclusive_zone,
      config.layer,
      config.margins,
      config.keyboard_focus,
    )
  };
  settings(sent) != settings(config)
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
  let margins = config.margins;
  surface.set_anchor(anchor(config.anchors));
  surface.set_size(config.width, config.height);
  surface.set_exclusive_zone(config.exclusive_zone);
  surface.set_margin(margins.top, margins.right, margins.bottom, margins.left);
  let version = match surface.kind() {
    SurfaceKind::Wlr(wlr) => wlr.version(),
    _ => 1,
  };
  surface.set_keyboard_interactivity(keyboard(config.keyboard_focus, version));
}

/// The keyboard interactivity for `focus` on a layer surface of
/// `version`. On-demand focus came in version 4; before it, asking for it
/// is a protocol error, so the window takes no keys instead of all of them.
pub(crate) fn keyboard(focus: KeyboardFocus, version: u32) -> KeyboardInteractivity {
  match focus {
    KeyboardFocus::None => KeyboardInteractivity::None,
    KeyboardFocus::Exclusive => KeyboardInteractivity::Exclusive,
    KeyboardFocus::OnDemand if version >= 4 => KeyboardInteractivity::OnDemand,
    KeyboardFocus::OnDemand => {
      static WARNED: AtomicBool = AtomicBool::new(false);
      if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
          version,
          "the compositor's layer-shell can't give focus on demand; such windows take no keyboard focus"
        );
      }
      KeyboardInteractivity::None
    }
  }
}

/// The namespace to send for `asked`. The protocol's strings can't hold a
/// NUL byte, and sending one panics in the bindings, so such a name is
/// logged and replaced with the default.
pub(crate) fn namespace(asked: &str) -> String {
  if asked.contains('\0') {
    tracing::warn!(
      namespace = asked,
      "a namespace can't hold a NUL byte; using the default"
    );
    SurfaceConfig::default().namespace
  } else {
    asked.to_owned()
  }
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
