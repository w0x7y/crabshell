//! Holding the keyboard and pointer focus on some of the shell's windows
//! until the user clicks elsewhere, through Hyprland's
//! `hyprland-focus-grab-v1`.

use crabshell_reactive::{Signal, untrack};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};

use crate::hyprland::focus_grab::hyprland_focus_grab_manager_v1::{
  self, HyprlandFocusGrabManagerV1,
};
use crate::hyprland::focus_grab::hyprland_focus_grab_v1::{self, HyprlandFocusGrabV1};
use crate::platform::Platform;
use crate::surface::Surface;
use crate::windows::Windows;

/// The compositor's focus grab manager and the grabs held.
pub(crate) struct FocusGrabs {
  pub(crate) manager: Option<HyprlandFocusGrabManagerV1>,
  pub(crate) grabs: Grabs<Grab, WlSurface>,
  /// Whether a window asked without the protocol, which is logged once.
  warned: bool,
}

impl FocusGrabs {
  pub(crate) fn new(manager: Option<HyprlandFocusGrabManagerV1>) -> FocusGrabs {
    FocusGrabs {
      manager,
      grabs: Grabs::default(),
      warned: false,
    }
  }
}

/// A grab object, destroyed when its grab ends.
pub(crate) struct Grab(HyprlandFocusGrabV1);

impl Drop for Grab {
  fn drop(&mut self) {
    self.0.destroy();
  }
}

/// The grabs held, one per `focus_grab` signal, with the surfaces each
/// was last sent. Generic over the grab object and the surface, so it is
/// tested without a compositor.
pub(crate) struct Grabs<G, K> {
  list: Vec<Held<G, K>>,
  next: u64,
}

impl<G, K> Default for Grabs<G, K> {
  fn default() -> Grabs<G, K> {
    Grabs {
      list: Vec::new(),
      next: 0,
    }
  }
}

struct Held<G, K> {
  /// What its grab object's events carry.
  id: u64,
  signal: Signal<bool>,
  /// Dropped when the grab ends.
  grab: G,
  surfaces: Vec<K>,
}

impl<G, K: Clone + PartialEq> Grabs<G, K> {
  /// Brings the grabs in line with `wanted`, the surfaces each `true`
  /// signal's grab should hold. `make` makes a grab object, given the id
  /// its events carry, and `change` sends one the surfaces added and
  /// removed, then commits. A grab whose signal turned false or went
  /// ends. One whose surfaces all went ends too, and its signal turns
  /// false, as the compositor's `cleared` would; whether one did, which
  /// needs a flush.
  pub(crate) fn sync(
    &mut self,
    mut wanted: Vec<(Signal<bool>, Vec<K>)>,
    mut make: impl FnMut(u64) -> G,
    mut change: impl FnMut(&G, &[K], &[K]),
  ) -> bool {
    let mut cleared = false;
    self.list.retain(|held| {
      let kept = wanted.iter().any(|(signal, _)| *signal == held.signal);
      if !kept && untrack(|| held.signal.try_get()) == Some(true) {
        cleared |= held.signal.try_set(false);
      }
      kept
    });
    for (signal, surfaces) in wanted.drain(..) {
      let index = match self.list.iter().position(|held| held.signal == signal) {
        Some(index) => index,
        None => {
          let id = self.next;
          self.next += 1;
          self.list.push(Held {
            id,
            signal,
            grab: make(id),
            surfaces: Vec::new(),
          });
          self.list.len() - 1
        }
      };
      let held = &mut self.list[index];
      let added: Vec<K> = surfaces
        .iter()
        .filter(|surface| !held.surfaces.contains(surface))
        .cloned()
        .collect();
      let removed: Vec<K> = held
        .surfaces
        .iter()
        .filter(|surface| !surfaces.contains(surface))
        .cloned()
        .collect();
      if !added.is_empty() || !removed.is_empty() {
        change(&held.grab, &added, &removed);
        held.surfaces = surfaces;
      }
    }
    cleared
  }

  /// The compositor cleared the grab whose events carry `id`: it ends,
  /// and its signal turns false.
  pub(crate) fn cleared(&mut self, id: u64) {
    if let Some(index) = self.list.iter().position(|held| held.id == id) {
      let held = self.list.remove(index);
      held.signal.try_set(false);
    }
  }

  /// How many grabs are held.
  #[cfg(test)]
  pub(crate) fn len(&self) -> usize {
    self.list.len()
  }
}

/// The surfaces each grab should hold: for every `focus_grab` signal that
/// is `true`, those of the windows given it and of the popups they
/// opened, where `shown` gives one, as for a surface that has mapped.
pub(crate) fn wanted<S, O, K: PartialEq>(
  windows: &Windows<S, O>,
  shown: impl Fn(&S) -> Option<K>,
) -> Vec<(Signal<bool>, Vec<K>)> {
  let mut grabbing = Vec::new();
  let mut wanted: Vec<(Signal<bool>, Vec<K>)> = Vec::new();
  for entry in &windows.entries {
    // An opener comes before its popups.
    let signal = match entry.opener {
      Some(opener) => grabbing
        .iter()
        .find(|(id, _)| *id == opener)
        .map(|(_, signal)| *signal),
      None => entry
        .window
        .focus_grab()
        .filter(|signal| untrack(|| signal.try_get()) == Some(true)),
    };
    let Some(signal) = signal else {
      continue;
    };
    grabbing.push((entry.id, signal));
    let Some(surface) = entry.surface.as_ref().and_then(&shown) else {
      continue;
    };
    match wanted.iter_mut().find(|(held, _)| *held == signal) {
      Some((_, surfaces)) => surfaces.push(surface),
      None => wanted.push((signal, vec![surface])),
    }
  }
  wanted
}

impl Platform {
  /// Makes, changes and ends the focus grabs as the windows' `focus_grab`
  /// signals and surfaces say, after the windows drew. A signal it turns
  /// false wakes the loop, whose next turn flushes it.
  pub(crate) fn update_focus_grabs(&mut self) {
    let wanted = wanted(&self.windows, |surface: &Surface| {
      (surface.commits > 0).then(|| surface.wl_surface().clone())
    });
    let FocusGrabs {
      manager,
      grabs,
      warned,
    } = &mut self.focus_grabs;
    let Some(manager) = manager else {
      if !wanted.is_empty() && !std::mem::replace(warned, true) {
        tracing::warn!("the compositor lacks hyprland-focus-grab-v1, so windows grab no focus");
      }
      return;
    };
    let qh = &self.qh;
    let cleared = grabs.sync(
      wanted,
      |id| Grab(manager.create_grab(qh, id)),
      |grab, added, removed| {
        // A destroyed surface left the grab by itself.
        for surface in removed.iter().filter(|surface| surface.is_alive()) {
          grab.0.remove_surface(surface);
        }
        for surface in added {
          grab.0.add_surface(surface);
        }
        grab.0.commit();
      },
    );
    if cleared {
      self.signal.wakeup();
    }
  }
}

impl Dispatch<HyprlandFocusGrabV1, u64> for Platform {
  fn event(
    platform: &mut Platform,
    _: &HyprlandFocusGrabV1,
    event: hyprland_focus_grab_v1::Event,
    id: &u64,
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let hyprland_focus_grab_v1::Event::Cleared = event;
    platform.focus_grabs.grabs.cleared(*id);
  }
}

impl Dispatch<HyprlandFocusGrabManagerV1, ()> for Platform {
  fn event(
    _: &mut Platform,
    _: &HyprlandFocusGrabManagerV1,
    _: hyprland_focus_grab_manager_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
  }
}
