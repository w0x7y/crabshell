//! Hyprland's workspaces, monitors and windows, over its two IPC sockets
//! in `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`:
//! `.socket.sock` takes one request per connection, such as
//! `j/workspaces`, answers and closes, and `.socket2.sock` sends events
//! as lines of `NAME>>DATA`.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::Once;

use calloop::{Interest, RegistrationToken};
use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::Shared;
use crate::dbus::set_changed;

/// One of Hyprland's workspaces, as `j/workspaces` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HyprlandWorkspace {
  /// Its number. Named and special workspaces have none from Hyprland
  /// 0.57 on, and a number below zero before it.
  pub id: Option<i64>,
  /// Its name, such as `1`, `web` or `special:magic`: the key to give
  /// `children_keyed`.
  pub name: String,
  /// The name of the monitor it is on, such as `DP-1`.
  pub monitor: String,
  /// How many windows it holds.
  pub windows: u32,
  /// Whether it is shown on the focused monitor.
  pub focused: bool,
  /// Whether it is shown on its monitor, focused or not.
  pub active: bool,
}

/// One of Hyprland's monitors, as `j/monitors` describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct HyprlandMonitor {
  /// Hyprland's number for it.
  pub id: i64,
  /// Its name, such as `DP-1`, as the output's name.
  pub name: String,
  /// Its make, model and serial, as Hyprland puts them.
  pub description: String,
  /// Whether it has the focus.
  pub focused: bool,
  /// The name of the workspace it shows.
  pub active_workspace: String,
  /// Where its left edge is, in the layout's logical pixels.
  pub x: i32,
  /// Where its top edge is, in the layout's logical pixels.
  pub y: i32,
  /// Its mode's width, in pixels before scaling.
  pub width: i32,
  /// Its mode's height, in pixels before scaling.
  pub height: i32,
  /// Its scale, such as 1.5.
  pub scale: f64,
}

/// One of Hyprland's windows, as `j/clients` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HyprlandToplevel {
  /// Its address, such as `0x55d4c8a3b0c0`: the key to give
  /// `children_keyed`, and what a dispatcher names it by, as
  /// `address:0x55d4c8a3b0c0`.
  pub address: String,
  /// Its class, the app id.
  pub class: String,
  /// Its title.
  pub title: String,
  /// The name of the workspace it is on.
  pub workspace: String,
  /// The name of the monitor it is on, or `""` while Hyprland doesn't
  /// list that monitor.
  pub monitor: String,
  /// Whether it floats.
  pub floating: bool,
  /// Whether it is fullscreen; a maximized window isn't.
  pub fullscreen: bool,
  /// Whether it has the keyboard focus.
  pub focused: bool,
}

/// The Hyprland service, from `cx.hyprland()`. A clone is cheap and
/// shares the one connection.
#[derive(Clone)]
pub struct Hyprland {
  workspaces: ReadSignal<Vec<HyprlandWorkspace>>,
  monitors: ReadSignal<Vec<HyprlandMonitor>>,
  toplevels: ReadSignal<Vec<HyprlandToplevel>>,
}

impl Hyprland {
  /// Every workspace, numbered ones by number after the named ones.
  /// Empty while Hyprland isn't connected.
  pub fn workspaces(&self) -> ReadSignal<Vec<HyprlandWorkspace>> {
    self.workspaces
  }

  /// Every monitor, in Hyprland's order.
  pub fn monitors(&self) -> ReadSignal<Vec<HyprlandMonitor>> {
    self.monitors
  }

  /// Every mapped window, in Hyprland's order.
  pub fn toplevels(&self) -> ReadSignal<Vec<HyprlandToplevel>> {
    self.toplevels
  }
}

/// `cx.hyprland()`.
pub(crate) fn hyprland(cx: &Cx) -> Hyprland {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.hyprland() needs the services the shell provides; it shows nothing");
    });
    return Hyprland {
      workspaces: cx.signal(Vec::new()).read_only(),
      monitors: cx.signal(Vec::new()).read_only(),
      toplevels: cx.signal(Vec::new()).read_only(),
    };
  };
  let (workspaces, monitors, toplevels) = service.start();
  Hyprland {
    workspaces: workspaces.read_only(),
    monitors: monitors.read_only(),
    toplevels: toplevels.read_only(),
  }
}

/// A list read whole with one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Query {
  Workspaces,
  Monitors,
  Clients,
  ActiveWindow,
}

impl Query {
  const ALL: [Query; 4] = [
    Query::Workspaces,
    Query::Monitors,
    Query::Clients,
    Query::ActiveWindow,
  ];

  fn command(self) -> &'static str {
    match self {
      Query::Workspaces => "j/workspaces",
      Query::Monitors => "j/monitors",
      Query::Clients => "j/clients",
      Query::ActiveWindow => "j/activewindow",
    }
  }
}

