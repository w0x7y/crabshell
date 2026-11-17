//! The session lock, through `ext-session-lock-v1`: locking while a
//! [`Lock`] says to, a lock surface on every output until it stops, and
//! unlocking or giving up as the protocol requires.

use crabshell_ui::{Lock, Output, Size, SurfaceConfig};
use smithay_client_toolkit::reexports::client::protocol::wl_callback::{self, WlCallback};
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_surface_v1::{
  self, ExtSessionLockSurfaceV1,
};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_v1::{
  self, ExtSessionLockV1,
};

use crate::platform::Platform;
use crate::scale::Scale;
use crate::surface::{Role, Surface};
use crate::windows::WindowId;

/// The compositor's lock manager, and the locks the shell holds.
pub(crate) struct Locks {
  manager: Option<ExtSessionLockManagerV1>,
  held: Vec<Held>,
  next: u64,
  /// Whether a lock asked without the protocol, which is logged once.
  warned: bool,
}

impl Locks {
  pub(crate) fn new(manager: Option<ExtSessionLockManagerV1>) -> Locks {
    Locks {
      manager,
      held: Vec::new(),
      next: 0,
      warned: false,
    }
  }

  /// The manager; without one, that is logged once.
  pub(crate) fn manager(&mut self) -> Option<ExtSessionLockManagerV1> {
    if self.manager.is_none() && !std::mem::replace(&mut self.warned, true) {
      tracing::warn!("the compositor lacks ext-session-lock-v1, so the session is never locked");
    }
    self.manager.clone()
  }
}

/// A lock from [`Platform::add_session_lock`], to end it with
/// [`Platform::remove_session_lock`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LockId(u64);

struct Held {
  id: u64,
  /// `None` once its context went while it held the session, or was
  /// about to: the session stays locked.
  lock: Option<Lock>,
  state: State,
  /// Its lock surfaces' windows.
  surfaces: Vec<WindowId>,
}

enum State {
  /// Not locked. `refused`: the compositor refused or ended the lock, or
  /// has no protocol, since `locked` last turned true.
  Unlocked { refused: bool },
  /// Asked to lock; waiting for `locked` or `finished`.
  Asked(ExtSessionLockV1),
  /// The compositor said `locked`.
  Locked(ExtSessionLockV1),
}

/// A lock surface and its `wl_surface`, destroyed in that order.
pub(crate) struct LockRole {
  role: ExtSessionLockSurfaceV1,
  pub(crate) wl_surface: WlSurface,
}

impl Drop for LockRole {
  fn drop(&mut self) {
    self.role.destroy();
    self.wl_surface.destroy();
  }
}

impl Platform {
  /// Locks the session whenever `lock`'s `locked` turns true, giving
  /// every output a lock surface built from it, and unlocks when it turns
  /// false. Its `locked` signal says when the compositor has locked. If
  /// the compositor refuses, as when another program holds the lock,
  /// that is logged and it asks again the next time `locked` turns true.
  /// Without `ext-session-lock-v1`, that is logged once and it never
  /// locks.
  pub fn add_session_lock(&mut self, lock: Lock) -> LockId {
    let id = self.locks.next;
    self.locks.next += 1;
    self.locks.held.push(Held {
      id,
      lock: Some(lock),
      state: State::Unlocked { refused: false },
      surfaces: Vec::new(),
    });
    LockId(id)
  }

  /// Drops a lock and its surfaces. If it holds the session, or the
  /// compositor hasn't answered yet, the session stays locked, showing the
  /// compositor's color, even after the shell exits: nothing unlocks it
  /// but the lock's own `locked`, and destroying a lock the compositor
  /// locked is a protocol error.
  pub fn remove_session_lock(&mut self, lock: LockId) {
    let Some(index) = self.locks.held.iter().position(|held| held.id == lock.0) else {
      return;
    };
    let held = &mut self.locks.held[index];
    let surfaces = std::mem::take(&mut held.surfaces);
    if matches!(held.state, State::Unlocked { .. }) {
      self.locks.held.remove(index);
    } else {
      held.lock = None;
      tracing::warn!("a session lock went while it held the session, so the session stays locked");
    }
    for id in surfaces {
      self.windows.remove(id);
    }
  }

  /// How many session locks are held, one kept after its context went
  /// included, as tests check they end.
  pub fn session_locks(&self) -> usize {
    self.locks.held.len()
  }

  /// Locks or unlocks each lock whose `locked` changed. Returns whether
  /// it did either, which set a signal or built surfaces whose bindings
  /// need a flush.
  pub(crate) fn update_locks(&mut self) -> bool {
    let mut acted = false;
    for index in 0..self.locks.held.len() {
      let held = &mut self.locks.held[index];
      // A gone lock keeps the session as it is.
      let Some(wanted) = held.lock.as_ref().and_then(Lock::wanted) else {
        continue;
      };
      match (&held.state, wanted) {
        (State::Unlocked { refused: true }, false) => {
          held.state = State::Unlocked { refused: false };
        }
        (State::Unlocked { refused: false }, true) => {
          self.lock_session(index);
          acted = true;
        }
        (State::Locked(_), false) => {
          self.end_session(index, false);
          tracing::info!("unlocked the session");
          acted = true;
        }
        // Asked and no longer wanted: destroying it now is a protocol
        // error if a `locked` is on its way, so the answer decides.
        _ => {}
      }
    }
    acted
  }

