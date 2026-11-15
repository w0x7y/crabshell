//! Floating windows as `xdg_toplevel`s: creating one while its window is
//! shown, sending its settings as they change, dropping it while hidden,
//! and what the compositor says about it.

use crabshell_ui::{Anchors, Edge, FloatingConfig, Size, SurfaceConfig, WindowRequest};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::window::{
  Window as Toplevel, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shm::slot::SlotPool;
use wayland_protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;

use crate::platform::{INITIAL_POOL_BYTES, Platform};
use crate::scale::Scale;
use crate::surface::{Role, Surface};
use crate::windows::WindowId;

impl Platform {
  /// Creates the toplevel of the floating window at `index`, if it is
  /// shown and has none.
  pub(crate) fn create_toplevel(&mut self, index: usize, config: &FloatingConfig) {
    if !config.shown {
      return;
    }
    let pool = match SlotPool::new(INITIAL_POOL_BYTES, &self.shm) {
      Ok(pool) => pool,
      Err(error) => {
        return tracing::warn!(%error, "couldn't create a shared-memory pool");
      }
    };
    let entry = &mut self.windows.entries[index];
    // Read now; later changes are sent as they come.
    entry.window.take_surface_changed();
    let wl_surface = self.compositor.create_surface(&self.qh);
    let fractional = self.scale_objects(&wl_surface);
    // Title bars and borders are the compositor's, where it draws them.
    let toplevel = self
      .xdg
      .create_window(wl_surface, WindowDecorations::RequestServer, &self.qh);
    apply(&toplevel, config, None);
    // An empty commit asks for the first configure.
    toplevel.commit();
    let sent = config.clone();
    let scale = if fractional.is_some() {
      Scale::Integer(1).as_fractional()
    } else {
      Scale::Integer(1)
    };
    tracing::debug!(title = config.title, "created a floating window");
    let entry = &mut self.windows.entries[index];
    let mut surface = Surface::new(Role::Toplevel(toplevel), fractional, pool, scale);
    surface.floating = Some(sent);
    entry.surface = Some(surface);
  }

  /// Sends each shown floating window's changed settings and carries out
  /// its requests, and drops the toplevel of each hidden one, with the
  /// popups it opened.
  pub(crate) fn update_toplevels(&mut self) {
    let mut index = 0;
    // Hiding drops popups, which come later in the list.
    while index < self.windows.entries.len() {
      let entry = &mut self.windows.entries[index];
      let id = entry.id;
      if let (Some(config), Some(surface)) = (entry.window.floating_config(), &mut entry.surface)
        && entry.window.take_surface_changed()
      {
        // Before the commit below, or the next frame's.
        let mut hinted = false;
        if config.shown {
          hinted = surface.send_mask(&self.compositor, entry.window.surface().mask.as_ref());
          let blur = entry.window.blur();
          hinted |= surface.send_blur(&mut self.effects, &self.compositor, blur.as_ref(), &self.qh);
          hinted |= surface.send_hyprland(
            &mut self.hyprland_surfaces,
            &self.registry,
            &self.compositor,
            &entry.window,
            &self.qh,
          );
        }
        if !config.shown {
          self.windows.remove_popups_of(id);
          // Its pointer events stop with its surface.
          self.windows.entries[index].window.pointer_leave();
          self.windows.entries[index].surface = None;
        } else if surface.floating.as_ref() == Some(&config) {
          // Only its hints changed, which need no commit of their own.
          if hinted {
            surface.frames.pacing.redraw();
          }
        } else if let Role::Toplevel(toplevel) = &surface.role {
          apply(toplevel, &config, surface.floating.as_ref());
          toplevel.commit();
          let resized = surface
            .floating
            .as_ref()
            .is_some_and(|old| (old.width, old.height) != (config.width, config.height));
          // The compositor left the size to the window: take the new one.
          if resized
            && let Some((width, height)) = surface.configure(surface.given, &asked(&config))
          {
            entry
              .window
              .set_size(Size::new(width as f32, height as f32));
          }
          surface.floating = Some(config);
        }
      }
      self.window_requests(index);
      index += 1;
    }
  }

  /// Carries out what a floating window's widgets asked, with the seat
  /// and serial of the last pointer button.
  pub(crate) fn window_requests(&mut self, index: usize) {
    let entry = &self.windows.entries[index];
    let requests = entry.window.take_requests();
    let Some(Role::Toplevel(toplevel)) = entry.surface.as_ref().map(|surface| &surface.role) else {
      return;
    };
    // A move or resize with no button held would follow the pointer
    // until the next click.
    let press = self.last_button.as_ref().filter(|_| self.button_held);
    for request in requests {
      match (request, press) {
        (WindowRequest::Minimize, _) => toplevel.set_minimized(),
        (WindowRequest::Move, Some((seat, serial))) => toplevel.move_(seat, *serial),
        (WindowRequest::Resize(edges), Some((seat, serial))) => {
          toplevel.resize(seat, *serial, resize_edge(edges));
        }
        (_, None) => tracing::warn!(
          ?request,
          "moving and resizing need a pointer button held; ask from on_press"
        ),
      }
    }
  }

  fn by_toplevel(&mut self, toplevel: &Toplevel) -> Option<(WindowId, usize)> {
    self.windows.entries.iter().position(|entry| {
      matches!(entry.surface.as_ref().map(|surface| &surface.role), Some(Role::Toplevel(ours)) if ours == toplevel)
    })
    .map(|index| (self.windows.entries[index].id, index))
  }
}

/// A floating window's asked size, as a configure reads it.
fn asked(config: &FloatingConfig) -> SurfaceConfig {
  SurfaceConfig {
    width: config.width,
    height: config.height,
    ..SurfaceConfig::default()
  }
}

/// Sends `config`, or what changed since `old`.
fn apply(toplevel: &Toplevel, config: &FloatingConfig, old: Option<&FloatingConfig>) {
  let changed = |pick: fn(&FloatingConfig) -> bool| old.is_none_or(|old| pick(old) != pick(config));
  if old.is_none_or(|old| old.title != config.title) {
    toplevel.set_title(config.title.clone());
  }
  if old.is_none_or(|old| (old.min_size, old.max_size) != (config.min_size, config.max_size)) {
    let (min, max) = limits(config.min_size, config.max_size);
    toplevel.set_min_size(min);
    toplevel.set_max_size(max);
  }
  if changed(|config| config.maximized) {
    if config.maximized {
      toplevel.set_maximized();
    } else if old.is_some() {
      toplevel.unset_maximized();
    }
  }
  if changed(|config| config.fullscreen) {
    if config.fullscreen {
      toplevel.set_fullscreen(None);
    } else if old.is_some() {
      toplevel.unset_fullscreen();
    }
  }
}

/// The size limits to send for `min` and `max`, where 0 leaves a side
/// free: each side at most 16384, and a maximum below the minimum raised
/// to it, since either would be a protocol error that ends the connection.
pub(crate) fn limits(min: Limit, max: Limit) -> (Option<Limit>, Option<Limit>) {
  let cap = |side: u32| side.min(MAX_SIDE);
  let min = (cap(min.0), cap(min.1));
  let raise = |max: u32, min: u32| if max == 0 { 0 } else { cap(max).max(min) };
  let max = (raise(max.0, min.0), raise(max.1, min.1));
  if max != (cap(max.0), cap(max.1)) {
    tracing::warn!(
      ?min,
      ?max,
      "a floating window's maximum size is below its minimum"
    );
  }
  let set = |(width, height): (u32, u32)| (width > 0 || height > 0).then_some((width, height));
  (set(min), set(max))
}

/// A width and height; 0 leaves a side free.
pub(crate) type Limit = (u32, u32);

/// The largest side sent, as for any window.
const MAX_SIDE: u32 = 16_384;

/// The `xdg_toplevel` resize edge for `edges`; opposite edges cancel.
pub(crate) fn resize_edge(edges: Anchors) -> ResizeEdge {
  let top = edges.contains(Edge::Top) && !edges.contains(Edge::Bottom);
  let bottom = edges.contains(Edge::Bottom) && !edges.contains(Edge::Top);
  let left = edges.contains(Edge::Left) && !edges.contains(Edge::Right);
  let right = edges.contains(Edge::Right) && !edges.contains(Edge::Left);
  match (top, bottom, left, right) {
    (true, _, true, _) => ResizeEdge::TopLeft,
    (true, _, _, true) => ResizeEdge::TopRight,
    (_, true, true, _) => ResizeEdge::BottomLeft,
    (_, true, _, true) => ResizeEdge::BottomRight,
    (true, ..) => ResizeEdge::Top,
    (_, true, ..) => ResizeEdge::Bottom,
    (_, _, true, _) => ResizeEdge::Left,
    (_, _, _, true) => ResizeEdge::Right,
    _ => ResizeEdge::None,
  }
}

impl WindowHandler for Platform {
  /// The user or compositor asked the window to close: its `on_close`
  /// runs, or it hides.
  fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, toplevel: &Toplevel) {
    if let Some((_, index)) = self.by_toplevel(toplevel) {
      self.windows.entries[index].window.request_close();
    }
  }

  /// sctk has acknowledged the configure; the next commit applies it. A
  /// side of 0 leaves the size to the window, which asks for its own.
  fn configure(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    toplevel: &Toplevel,
    configure: WindowConfigure,
    _: u32,
  ) {
    let Some((_, index)) = self.by_toplevel(toplevel) else {
      return;
    };
    let entry = &mut self.windows.entries[index];
    let Some(config) = entry.window.floating_config() else {
      return;
    };
    let given = (
      configure.new_size.0.map_or(0, |width| width.get()),
      configure.new_size.1.map_or(0, |height| height.get()),
    );
    let Some(surface) = entry.surface.as_mut() else {
      return;
    };
    surface.given = given;
    match surface.configure(given, &asked(&config)) {
      Some((width, height)) => entry
        .window
        .set_size(Size::new(width as f32, height as f32)),
      None => tracing::warn!(size = ?given, "ignoring a configure with a size that can't be drawn"),
    }
  }
}
