//! Popups: taking the ones widgets opened, placing each next to its
//! widget with `xdg_positioner`, and what the compositor says about them.

use crabshell_ui::{Edge, Rect};
use smithay_client_toolkit::error::GlobalError;
use smithay_client_toolkit::globals::ProvidesBoundGlobal;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::shell::xdg::XdgPositioner;
use smithay_client_toolkit::shell::xdg::popup::{Popup, PopupConfigure, PopupHandler};
use smithay_client_toolkit::shm::slot::SlotPool;
use wayland_protocols::xdg::shell::client::xdg_positioner::{
  Anchor, ConstraintAdjustment, Gravity,
};
use wayland_protocols::xdg::shell::client::xdg_wm_base::XdgWmBase;

use crate::platform::{INITIAL_POOL_BYTES, Platform};
use crate::surface::{PopupRole, Role, Surface};

/// The newest `xdg_wm_base` sctk's popups know.
pub(crate) const WM_BASE_VERSION: u32 = 6;

/// `xdg_wm_base`, for sctk's popups. sctk's own `XdgShell` binds the
/// window decoration manager too, which a shell has no use for.
pub(crate) struct WmBase(pub(crate) XdgWmBase);

impl ProvidesBoundGlobal<XdgWmBase, 5> for WmBase {
  fn bound_global(&self) -> Result<XdgWmBase, GlobalError> {
    Ok(self.0.clone())
  }
}

impl ProvidesBoundGlobal<XdgWmBase, WM_BASE_VERSION> for WmBase {
  fn bound_global(&self) -> Result<XdgWmBase, GlobalError> {
    Ok(self.0.clone())
  }
}

/// Which side of the widget that opened it a popup goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
  /// Below it, as from a horizontal bar.
  Below,
  /// To its right, as a submenu, or from a vertical bar.
  Beside,
}

impl Side {
  /// Beside a widget in a popup or a vertical bar, and below one in any
  /// other bar. `edge` is the bar's.
  pub(crate) fn of(in_popup: bool, edge: Option<Edge>) -> Side {
    match (in_popup, edge) {
      (true, _) | (false, Some(Edge::Left | Edge::Right)) => Side::Beside,
      _ => Side::Below,
    }
  }
}

impl Platform {
  /// Adds the popups each window's widgets opened since the last call.
  pub(crate) fn take_popups(&mut self) {
    let opened: Vec<_> = self
      .windows
      .entries
      .iter()
      .flat_map(|entry| {
        let id = entry.id;
        entry
          .window
          .take_popups()
          .into_iter()
          .map(move |popup| (id, popup))
      })
      .collect();
    for (opener, popup) in opened {
      self.windows.add_popup(opener, popup);
    }
  }

  /// Shows the popup at `index` once the window that opened it is on
  /// screen: lays it out for its size, places it next to the widget, and
  /// grabs the pointer with the last button event's serial if its opener
  /// is a panel or grabbed too.
  pub(crate) fn create_popup(&mut self, index: usize) {
    let entry = &self.windows.entries[index];
    let Some((opener, rect)) = entry.opener else {
      return;
    };
    let Some(parent) = self.windows.get(opener) else {
      return;
    };
    // xdg-shell needs the parent mapped first.
    let Some((parent_surface, parent_size)) = parent
      .surface
      .as_ref()
      .filter(|surface| surface.commits > 0)
      .and_then(|surface| Some((surface, surface.size?)))
    else {
      return;
    };
    let side = Side::of(
      parent_surface.popup().is_some(),
      parent.window.surface().anchors.edge(),
    );
    let (layer, parent_xdg, may_grab) = match &parent_surface.role {
      Role::Layer(layer) => (Some(layer.clone()), None, true),
      Role::Popup(popup) => (None, Some(popup.popup.xdg_surface().clone()), popup.grabbed),
    };
    let grab = self.last_button.clone().filter(|_| may_grab);
    let scale = parent_surface.frames.scale;
    entry.window.layout(&mut self.text);
    let size = entry.window.size();
    let size = (size.width as u32, size.height as u32);
    let pool = match SlotPool::new(INITIAL_POOL_BYTES, &self.shm) {
      Ok(pool) => pool,
      Err(error) => {
        return tracing::warn!(%error, "couldn't create a shared-memory pool");
      }
    };
    let positioner = positioner(&self.xdg, size, anchor_rect(rect, parent_size), side);
    let wl_surface = self.compositor.create_surface(&self.qh);
    let popup = Popup::from_surface(
      parent_xdg.as_ref(),
      &positioner,
      &self.qh,
      wl_surface,
      &self.xdg,
    )
    .expect(BOUND);
    let fractional = self.scale_objects(popup.wl_surface());
    if let Some(layer) = &layer {
      layer.get_popup(popup.xdg_popup());
    }
    if let Some((seat, serial)) = &grab {
      popup.xdg_popup().grab(seat, *serial);
    }
    // An empty commit asks for the first configure.
    popup.wl_surface().commit();
    tracing::debug!(?size, ?side, grab = grab.is_some(), "created a popup");
    let role = Role::Popup(PopupRole {
      popup,
      positioner,
      placed: size,
      grabbed: grab.is_some(),
      position: None,
    });
    let mut surface = Surface::new(role, fractional, pool, scale);
    surface.size = Some(size);
    self.windows.entries[index].surface = Some(surface);
  }

