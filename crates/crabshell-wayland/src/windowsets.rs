//! Workspaces through `ext-workspace-v1`, named as Quickshell's
//! WindowManager names them: windowsets (workspaces) in projections
//! (workspace groups) shown on outputs.

use std::panic::{self, AssertUnwindSafe};

use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::{
  Connection, Dispatch, QueueHandle, event_created_child,
};
use wayland_protocols::ext::workspace::v1::client::ext_workspace_group_handle_v1::{
  self, ExtWorkspaceGroupHandleV1, GroupCapabilities,
};
use wayland_protocols::ext::workspace::v1::client::ext_workspace_handle_v1::{
  self, ExtWorkspaceHandleV1, State as Flag, WorkspaceCapabilities as Can,
};
use wayland_protocols::ext::workspace::v1::client::ext_workspace_manager_v1::{
  self, ExtWorkspaceManagerV1,
};

use crate::foreign_toplevels::cut;
use crate::platform::Platform;

/// The most windowsets kept.
const MAX_WINDOWSETS: usize = 1024;

/// The most projections kept.
const MAX_PROJECTIONS: usize = 256;

/// The most coordinates a windowset may have.
const MAX_DIMENSIONS: usize = 16;

/// A workspace, as `ext-workspace-v1` lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Windowset {
  /// Names it while it exists, for the requests. Ids aren't reused, and
  /// never name a [`WindowsetProjection`] too.
  pub id: u64,
  /// The compositor's id for it, the same across sessions, if it gave one.
  pub stable_id: Option<String>,
  /// Its name, such as `1`, or `""`.
  pub name: String,
  /// Its place in its projection's grid, or empty if the compositor
  /// doesn't arrange them.
  pub coordinates: Vec<u32>,
  /// Whether it is shown.
  pub active: bool,
  /// Whether it asks for attention.
  pub urgent: bool,
  /// Whether the compositor asks that it not be shown in a list.
  pub hidden: bool,
  /// The id of the projection it is in.
  pub projection: Option<u64>,
  /// Whether the compositor takes `activate` for it.
  pub can_activate: bool,
  /// Whether the compositor takes `deactivate` for it.
  pub can_deactivate: bool,
  /// Whether the compositor takes `remove` for it.
  pub can_remove: bool,
  /// Whether the compositor takes `assign`, moving it to another
  /// projection.
  pub can_assign: bool,
}

/// A workspace group: windowsets shown on a set of outputs.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowsetProjection {
  /// Names it while it exists, for the requests.
  pub id: u64,
  /// The names of the outputs it is shown on, such as `DP-1`.
  pub outputs: Vec<String>,
  /// Whether the compositor takes `create_windowset` for it.
  pub can_create: bool,
}

/// The windowsets and projections, once a watch asked for them.
#[derive(Default)]
pub(crate) struct Windowsets {
  /// Whether a watch asked, so the manager is bound once at most.
  asked: bool,
  /// In the order the compositor announced them, with what has come
  /// since the last `done`.
  sets: Vec<Set>,
  groups: Vec<Group>,
  /// What the compositor said as of its last `done`.
  listed: (Vec<Windowset>, Vec<GroupState>),
  next: u64,
  /// Whether the lists changed since the watch last heard them.
  changed: bool,
  /// Whether input past a cap was logged.
  warned: bool,
  on_change: Option<Watch>,
}

/// What [`Platform::watch_windowsets`] calls.
type Watch = Box<dyn FnMut(&[Windowset], &[WindowsetProjection])>;

/// A windowset's handle, destroyed when dropped.
struct Set {
  proxy: ExtWorkspaceHandleV1,
  state: Windowset,
}

impl Drop for Set {
  fn drop(&mut self) {
    self.proxy.destroy();
  }
}

/// A projection's handle, destroyed when dropped.
struct Group {
  proxy: ExtWorkspaceGroupHandleV1,
  state: GroupState,
}

impl Drop for Group {
  fn drop(&mut self) {
    self.proxy.destroy();
  }
}

#[derive(Clone, PartialEq)]
struct GroupState {
  id: u64,
  outputs: Vec<WlOutput>,
  can_create: bool,
}

