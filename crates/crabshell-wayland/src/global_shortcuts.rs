//! Shortcuts the compositor binds and tells the shell about, through
//! Hyprland's `hyprland-global-shortcuts-v1`: a Hyprland config line
//! such as `bind = SUPER, D, global, crabshell:launcher` presses the
//! shortcut `launcher` of the app id `crabshell`.

use std::panic::{self, AssertUnwindSafe};

use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};

use crate::hyprland::global_shortcuts::hyprland_global_shortcut_v1::{
  self, HyprlandGlobalShortcutV1,
};
use crate::hyprland::global_shortcuts::hyprland_global_shortcuts_manager_v1::{
  self, HyprlandGlobalShortcutsManagerV1,
};
use crate::platform::Platform;

/// The most the strings of one registration may hold together, so the
/// request fits the protocol's 4 KiB message.
const MOST_BYTES: usize = 4000;

/// A watch from [`Platform::watch_shortcut`], to end it with
/// [`Platform::unwatch_shortcut`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShortcutWatch(u64);

/// The compositor's shortcuts manager and the shortcuts registered.
pub(crate) struct GlobalShortcuts {
  pub(crate) manager: Option<HyprlandGlobalShortcutsManagerV1>,
  pub(crate) shortcuts: Shortcuts<Registered>,
  /// Whether a watch came without the protocol, which is logged once.
  warned: bool,
}

impl GlobalShortcuts {
  pub(crate) fn new(manager: Option<HyprlandGlobalShortcutsManagerV1>) -> GlobalShortcuts {
    GlobalShortcuts {
      manager,
      shortcuts: Shortcuts::default(),
      warned: false,
    }
  }
}

/// A registered shortcut, destroyed when its last watch ends.
pub(crate) struct Registered(HyprlandGlobalShortcutV1);

impl Drop for Registered {
  fn drop(&mut self) {
    self.0.destroy();
  }
}

/// The shortcuts registered, each once per app id and name however many
/// watch it, since the compositor ends the connection of a client that
/// registers one twice. Generic over the protocol object, so it is tested
/// without a compositor.
pub(crate) struct Shortcuts<P> {
  list: Vec<Shortcut<P>>,
  next: u64,
}

impl<P> Default for Shortcuts<P> {
  fn default() -> Shortcuts<P> {
    Shortcuts {
      list: Vec::new(),
      next: 0,
    }
  }
}

struct Shortcut<P> {
  /// What its protocol object's events carry.
  id: u64,
  app_id: String,
  name: String,
  /// Dropped with the shortcut.
  _object: P,
  held: bool,
  /// Pressed since the last flush.
  fresh: bool,
  /// Released while fresh, so told after the flush shows the press.
  release: bool,
  watches: Vec<(u64, OnChange)>,
}

/// What a watch calls with whether its shortcut is held.
type OnChange = Box<dyn FnMut(bool)>;

impl<P> Shortcut<P> {
  /// Tells every watch whether the shortcut is held. A watch that panics
  /// is logged, and the rest are still told.
  fn tell(&mut self, held: bool) {
    let Shortcut {
      app_id,
      name,
      watches,
      ..
    } = self;
    for (_, on_change) in watches {
      if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| on_change(held))) {
        let message = crate::events::panic_message(payload.as_ref());
        tracing::error!("a watch of the global shortcut {app_id}:{name} panicked: {message}");
      }
    }
  }
}

impl<P> Shortcuts<P> {
  /// Adds a watch on `app_id`'s `name`, registering it with `register`,
  /// given the id its events carry, unless it is registered already. A
  /// watch on a held shortcut hears `true` at once.
  pub(crate) fn watch(
    &mut self,
    app_id: &str,
    name: &str,
    mut on_change: OnChange,
    register: impl FnOnce(u64) -> P,
  ) -> ShortcutWatch {
    let watch = self.next;
    self.next += 1;
    let found = self
      .list
      .iter()
      .position(|shortcut| shortcut.app_id == app_id && shortcut.name == name);
    let index = found.unwrap_or_else(|| {
      self.list.push(Shortcut {
        id: watch,
        app_id: app_id.to_owned(),
        name: name.to_owned(),
        _object: register(watch),
        held: false,
        fresh: false,
        release: false,
        watches: Vec::new(),
      });
      self.list.len() - 1
    });
    let shortcut = &mut self.list[index];
    if shortcut.held {
      on_change(true);
    }
    shortcut.watches.push((watch, on_change));
    ShortcutWatch(watch)
  }

  /// Ends a watch, and the registration with its last one. One already
  /// ended is ignored.
  pub(crate) fn unwatch(&mut self, ShortcutWatch(watch): ShortcutWatch) {
    for shortcut in &mut self.list {
      shortcut.watches.retain(|(id, _)| *id != watch);
    }
    self.list.retain(|shortcut| !shortcut.watches.is_empty());
  }

  /// How many watches are on.
  pub(crate) fn watches(&self) -> usize {
    self
      .list
      .iter()
      .map(|shortcut| shortcut.watches.len())
      .sum()
  }

