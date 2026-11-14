//! Every program's windows, through
//! `wlr-foreign-toplevel-management-unstable-v1`: their titles, app ids,
//! outputs and states, and requests to activate, close, maximize,
//! minimize and fullscreen them.

use std::panic::{self, AssertUnwindSafe};

use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::{
  Connection, Dispatch, Proxy, QueueHandle, event_created_child,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::{
  self, ZwlrForeignToplevelHandleV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::{
  self, ZwlrForeignToplevelManagerV1,
};

use crate::platform::Platform;

/// The longest title or app id kept, in bytes.
const MAX_TEXT: usize = 4096;

/// The most toplevels kept.
const MAX_TOPLEVELS: usize = 1024;

/// A window the compositor shows, from any program, the shell's own
/// floating windows included.
#[derive(Clone, Debug, PartialEq)]
pub struct Toplevel {
  /// Names the window while it is open. Ids aren't reused.
  pub id: u64,
  /// Its title, or `""`.
  pub title: String,
  /// Its app id, such as `org.gnome.Nautilus`, or `""`.
  pub app_id: String,
  /// The names of the outputs it shows on, such as `DP-1`.
  pub outputs: Vec<String>,
  /// Whether it has the focus.
  pub activated: bool,
  /// Whether it is maximized.
  pub maximized: bool,
  /// Whether it is minimized.
  pub minimized: bool,
  /// Whether it is fullscreen.
  pub fullscreen: bool,
  /// The id of the window it belongs to, such as a dialog's main window.
  pub parent: Option<u64>,
}

/// The toplevels, once a watch asked for them.
#[derive(Default)]
pub(crate) struct Foreign {
  /// Whether a watch asked, so the manager is bound once at most.
  asked: bool,
  /// In the order the compositor announced them.
  handles: Vec<Handle>,
  next: u64,
  /// Whether the list changed since the watch last heard it.
  changed: bool,
  /// Whether toplevels past the cap were logged.
  warned: bool,
  on_change: Option<Watch>,
}

/// What [`Platform::watch_toplevels`] calls.
type Watch = Box<dyn FnMut(&[Toplevel])>;

impl Foreign {
  fn handle(&self, id: u64) -> Option<&ZwlrForeignToplevelHandleV1> {
    let handle = self.handles.iter().find(|handle| handle.id == id)?;
    Some(&handle.proxy)
  }

  /// Each listed toplevel's id, app id and title, in the order the
  /// compositor announced them.
  pub(crate) fn names(&self) -> impl Iterator<Item = (u64, (&str, &str))> {
    self.handles.iter().filter_map(|handle| {
      let state = handle.current.as_ref()?;
      Some((handle.id, (state.app_id.as_str(), state.title.as_str())))
    })
  }

  /// Notes that `output` came, went or got its name: the names listed
  /// change if a toplevel is on it.
  pub(crate) fn output_changed(&mut self, output: &WlOutput) {
    self.changed |= self.handles.iter().any(|handle| {
      handle
        .current
        .as_ref()
        .is_some_and(|state| state.outputs.contains(output))
    });
  }
}

/// A toplevel's handle, destroyed when dropped.
struct Handle {
  proxy: ZwlrForeignToplevelHandleV1,
  id: u64,
  /// What has come since the last `done`.
  pending: State,
  /// As of the last `done`; `None` before the first, when it isn't listed.
  current: Option<State>,
}

impl Drop for Handle {
  fn drop(&mut self) {
    self.proxy.destroy();
  }
}

#[derive(Clone, Default, PartialEq)]
struct State {
  title: String,
  app_id: String,
  outputs: Vec<WlOutput>,
  activated: bool,
  maximized: bool,
  minimized: bool,
  fullscreen: bool,
  parent: Option<u64>,
}

impl Platform {
  /// Calls `on_change` with every toplevel, in the order the compositor
  /// announced them: after the next dispatch, then after each dispatch
  /// that changed one. It replaces any watch set before. If it panics, the
  /// panic is logged and the watch kept.
  ///
  /// The first watch binds `zwlr_foreign_toplevel_manager_v1`, so a shell
  /// that never asks hears nothing of other windows. Without it, the list
  /// stays empty, and that is logged.
  pub fn watch_toplevels(&mut self, on_change: impl FnMut(&[Toplevel]) + 'static) {
    self.foreign.on_change = Some(Box::new(on_change));
    self.foreign.changed = true;
    if std::mem::replace(&mut self.foreign.asked, true) {
      return;
    }
    // The handles it announces carry everything from here on.
    let bound = self
      .registry
      .bind_one::<ZwlrForeignToplevelManagerV1, _, _>(&self.qh, 1..=3, ());
    if let Err(error) = bound {
      tracing::warn!(
        %error,
        "the compositor lacks wlr-foreign-toplevel-management, so no windows are listed"
      );
    }
  }

  /// Asks the compositor to focus toplevel `id` on the first seat. An
  /// unknown id is ignored.
  pub fn activate_toplevel(&mut self, id: u64) {
    let (Some(handle), Some(seat)) = (self.foreign.handle(id), self.seats.seats().next()) else {
      return;
    };
    handle.activate(&seat);
  }

  /// Asks toplevel `id` to close. An unknown id is ignored.
  pub fn close_toplevel(&mut self, id: u64) {
    if let Some(handle) = self.foreign.handle(id) {
      handle.close();
    }
  }

  /// Asks the compositor to maximize toplevel `id`, or to stop. An
  /// unknown id is ignored.
  pub fn set_toplevel_maximized(&mut self, id: u64, maximized: bool) {
    match self.foreign.handle(id) {
      Some(handle) if maximized => handle.set_maximized(),
      Some(handle) => handle.unset_maximized(),
      None => {}
    }
  }

  /// Asks the compositor to minimize toplevel `id`, or to stop. An
  /// unknown id is ignored.
  pub fn set_toplevel_minimized(&mut self, id: u64, minimized: bool) {
    match self.foreign.handle(id) {
      Some(handle) if minimized => handle.set_minimized(),
      Some(handle) => handle.unset_minimized(),
      None => {}
    }
  }

  /// Asks the compositor to make toplevel `id` fullscreen on an output
  /// of its choosing, or to stop. An unknown id is ignored, and so is a
  /// compositor with version 1 of the protocol, which can't.
  pub fn set_toplevel_fullscreen(&mut self, id: u64, fullscreen: bool) {
    match self
      .foreign
      .handle(id)
      .filter(|handle| handle.version() >= 2)
    {
      Some(handle) if fullscreen => handle.set_fullscreen(None),
      Some(handle) => handle.unset_fullscreen(),
      None => {}
    }
  }

  /// Tells the watch the list, if it changed. Before the flush, so the
  /// effects it sets off run in this iteration.
  pub(crate) fn report_toplevels(&mut self) {
    if !std::mem::take(&mut self.foreign.changed) {
      return;
    }
    let name = |output: &WlOutput| {
      let known = self.known.iter().find(|(known, _)| known == output);
      known.map(|(_, info)| info.name.clone())
    };
    let list: Vec<Toplevel> = self
      .foreign
      .handles
      .iter()
      .filter_map(|handle| {
        let state = handle.current.as_ref()?;
        Some(Toplevel {
          id: handle.id,
          title: state.title.clone(),
          app_id: state.app_id.clone(),
          // An output not announced yet is left out until it is.
          outputs: state.outputs.iter().filter_map(name).collect(),
          activated: state.activated,
          maximized: state.maximized,
          minimized: state.minimized,
          fullscreen: state.fullscreen,
          parent: state.parent,
        })
      })
      .collect();
    if let Some(on_change) = &mut self.foreign.on_change
      && let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| on_change(&list)))
    {
      let message = crate::events::panic_message(payload.as_ref());
      tracing::error!("a toplevel watch panicked: {message}");
    }
  }
}