type Lists = (
  Signal<Vec<HyprlandWorkspace>>,
  Signal<Vec<HyprlandMonitor>>,
  Signal<Vec<HyprlandToplevel>>,
);

/// The one Hyprland connection of a shell, started on first use.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
}

struct State {
  workspaces: Signal<Vec<HyprlandWorkspace>>,
  monitors: Signal<Vec<HyprlandMonitor>>,
  toplevels: Signal<Vec<HyprlandToplevel>>,
  /// Requests waiting for their reply.
  requests: Vec<Request>,
  next_request: u64,
  /// Whether a request that couldn't connect was logged, so a missing
  /// request socket logs once.
  request_failure_logged: bool,
  raw: Raw,
}

/// What Hyprland last said, the lists are built from.
#[derive(Default)]
struct Raw {
  workspaces: Vec<RawWorkspace>,
  monitors: Vec<RawMonitor>,
  windows: Vec<RawWindow>,
  /// The focused window's address.
  active: Option<String>,
}

struct Request {
  id: u64,
  query: Query,
  stream: UnixStream,
  reader: RegistrationToken,
  reply: Vec<u8>,
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      state: RefCell::new(None),
    }
  }

  /// The lists, reading them from Hyprland if this is the first ask.
  fn start(self: &Rc<Self>) -> Lists {
    if self.state.borrow().is_none() {
      let root = &self.shared.root;
      *self.state.borrow_mut() = Some(State {
        workspaces: root.signal(Vec::new()),
        monitors: root.signal(Vec::new()),
        toplevels: root.signal(Vec::new()),
        requests: Vec::new(),
        next_request: 0,
        request_failure_logged: false,
        raw: Raw::default(),
      });
      if self.shared.config.hyprland.is_none() {
        tracing::info!(
          "HYPRLAND_INSTANCE_SIGNATURE or XDG_RUNTIME_DIR is unset, so there is no Hyprland to show"
        );
      } else {
        let mut state = self.state.borrow_mut();
        let state = state.as_mut().expect("invariant: started above");
        for query in Query::ALL {
          self.request(state, query);
        }
      }
    }
    let state = self.state.borrow();
    let state = state.as_ref().expect("invariant: started above");
    (state.workspaces, state.monitors, state.toplevels)
  }

  /// Sends `query`'s request on a connection of its own. `false`, logged
  /// once, if Hyprland can't be reached.
  fn request(self: &Rc<Self>, state: &mut State, query: Query) -> bool {
    let (Some(dir), Some(event_loop)) = (&self.shared.config.hyprland, self.shared.event_loop())
    else {
      return false;
    };
    let path = dir.join(".socket.sock");
    let command = query.command().as_bytes();
    let opened = UnixStream::connect(&path).and_then(|mut stream| {
      stream.set_nonblocking(true)?;
      // One write: Hyprland takes what has arrived as the whole request.
      if stream.write(command)? < command.len() {
        return Err(std::io::Error::other("the request didn't fit the socket"));
      }
      let fd = stream.as_fd().try_clone_to_owned()?;
      Ok((stream, fd))
    });
    let (stream, fd) = match opened {
      Ok(opened) => opened,
      Err(error) => {
        if !std::mem::replace(&mut state.request_failure_logged, true) {
          let path = path.display();
          tracing::warn!(%path, %error, "can't send Hyprland a request");
        }
        return false;
      }
    };
    state.request_failure_logged = false;
    let id = state.next_request;
    state.next_request += 1;
    let service = Rc::clone(self);
    let read = Box::new(move || service.on_reply(id));
    let Some(reader) = event_loop.insert_fd(fd, Interest::READ, read) else {
      return false;
    };
    state.requests.push(Request {
      id,
      query,
      stream,
      reader,
      reply: Vec::new(),
    });
    true
  }

  /// Reads what came of a reply, and acts on it once Hyprland closed the
  /// connection.
  fn on_reply(self: &Rc<Self>, id: u64) {
    let mut chunk = [0; 64 * 1024];
    let mut state = self.state.borrow_mut();
    let state = state.as_mut().expect("invariant: started before reading");
    let Some(at) = state.requests.iter().position(|request| request.id == id) else {
      return;
    };
    let request = &mut state.requests[at];
    let failed = match request.stream.read(&mut chunk) {
      Ok(0) => None,
      Ok(read) => return request.reply.extend_from_slice(&chunk[..read]),
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
        return;
      }
      Err(error) => Some(error.to_string()),
    };
    let request = state.requests.swap_remove(at);
    if let Some(event_loop) = self.shared.event_loop() {
      event_loop.remove(request.reader);
    }
    let query = request.query;
    match failed {
      Some(why) => {
        tracing::warn!(
          command = query.command(),
          why,
          "Hyprland's reply was dropped"
        );
      }
      None => state.raw.apply(query, &request.reply),
    }
    state.show();
  }
}