impl Windowsets {
  /// Notes that `output` came, went or got its name: the names listed
  /// change if a projection is on it.
  pub(crate) fn output_changed(&mut self, output: &WlOutput) {
    let listed = &self.listed.1;
    self.changed |= listed.iter().any(|group| group.outputs.contains(output));
  }

  /// Logs that the compositor sent more than fits, the first time only.
  fn warn_once(&mut self, what: std::fmt::Arguments) {
    if !std::mem::replace(&mut self.warned, true) {
      tracing::warn!("the compositor sent {what}; what doesn't fit is left out");
    }
  }
}

impl Platform {
  /// Calls `on_change` with every windowset and projection, in the order
  /// the compositor announced them: after the next dispatch, then after
  /// each dispatch that changed one. It replaces any watch set before. If
  /// it panics, the panic is logged and the watch kept.
  ///
  /// The first watch binds `ext_workspace_manager_v1`, so a shell that
  /// never asks hears nothing of workspaces. Without it, the lists stay
  /// empty, and that is logged.
  pub fn watch_windowsets(
    &mut self,
    on_change: impl FnMut(&[Windowset], &[WindowsetProjection]) + 'static,
  ) {
    self.windowsets.on_change = Some(Box::new(on_change));
    self.windowsets.changed = true;
    if std::mem::replace(&mut self.windowsets.asked, true) {
      return;
    }
    // The handles it announces carry everything from here on.
    let bound = self
      .registry
      .bind_one::<ExtWorkspaceManagerV1, _, _>(&self.qh, 1..=1, ());
    if let Err(error) = bound {
      tracing::warn!(
        %error,
        "the compositor lacks ext-workspace, so no workspaces are listed"
      );
    }
  }

  /// Tells the watch the lists, if they changed. Before the flush, so the
  /// effects it sets off run in this iteration.
  pub(crate) fn report_windowsets(&mut self) {
    if !std::mem::take(&mut self.windowsets.changed) {
      return;
    }
    let name = |output: &WlOutput| {
      let known = self.known.iter().find(|(known, _)| known == output);
      known.map(|(_, info)| info.name.clone())
    };
    let (sets, groups) = &self.windowsets.listed;
    let projections: Vec<WindowsetProjection> = groups
      .iter()
      .map(|group| WindowsetProjection {
        id: group.id,
        // An output not announced yet is left out until it is.
        outputs: group.outputs.iter().filter_map(name).collect(),
        can_create: group.can_create,
      })
      .collect();
    if let Some(on_change) = &mut self.windowsets.on_change
      && let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| on_change(sets, &projections)))
    {
      let message = crate::events::panic_message(payload.as_ref());
      tracing::error!("a windowset watch panicked: {message}");
    }
  }
}