/// `text` cut to [`MAX_TEXT`] bytes, at a character's start.
pub(crate) fn cut(mut text: String) -> String {
  text.truncate(text.floor_char_boundary(MAX_TEXT));
  text
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    _: &ZwlrForeignToplevelManagerV1,
    event: zwlr_foreign_toplevel_manager_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let foreign = &mut platform.foreign;
    match event {
      zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
        if foreign.handles.len() >= MAX_TOPLEVELS {
          if !std::mem::replace(&mut foreign.warned, true) {
            tracing::warn!("more than {MAX_TOPLEVELS} windows; the rest aren't listed");
          }
          toplevel.destroy();
          return;
        }
        foreign.handles.push(Handle {
          proxy: toplevel,
          id: foreign.next,
          pending: State::default(),
          current: None,
        });
        foreign.next += 1;
      }
      zwlr_foreign_toplevel_manager_v1::Event::Finished => {
        tracing::info!("the compositor stopped listing windows");
        foreign.changed |= !foreign.handles.is_empty();
        foreign.handles.clear();
      }
      _ => {}
    }
  }

  event_created_child!(Platform, ZwlrForeignToplevelManagerV1, [
    zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
  ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    proxy: &ZwlrForeignToplevelHandleV1,
    event: zwlr_foreign_toplevel_handle_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    use zwlr_foreign_toplevel_handle_v1::{Event, State as Flag};
    let foreign = &mut platform.foreign;
    // A handle past the cap was destroyed, and isn't here.
    let Some(index) = foreign.handles.iter().position(|ours| ours.proxy == *proxy) else {
      return;
    };
    // The parent is a handle announced before.
    let parent = match &event {
      Event::Parent { parent } => parent.as_ref().and_then(|parent| {
        let found = foreign.handles.iter().find(|ours| ours.proxy == *parent);
        found.map(|ours| ours.id)
      }),
      _ => None,
    };
    let handle = &mut foreign.handles[index];
    let pending = &mut handle.pending;
    match event {
      Event::Title { title } => pending.title = cut(title),
      Event::AppId { app_id } => pending.app_id = cut(app_id),
      Event::OutputEnter { output } if !pending.outputs.contains(&output) => {
        pending.outputs.push(output);
      }
      Event::OutputLeave { output } => pending.outputs.retain(|ours| *ours != output),
      Event::State { state } => {
        let flags: Vec<u32> = state
          .as_chunks::<4>()
          .0
          .iter()
          .map(|bytes| u32::from_ne_bytes(*bytes))
          .collect();
        let has = |flag: Flag| flags.contains(&(flag as u32));
        pending.activated = has(Flag::Activated);
        pending.maximized = has(Flag::Maximized);
        pending.minimized = has(Flag::Minimized);
        pending.fullscreen = has(Flag::Fullscreen);
      }
      Event::Parent { .. } => pending.parent = parent,
      Event::Done if handle.current.as_ref() != Some(&handle.pending) => {
        handle.current = Some(handle.pending.clone());
        foreign.changed = true;
      }
      Event::Closed => {
        foreign.changed |= handle.current.is_some();
        let closed = foreign.handles.remove(index).id;
        // The protocol doesn't promise to tell a child its parent went;
        // wlroots does, others may not.
        for other in &mut foreign.handles {
          if other.pending.parent == Some(closed) {
            other.pending.parent = None;
          }
          if let Some(current) = other
            .current
            .as_mut()
            .filter(|current| current.parent == Some(closed))
          {
            current.parent = None;
            foreign.changed = true;
          }
        }
      }
      _ => {}
    }
  }
}