  /// The compositor's `pressed` for the shortcut whose events carry `id`.
  pub(crate) fn pressed(&mut self, id: u64) {
    let Some(shortcut) = self.list.iter_mut().find(|shortcut| shortcut.id == id) else {
      return;
    };
    shortcut.fresh = true;
    // shortcut: a press, release and press in one turn count once; count
    // presses if a config ever needs each.
    shortcut.release = false;
    if !std::mem::replace(&mut shortcut.held, true) {
      shortcut.tell(true);
    }
  }

  /// The compositor's `released`. One in the same turn as its press waits
  /// for [`flushed`](Self::flushed), so the press shows first.
  pub(crate) fn released(&mut self, id: u64) {
    let Some(shortcut) = self.list.iter_mut().find(|shortcut| shortcut.id == id) else {
      return;
    };
    if shortcut.fresh {
      shortcut.release = true;
    } else if std::mem::replace(&mut shortcut.held, false) {
      shortcut.tell(false);
    }
  }

  /// After a flush: tells the releases that waited for it. Whether any
  /// did, which needs another flush.
  pub(crate) fn flushed(&mut self) -> bool {
    let mut told = false;
    for shortcut in &mut self.list {
      shortcut.fresh = false;
      if std::mem::take(&mut shortcut.release) && std::mem::replace(&mut shortcut.held, false) {
        shortcut.tell(false);
        told = true;
      }
    }
    told
  }
}

/// Why the protocol can't carry a registration of these strings, if it
/// can't: its strings end at a NUL byte, and its messages at 4 KiB.
pub(crate) fn refusal(app_id: &str, name: &str, description: &str) -> Option<&'static str> {
  let strings = [app_id, name, description];
  if strings.iter().any(|string| string.contains('\0')) {
    Some("it holds a NUL byte")
  } else if strings.iter().map(|string| string.len()).sum::<usize>() > MOST_BYTES {
    Some("its app id, name and description pass 4000 bytes")
  } else {
    None
  }
}

impl Platform {
  /// Calls `on_change(true)` when the user presses the compositor's
  /// binding for `app_id`'s shortcut `name`, and `on_change(false)` when
  /// they release it; the compositor says when, so nothing wakes the
  /// shell in between. Watches of the same app id and name share one
  /// registration, whose description is the first watch's; a watch on a
  /// held shortcut hears `true` at once. A release that comes in the same
  /// loop turn as its press is told after that turn's flush, so the press
  /// shows. A panicking `on_change` is logged and the shell goes on.
  ///
  /// `None`, logged, if the compositor lacks Hyprland's
  /// `hyprland-global-shortcuts-v1`, which is logged once per shell, or
  /// if a string holds a NUL byte or the three pass 4000 bytes. Another
  /// program registering the same app id and name too makes Hyprland end
  /// the connection of whichever comes second.
  pub fn watch_shortcut(
    &mut self,
    app_id: &str,
    name: &str,
    description: &str,
    on_change: impl FnMut(bool) + 'static,
  ) -> Option<ShortcutWatch> {
    let global = &mut self.global_shortcuts;
    let Some(manager) = &global.manager else {
      if !std::mem::replace(&mut global.warned, true) {
        tracing::warn!(
          "the compositor lacks hyprland-global-shortcuts-v1, so global shortcuts are never pressed"
        );
      }
      return None;
    };
    if let Some(problem) = refusal(app_id, name, description) {
      tracing::warn!("the global shortcut {app_id:?}:{name:?} isn't registered: {problem}");
      return None;
    }
    let qh = &self.qh;
    Some(
      global
        .shortcuts
        .watch(app_id, name, Box::new(on_change), |id| {
          Registered(manager.register_shortcut(
            name.to_owned(),
            app_id.to_owned(),
            description.to_owned(),
            String::new(),
            qh,
            id,
          ))
        }),
    )
  }

  /// Ends a watch, and the registration with the last watch of its
  /// shortcut. One already ended is ignored.
  pub fn unwatch_shortcut(&mut self, watch: ShortcutWatch) {
    self.global_shortcuts.shortcuts.unwatch(watch);
  }

  /// How many global shortcut watches are on, as tests check they end.
  pub fn shortcut_watches(&self) -> usize {
    self.global_shortcuts.shortcuts.watches()
  }
}

impl Dispatch<HyprlandGlobalShortcutV1, u64> for Platform {
  fn event(
    platform: &mut Platform,
    _: &HyprlandGlobalShortcutV1,
    event: hyprland_global_shortcut_v1::Event,
    id: &u64,
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let shortcuts = &mut platform.global_shortcuts.shortcuts;
    match event {
      hyprland_global_shortcut_v1::Event::Pressed { .. } => shortcuts.pressed(*id),
      hyprland_global_shortcut_v1::Event::Released { .. } => shortcuts.released(*id),
    }
  }
}

impl Dispatch<HyprlandGlobalShortcutsManagerV1, ()> for Platform {
  fn event(
    _: &mut Platform,
    _: &HyprlandGlobalShortcutsManagerV1,
    _: hyprland_global_shortcuts_manager_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
  }
}