/// The coordinates in `bytes`, native-endian `u32`s, or `None` past
/// [`MAX_DIMENSIONS`].
pub(crate) fn coordinates(bytes: &[u8]) -> Option<Vec<u32>> {
  let (chunks, _) = bytes.as_chunks::<4>();
  let fits = chunks.len() <= MAX_DIMENSIONS;
  fits.then(|| {
    chunks
      .iter()
      .map(|chunk| u32::from_ne_bytes(*chunk))
      .collect()
  })
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    _: &ExtWorkspaceManagerV1,
    event: ext_workspace_manager_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    use ext_workspace_manager_v1::Event;
    let ours = &mut platform.windowsets;
    match event {
      Event::WorkspaceGroup { workspace_group } => {
        if ours.groups.len() >= MAX_PROJECTIONS {
          ours.warn_once(format_args!("more than {MAX_PROJECTIONS} workspace groups"));
          return workspace_group.destroy();
        }
        let state = GroupState {
          id: ours.next,
          outputs: Vec::new(),
          can_create: false,
        };
        ours.next += 1;
        ours.groups.push(Group {
          proxy: workspace_group,
          state,
        });
      }
      Event::Workspace { workspace } => {
        if ours.sets.len() >= MAX_WINDOWSETS {
          ours.warn_once(format_args!("more than {MAX_WINDOWSETS} workspaces"));
          return workspace.destroy();
        }
        let state = Windowset {
          id: ours.next,
          ..Windowset::default()
        };
        ours.next += 1;
        ours.sets.push(Set {
          proxy: workspace,
          state,
        });
      }
      Event::Done => {
        let now = (
          ours.sets.iter().map(|set| set.state.clone()).collect(),
          ours
            .groups
            .iter()
            .map(|group| group.state.clone())
            .collect(),
        );
        if ours.listed != now {
          ours.listed = now;
          ours.changed = true;
        }
      }
      Event::Finished => {
        tracing::info!("the compositor stopped listing workspaces");
        // Dropping the handles destroys them.
        *ours = Windowsets {
          asked: true,
          changed: ours.changed || ours.listed != Default::default(),
          on_change: ours.on_change.take(),
          ..Windowsets::default()
        };
      }
      _ => {}
    }
  }

  event_created_child!(Platform, ExtWorkspaceManagerV1, [
    ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
    ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
  ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    proxy: &ExtWorkspaceGroupHandleV1,
    event: ext_workspace_group_handle_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    use ext_workspace_group_handle_v1::Event;
    let ours = &mut platform.windowsets;
    // A group past the cap was destroyed, and isn't here.
    let Some(index) = ours.groups.iter().position(|group| group.proxy == *proxy) else {
      return;
    };
    let id = ours.groups[index].state.id;
    let group = &mut ours.groups[index].state;
    match event {
      Event::Capabilities { capabilities } => {
        let can = GroupCapabilities::from_bits_truncate(capabilities.into());
        group.can_create = can.contains(GroupCapabilities::CreateWorkspace);
      }
      Event::OutputEnter { output } if !group.outputs.contains(&output) => {
        group.outputs.push(output);
      }
      Event::OutputLeave { output } => group.outputs.retain(|ours| *ours != output),
      Event::WorkspaceEnter { workspace } => {
        if let Some(set) = ours.sets.iter_mut().find(|set| set.proxy == workspace) {
          set.state.projection = Some(id);
        }
      }
      Event::WorkspaceLeave { workspace } => {
        if let Some(set) = ours.sets.iter_mut().find(|set| set.proxy == workspace)
          && set.state.projection == Some(id)
        {
          set.state.projection = None;
        }
      }
      Event::Removed => {
        ours.groups.remove(index);
        // The protocol has its windowsets leave first; in case they didn't.
        for set in &mut ours.sets {
          if set.state.projection == Some(id) {
            set.state.projection = None;
          }
        }
      }
      _ => {}
    }
  }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    proxy: &ExtWorkspaceHandleV1,
    event: ext_workspace_handle_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    use ext_workspace_handle_v1::Event;
    let ours = &mut platform.windowsets;
    // A windowset past the cap was destroyed, and isn't here.
    let Some(index) = ours.sets.iter().position(|set| set.proxy == *proxy) else {
      return;
    };
    let set = &mut ours.sets[index].state;
    match event {
      Event::Id { id } => set.stable_id = Some(cut(id)),
      Event::Name { name } => set.name = cut(name),
      Event::Coordinates { coordinates: bytes } => match coordinates(&bytes) {
        Some(coordinates) => set.coordinates = coordinates,
        None => ours.warn_once(format_args!(
          "a workspace with more than {MAX_DIMENSIONS} coordinates"
        )),
      },
      Event::State { state } => {
        let state = Flag::from_bits_truncate(state.into());
        set.active = state.contains(Flag::Active);
        set.urgent = state.contains(Flag::Urgent);
        set.hidden = state.contains(Flag::Hidden);
      }
      Event::Capabilities { capabilities } => {
        let can = Can::from_bits_truncate(capabilities.into());
        set.can_activate = can.contains(Can::Activate);
        set.can_deactivate = can.contains(Can::Deactivate);
        set.can_remove = can.contains(Can::Remove);
        set.can_assign = can.contains(Can::Assign);
      }
      Event::Removed => drop(ours.sets.remove(index)),
      _ => {}
    }
  }
}