impl State {
  /// Builds the lists from what Hyprland last said, and sets each that
  /// changed.
  fn show(&self) {
    let raw = &self.raw;
    let focused = raw.monitors.iter().find(|monitor| monitor.focused);
    let mut workspaces: Vec<_> = raw
      .workspaces
      .iter()
      .map(|workspace| HyprlandWorkspace {
        id: workspace.at.id,
        name: workspace.at.name.clone(),
        monitor: workspace.monitor.clone(),
        windows: workspace.windows,
        focused: focused.is_some_and(|monitor| monitor.active.is(&workspace.at)),
        active: raw
          .monitors
          .iter()
          .any(|monitor| monitor.name == workspace.monitor && monitor.active.is(&workspace.at)),
      })
      .collect();
    // Named ones first, as Hyprland before 0.57 numbers them below zero.
    workspaces.sort_by_key(|workspace| workspace.id.unwrap_or(i64::MIN));
    let monitors = raw.monitors.iter().map(|monitor| HyprlandMonitor {
      id: monitor.id,
      name: monitor.name.clone(),
      description: monitor.description.clone(),
      focused: monitor.focused,
      active_workspace: monitor.active.name.clone(),
      x: monitor.x,
      y: monitor.y,
      width: monitor.width,
      height: monitor.height,
      scale: monitor.scale,
    });
    let toplevels = raw.windows.iter().map(|window| HyprlandToplevel {
      address: window.address.clone(),
      class: window.class.clone(),
      title: window.title.clone(),
      workspace: window.workspace.name.clone(),
      monitor: raw
        .monitors
        .iter()
        .find(|monitor| monitor.id == window.monitor)
        .map_or_else(String::new, |monitor| monitor.name.clone()),
      floating: window.floating,
      fullscreen: window.fullscreen.is_fullscreen(),
      focused: raw.active.as_ref() == Some(&window.address),
    });
    set_changed(self.workspaces, workspaces);
    set_changed(self.monitors, monitors.collect());
    set_changed(self.toplevels, toplevels.collect());
  }
}

impl Raw {
  /// Keeps what `reply` says for `query`. A reply that doesn't parse is
  /// logged and the last one kept.
  fn apply(&mut self, query: Query, reply: &[u8]) {
    match query {
      Query::Workspaces => {
        if let Some(list) = list("workspace", reply) {
          self.workspaces = list;
        }
      }
      Query::Monitors => {
        if let Some(list) = list("monitor", reply) {
          self.monitors = list;
        }
      }
      Query::Clients => {
        if let Some(list) = list("window", reply) {
          self.windows = list;
        }
      }
      Query::ActiveWindow => match serde_json::from_slice::<ActiveWindow>(reply) {
        Ok(active) => self.active = active.address,
        Err(error) => tracing::warn!(%error, "a bad focused window from Hyprland"),
      },
    }
  }
}

/// A JSON list of `what`s. `None`, logged, for a reply that isn't one.
fn list<T: DeserializeOwned>(what: &str, reply: &[u8]) -> Option<Vec<T>> {
  serde_json::from_slice(reply)
    .inspect_err(|error| tracing::warn!(%error, "a bad {what} list from Hyprland"))
    .ok()
}

/// A workspace as a monitor or a window names it: by number while it has
/// one, and from Hyprland 0.57 on by address otherwise.
#[derive(Deserialize)]
struct WorkspaceRef {
  id: Option<i64>,
  address: Option<String>,
  name: String,
}

impl WorkspaceRef {
  fn is(&self, other: &WorkspaceRef) -> bool {
    match (self.id, other.id) {
      (Some(id), Some(other)) => id == other,
      _ => self.address.is_some() && self.address == other.address,
    }
  }
}

#[derive(Deserialize)]
struct RawWorkspace {
  #[serde(flatten)]
  at: WorkspaceRef,
  monitor: String,
  windows: u32,
}

#[derive(Deserialize)]
struct RawMonitor {
  id: i64,
  name: String,
  description: String,
  focused: bool,
  x: i32,
  y: i32,
  width: i32,
  height: i32,
  scale: f64,
  #[serde(rename = "activeWorkspace")]
  active: WorkspaceRef,
}

#[derive(Deserialize)]
struct RawWindow {
  address: String,
  class: String,
  title: String,
  workspace: WorkspaceRef,
  monitor: i64,
  floating: bool,
  fullscreen: Fullscreen,
}

/// A window's fullscreen state: a flag in older Hyprland, then its mode,
/// where 1 is maximized and 2 fullscreen.
#[derive(Deserialize)]
#[serde(untagged)]
enum Fullscreen {
  Flag(bool),
  Mode(i64),
}

impl Fullscreen {
  fn is_fullscreen(&self) -> bool {
    match *self {
      Fullscreen::Flag(flag) => flag,
      Fullscreen::Mode(mode) => mode & 2 != 0,
    }
  }
}

/// `j/activewindow`'s reply: the focused window, or `{}` for none.
#[derive(Deserialize)]
struct ActiveWindow {
  address: Option<String>,
}