  fn by_popup(&mut self, popup: &Popup) -> Option<(crate::windows::WindowId, &mut Surface)> {
    self.windows.entries.iter_mut().find_map(|entry| {
      let id = entry.id;
      entry
        .surface
        .as_mut()
        .filter(|surface| surface.popup().is_some_and(|ours| ours.popup == *popup))
        .map(|surface| (id, surface))
    })
  }
}

/// sctk fails only when the global isn't bound, and `WmBase` always is.
const BOUND: &str = "invariant: WmBase holds a bound xdg_wm_base";

/// A positioner for a popup of `size` next to `anchor`, flipped or slid
/// to stay on the output.
fn positioner(
  xdg: &WmBase,
  (width, height): (u32, u32),
  [x, y, anchor_width, anchor_height]: [i32; 4],
  side: Side,
) -> XdgPositioner {
  let positioner = XdgPositioner::new(xdg).expect(BOUND);
  let [width, height] = [width, height].map(|side| i32::try_from(side).unwrap_or(i32::MAX));
  positioner.set_size(width, height);
  positioner.set_anchor_rect(x, y, anchor_width, anchor_height);
  let (anchor, gravity) = match side {
    Side::Below => (Anchor::BottomLeft, Gravity::BottomRight),
    Side::Beside => (Anchor::TopRight, Gravity::BottomRight),
  };
  positioner.set_anchor(anchor);
  positioner.set_gravity(gravity);
  positioner.set_constraint_adjustment(
    ConstraintAdjustment::FlipX
      | ConstraintAdjustment::FlipY
      | ConstraintAdjustment::SlideX
      | ConstraintAdjustment::SlideY,
  );
  positioner
}

/// The anchor rectangle for a widget at `rect` in a parent `width` by
/// `height` logical pixels, as `[x, y, width, height]`: whole pixels
/// covering the widget, at least 1 a side, and inside the parent, as
/// `xdg_positioner` requires.
pub(crate) fn anchor_rect(rect: Rect, (width, height): (u32, u32)) -> [i32; 4] {
  // `as` saturates, and turns NaN into 0.
  let span = |start: f32, length: f32, limit: u32| {
    let limit = i32::try_from(limit).unwrap_or(i32::MAX).max(1);
    let low = (start.floor() as i32).clamp(0, limit - 1);
    let high = ((start + length).ceil() as i32).clamp(low + 1, limit);
    (low, high - low)
  };
  let (x, width) = span(rect.x, rect.width, width);
  let (y, height) = span(rect.y, rect.height, height);
  [x, y, width, height]
}

impl PopupHandler for Platform {
  /// sctk has acknowledged the configure; the next commit applies it.
  fn configure(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    popup: &Popup,
    configure: PopupConfigure,
  ) {
    let Some((_, surface)) = self.by_popup(popup) else {
      return;
    };
    if let Role::Popup(ours) = &mut surface.role {
      ours.position = Some(configure.position);
    }
    surface.frames.pacing.configure();
  }

  /// The compositor dismissed the popup, as after a click outside it. It
  /// goes, with the popups it opened.
  fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup) {
    let Some((id, _)) = self.by_popup(popup) else {
      return;
    };
    tracing::debug!(?id, "the compositor dismissed a popup");
    self.windows.remove(id);
  }
}
