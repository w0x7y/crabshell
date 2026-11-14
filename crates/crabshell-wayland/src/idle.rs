//! Idleness: watching how long the user has been away, through
//! `ext-idle-notify-v1`, and keeping the screen awake for a shown window,
//! through `idle-inhibit-unstable-v1`.

use std::collections::HashMap;
use std::time::Duration;

use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{
  self, ExtIdleNotificationV1,
};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::{
  self, ExtIdleNotifierV1,
};
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibit_manager_v1::{
  self, ZwpIdleInhibitManagerV1,
};
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::{
  self, ZwpIdleInhibitorV1,
};

use crate::platform::Platform;
use crate::surface::Surface;

/// The compositor's idle protocols, and the watches on them.
pub(crate) struct Idle {
  pub(crate) notifier: Option<ExtIdleNotifierV1>,
  pub(crate) inhibit: Option<ZwpIdleInhibitManagerV1>,
  watches: HashMap<u64, Watch>,
  next: u64,
  /// Whether a window asked to inhibit idle without the protocol, which
  /// is logged once.
  warned: bool,
}

impl Idle {
  pub(crate) fn new(
    notifier: Option<ExtIdleNotifierV1>,
    inhibit: Option<ZwpIdleInhibitManagerV1>,
  ) -> Idle {
    Idle {
      notifier,
      inhibit,
      watches: HashMap::new(),
      next: 0,
      warned: false,
    }
  }
}

struct Watch {
  notification: ExtIdleNotificationV1,
  on_change: Box<dyn FnMut(bool)>,
}

impl Drop for Watch {
  fn drop(&mut self) {
    self.notification.destroy();
  }
}

/// A watch from [`Platform::watch_idle`], to end it with
/// [`Platform::unwatch_idle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IdleWatch(u64);

/// A window's idle inhibitor, destroyed with its surface.
pub(crate) struct Inhibitor(ZwpIdleInhibitorV1);

impl Drop for Inhibitor {
  fn drop(&mut self) {
    self.0.destroy();
  }
}

impl Platform {
  /// Calls `on_change(true)` once the user has been idle for `timeout`,
  /// and `on_change(false)` when they come back. The compositor says
  /// when, so nothing wakes the shell in between. Windows that inhibit
  /// idle, the shell's or other programs', hold it off.
  ///
  /// `None`, logged, if the compositor lacks `ext-idle-notify-v1` or
  /// there is no seat.
  pub fn watch_idle(
    &mut self,
    timeout: Duration,
    on_change: impl FnMut(bool) + 'static,
  ) -> Option<IdleWatch> {
    let Some(notifier) = &self.idle.notifier else {
      tracing::warn!("the compositor lacks ext-idle-notify-v1, so idleness is never seen");
      return None;
    };
    let Some(seat) = self.seats.seats().next() else {
      tracing::warn!("no seat to watch for idleness");
      return None;
    };
    let id = self.idle.next;
    self.idle.next += 1;
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    let notification = notifier.get_idle_notification(ms, &seat, &self.qh, id);
    let on_change = Box::new(on_change);
    self.idle.watches.insert(
      id,
      Watch {
        notification,
        on_change,
      },
    );
    Some(IdleWatch(id))
  }

  /// Ends a watch. One already ended is ignored.
  pub fn unwatch_idle(&mut self, watch: IdleWatch) {
    self.idle.watches.remove(&watch.0);
  }

  /// How many idle watches are on, as tests check they end.
  pub fn idle_watches(&self) -> usize {
    self.idle.watches.len()
  }
}

impl Surface {
  /// Holds an idle inhibitor on this surface while `wanted`, and none
  /// otherwise.
  pub(crate) fn inhibit_idle(&mut self, idle: &mut Idle, wanted: bool, qh: &QueueHandle<Platform>) {
    if self.inhibitor.is_some() == wanted {
      return;
    }
    if !wanted {
      self.inhibitor = None;
      return;
    }
    let Some(manager) = &idle.inhibit else {
      if !std::mem::replace(&mut idle.warned, true) {
        tracing::warn!("the compositor lacks idle-inhibit-unstable-v1, so the screen may idle");
      }
      return;
    };
    let inhibitor = manager.create_inhibitor(self.wl_surface(), qh, ());
    self.inhibitor = Some(Inhibitor(inhibitor));
  }
}

impl Dispatch<ExtIdleNotificationV1, u64> for Platform {
  fn event(
    platform: &mut Platform,
    _: &ExtIdleNotificationV1,
    event: ext_idle_notification_v1::Event,
    id: &u64,
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let idle = match event {
      ext_idle_notification_v1::Event::Idled => true,
      ext_idle_notification_v1::Event::Resumed => false,
      _ => return,
    };
    if let Some(watch) = platform.idle.watches.get_mut(id) {
      (watch.on_change)(idle);
    }
  }
}

impl Dispatch<ExtIdleNotifierV1, ()> for Platform {
  fn event(
    _: &mut Platform,
    _: &ExtIdleNotifierV1,
    _: ext_idle_notifier_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
  }
}

impl Dispatch<ZwpIdleInhibitManagerV1, ()> for Platform {
  fn event(
    _: &mut Platform,
    _: &ZwpIdleInhibitManagerV1,
    _: zwp_idle_inhibit_manager_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
  }
}

impl Dispatch<ZwpIdleInhibitorV1, ()> for Platform {
  fn event(
    _: &mut Platform,
    _: &ZwpIdleInhibitorV1,
    _: zwp_idle_inhibitor_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
  }
}