  /// Asks the compositor to lock for lock `index`, and gives every output
  /// a lock surface at once, as the protocol asks.
  fn lock_session(&mut self, index: usize) {
    let Some(manager) = self.locks.manager() else {
      self.locks.held[index].state = State::Unlocked { refused: true };
      return;
    };
    let held = &mut self.locks.held[index];
    held.state = State::Asked(manager.lock(&self.qh, held.id));
    tracing::info!("asked the compositor to lock the session");
    let outputs: Vec<WlOutput> = self
      .known
      .iter()
      .map(|(output, _)| output.clone())
      .collect();
    for output in &outputs {
      self.add_lock_surface(index, output);
    }
  }

  /// Gives every lock that holds the session, or asked to, a surface on
  /// `output`, just announced.
  pub(crate) fn lock_output(&mut self, output: &WlOutput) {
    for index in 0..self.locks.held.len() {
      self.add_lock_surface(index, output);
    }
  }

  /// Builds lock `index`'s surface for `output` and creates it. The
  /// compositor configures it at once; nothing is committed before.
  fn add_lock_surface(&mut self, index: usize, output: &WlOutput) {
    let held = &self.locks.held[index];
    let (State::Asked(session) | State::Locked(session)) = &held.state else {
      return;
    };
    let (Some(lock), Some((_, info))) = (
      &held.lock,
      self.known.iter().find(|(known, _)| known == output),
    ) else {
      return;
    };
    let Some(window) = lock.surface(&Output::new(info.name.clone())) else {
      return;
    };
    let session = session.clone();
    let scale = Scale::integer(info.scale).unwrap_or(Scale::Integer(1));
    let wl_surface = self.compositor.create_surface(&self.qh);
    let fractional = self.scale_objects(&wl_surface);
    let scale = if fractional.is_some() {
      scale.as_fractional()
    } else {
      scale
    };
    let id = self.windows.add(window);
    let role = session.get_lock_surface(&wl_surface, output, &self.qh, id);
    let entry = self
      .windows
      .entries
      .last_mut()
      .expect("invariant: the window just added is the last");
    let role = Role::Lock(LockRole { role, wl_surface });
    entry.surface = Some(Surface::new(role, fractional, scale));
    entry.output = Some(output.clone());
    self.locks.held[index].surfaces.push(id);
  }

  /// Ends lock `index`'s session as the protocol says, `unlock_and_destroy`
  /// once locked and `destroy` before, drops its surfaces and says it is
  /// unlocked: after an unlock, once the compositor has it. A lock whose
  /// context went is forgotten.
  fn end_session(&mut self, index: usize, refused: bool) {
    let held = &mut self.locks.held[index];
    match std::mem::replace(&mut held.state, State::Unlocked { refused }) {
      State::Locked(session) => {
        session.unlock_and_destroy();
        // A config may exit as soon as it hears, which must not leave the
        // unlock unsent; the protocol asks for this sync.
        self.connection.display().sync(&self.qh, Unlocked(held.id));
      }
      State::Asked(session) => {
        session.destroy();
        if let Some(lock) = &held.lock {
          lock.set_locked(false);
        }
      }
      State::Unlocked { .. } => {}
    }
    let surfaces = std::mem::take(&mut held.surfaces);
    if held.lock.is_none() {
      self.locks.held.remove(index);
    }
    for id in surfaces {
      self.windows.remove(id);
    }
  }
}

impl Dispatch<ExtSessionLockV1, u64> for Platform {
  fn event(
    platform: &mut Platform,
    _: &ExtSessionLockV1,
    event: ext_session_lock_v1::Event,
    id: &u64,
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let Some(index) = platform.locks.held.iter().position(|held| held.id == *id) else {
      return;
    };
    match event {
      ext_session_lock_v1::Event::Locked => {
        let held = &mut platform.locks.held[index];
        if let State::Asked(session) = &held.state {
          held.state = State::Locked(session.clone());
        }
        if let Some(lock) = &held.lock {
          lock.set_locked(true);
        }
        tracing::info!("the compositor locked the session");
      }
      ext_session_lock_v1::Event::Finished => {
        platform.end_session(index, true);
        tracing::warn!(
          "the compositor refused or ended the session lock, as when another program holds it; it asks again once locked turns false and true"
        );
      }
      _ => {}
    }
  }
}

impl Dispatch<ExtSessionLockSurfaceV1, WindowId> for Platform {
  /// The size is the output's, and exact: the next commit must match it.
  fn event(
    platform: &mut Platform,
    surface: &ExtSessionLockSurfaceV1,
    event: ext_session_lock_surface_v1::Event,
    id: &WindowId,
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let ext_session_lock_surface_v1::Event::Configure {
      serial,
      width,
      height,
    } = event
    else {
      return;
    };
    surface.ack_configure(serial);
    let Some(entry) = platform
      .windows
      .entries
      .iter_mut()
      .find(|entry| entry.id == *id)
    else {
      return;
    };
    let Some(ours) = entry.surface.as_mut() else {
      return;
    };
    match ours.configure((width, height), &SurfaceConfig::default()) {
      Some((width, height)) => entry
        .window
        .set_size(Size::new(width as f32, height as f32)),
      None => tracing::warn!(
        width,
        height,
        "ignoring a lock surface's configure with a size that can't be drawn"
      ),
    }
  }
}

/// The id of a lock whose unlock the compositor has once this sync is
/// done.
pub(crate) struct Unlocked(u64);

impl Dispatch<WlCallback, Unlocked> for Platform {
  fn event(
    platform: &mut Platform,
    _: &WlCallback,
    event: wl_callback::Event,
    unlocked: &Unlocked,
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    if let wl_callback::Event::Done { .. } = event
      && let Some(held) = platform
        .locks
        .held
        .iter()
        .find(|held| held.id == unlocked.0)
      && let Some(lock) = &held.lock
    {
      lock.set_locked(false);
    }
  }
}

// It has no events.
delegate_noop!(Platform: ExtSessionLockManagerV1);
