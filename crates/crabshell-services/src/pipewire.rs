//! PipeWire's graph, its nodes and the links between them, over its
//! native protocol on the socket `pipewire-0`. The shell says hello,
//! asks for the registry, which lists every object, and binds each link
//! for its state. libpulse still carries volume, sinks and meters.

mod pod;
#[cfg(test)]
mod tests;

use std::cell::{RefCell, RefMut};
use std::collections::{BTreeMap, HashMap};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::rc::Rc;
use std::sync::Once;

use calloop::{Interest, RegistrationToken};
use crabshell_reactive::{Memo, Owner, ReadSignal, Signal};
use crabshell_ui::Cx;

use crate::Shared;
use crate::dbus::set_changed;
use crate::hyprland::{cut, open};
use pod::{Bad, Builder, Entry, Fields, HEADER, MAX_MESSAGE};

/// The most nodes and links kept together; the rest are left out.
pub(crate) const MAX_OBJECTS: usize = 4096;

/// The ids the protocol fixes: the core, this client, and the registry,
/// which the shell names when it asks for it.
const CORE: u32 = 0;
const CLIENT: u32 = 1;
const REGISTRY: u32 = 2;

/// The versions spoken: the protocol's, and the registry's and links'
/// interfaces'.
const PROTOCOL_VERSION: i32 = 3;
const REGISTRY_VERSION: i32 = 3;
const LINK_VERSION: u32 = 3;

const NODE_TYPE: &[u8] = b"PipeWire:Interface:Node";
const LINK_TYPE: &[u8] = b"PipeWire:Interface:Link";

/// One of PipeWire's nodes: a device's sink or source, or a program's
/// stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PwNode {
  /// PipeWire's id for it, which the links name: the key to give
  /// `children_keyed`. A new node may reuse a gone one's id.
  pub id: u32,
  /// Its `node.name`, such as `alsa_output.pci-0000_00_1f.3.analog-stereo`.
  pub name: String,
  /// Its `node.description`, else its `node.nick`, else its name.
  pub description: String,
  /// Its `media.class`, such as `Audio/Sink` or `Stream/Input/Video`.
  pub media_class: String,
  /// Whether it is a program's stream: its class starts with `Stream/`.
  pub is_stream: bool,
}

/// Where a link is in being set up, as PipeWire says. The order is from
/// worst to best.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PwLinkState {
  /// It failed.
  Error,
  /// Its ports were unlinked.
  Unlinked,
  /// It was just made.
  Init,
  /// Its ports are agreeing on a format.
  Negotiating,
  /// Its buffers are being made.
  Allocating,
  /// It is ready but nothing flows.
  Paused,
  /// Data flows through it.
  Active,
}

/// A link from one node's output port to another's input port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PwLink {
  /// PipeWire's id for it.
  pub id: u32,
  /// The node it goes from.
  pub output_node: u32,
  /// The port of `output_node` it goes from.
  pub output_port: u32,
  /// The node it goes to.
  pub input_node: u32,
  /// The port of `input_node` it goes to.
  pub input_port: u32,
  /// Where it is in being set up.
  pub state: PwLinkState,
}

/// The links from one node to another, one per channel, as one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PwLinkGroup {
  /// The node they go from.
  pub output_node: u32,
  /// The node they go to.
  pub input_node: u32,
  /// The best state among the links.
  pub state: PwLinkState,
}

/// The PipeWire service, from `cx.pipewire()`. A clone is cheap and
/// shares the one connection.
#[derive(Clone)]
pub struct PipeWire {
  /// The asking context's owner, which [`node_links`](Self::node_links)'s
  /// memos go with.
  owner: Owner,
  nodes: ReadSignal<Vec<PwNode>>,
  links: ReadSignal<Vec<PwLink>>,
  link_groups: ReadSignal<Vec<PwLinkGroup>>,
  screencasts: ReadSignal<Vec<PwNode>>,
}

impl PipeWire {
  /// Every node, by id. Empty while PipeWire isn't connected.
  pub fn nodes(&self) -> ReadSignal<Vec<PwNode>> {
    self.nodes
  }

  /// Every link whose state PipeWire has said, by id.
  pub fn links(&self) -> ReadSignal<Vec<PwLink>> {
    self.links
  }

  /// The links folded per output node and input node, by those ids.
  pub fn link_groups(&self) -> ReadSignal<Vec<PwLinkGroup>> {
    self.link_groups
  }

  /// The link groups to or from the node `node`, as a memo of the
  /// context `cx.pipewire()` was called on. Called on a handle whose
  /// context went, it panics, as `cx.memo` does.
  pub fn node_links(&self, node: u32) -> Memo<Vec<PwLinkGroup>> {
    let groups = self.link_groups;
    self.owner.memo(move || {
      groups.with(|groups| {
        let touches = |group: &&PwLinkGroup| group.output_node == node || group.input_node == node;
        groups.iter().filter(touches).copied().collect()
      })
    })
  }

  /// The video sources a program made, such as a screen-share portal's,
  /// that another node captures: a `Video/Source` node with no
  /// `device.id`, so not a camera, linked to at least one node.
  pub fn screencasts(&self) -> ReadSignal<Vec<PwNode>> {
    self.screencasts
  }
}

/// `cx.pipewire()`.
pub(crate) fn pipewire(cx: &Cx) -> PipeWire {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.pipewire() needs the services the shell provides; it shows nothing");
    });
    return PipeWire {
      owner: cx.owner(),
      nodes: cx.signal(Vec::new()).read_only(),
      links: cx.signal(Vec::new()).read_only(),
      link_groups: cx.signal(Vec::new()).read_only(),
      screencasts: cx.signal(Vec::new()).read_only(),
    };
  };
  let signals = service.start();
  PipeWire {
    owner: cx.owner(),
    nodes: signals.nodes.read_only(),
    links: signals.links.read_only(),
    link_groups: signals.link_groups.read_only(),
    screencasts: signals.screencasts.read_only(),
  }
}

#[derive(Clone, Copy)]
struct Signals {
  nodes: Signal<Vec<PwNode>>,
  links: Signal<Vec<PwLink>>,
  link_groups: Signal<Vec<PwLinkGroup>>,
  screencasts: Signal<Vec<PwNode>>,
}

/// The one PipeWire connection of a shell, started on first use.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
}

struct State {
  signals: Signals,
  connection: Option<Connection>,
  /// Whether the shell connected again since PipeWire last sent anything
  /// past the graph it gives a new client.
  reconnected: bool,
}

struct Connection {
  stream: UnixStream,
  reader: RegistrationToken,
  /// Waits for room in the socket while it holds back part of `outbox`.
  writer: Option<RegistrationToken>,
  /// What was read past the last whole message.
  inbox: Vec<u8>,
  outbox: Vec<u8>,
  /// The next message's sequence number.
  seq: u32,
  /// The next proxy id never used, and those PipeWire gave back. PipeWire
  /// takes a new id only if it is one of those or the next.
  next_id: u32,
  free_ids: Vec<u32>,
  /// Each bound link's proxy id, to its global id.
  proxies: HashMap<u32, u32>,
  graph: Graph,
  opening: Opening,
  /// Whether the cap on objects was logged.
  full: bool,
}

/// How far the graph PipeWire gives a new client has come.
#[derive(PartialEq, Eq)]
enum Opening {
  /// Waiting for the answer to the sync numbered this. `bound` says a link
  /// was bound since it was sent, whose info would come after it.
  Syncing { seq: i32, bound: bool },
  /// All of it came, so it shows.
  Open,
}

#[derive(Default)]
struct Graph {
  nodes: BTreeMap<u32, Node>,
  /// Every link the registry listed, by global id, with what its info
  /// said once it came.
  links: BTreeMap<u32, Option<PwLink>>,
}

struct Node {
  node: PwNode,
  /// Whether a device made it, as a camera's or a sound card's.
  device: bool,
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      state: RefCell::new(None),
    }
  }

  /// The signals, connecting to PipeWire if this is the first ask.
  fn start(self: &Rc<Self>) -> Signals {
    if self.state.borrow().is_none() {
      let root = &self.shared.root;
      let signals = Signals {
        nodes: root.signal(Vec::new()),
        links: root.signal(Vec::new()),
        link_groups: root.signal(Vec::new()),
        screencasts: root.signal(Vec::new()),
      };
      *self.state.borrow_mut() = Some(State {
        signals,
        connection: None,
        reconnected: false,
      });
      match &self.shared.config.pipewire {
        None => tracing::info!("no runtime directory, so there is no PipeWire to show"),
        Some(path) => {
          self.connect(path);
        }
      }
    }
    let state = self.state.borrow();
    state.as_ref().expect("invariant: started above").signals
  }

  /// Connects and asks for the graph. A failure is logged and never
  /// retried, so a stale socket never wakes the shell. `false` if it
  /// failed.
  fn connect(self: &Rc<Self>, path: &Path) -> bool {
    let Some(event_loop) = self.shared.event_loop() else {
      return false;
    };
    let opened = open(path).and_then(|stream| {
      let fd = stream.as_fd().try_clone_to_owned()?;
      Ok((stream, fd))
    });
    let (stream, fd) = match opened {
      Ok(opened) => opened,
      Err(error) => {
        let path = path.display();
        tracing::warn!(%path, %error, "can't connect to PipeWire, so its graph stays empty");
        return false;
      }
    };
    let service = Rc::clone(self);
    let Some(reader) =
      event_loop.insert_fd(fd, Interest::READ, Box::new(move || service.on_readable()))
    else {
      return false;
    };
    let mut connection = Connection {
      stream,
      reader,
      writer: None,
      inbox: Vec::new(),
      outbox: Vec::new(),
      seq: 0,
      next_id: REGISTRY + 1,
      free_ids: Vec::new(),
      proxies: HashMap::new(),
      graph: Graph::default(),
      opening: Opening::Open,
      full: false,
    };
    connection.send(CORE, 1, Builder::default().int(PROTOCOL_VERSION));
    let name = Builder::default()
      .int(1)
      .string("application.name")
      .string("crabshell");
    connection.send(CLIENT, 2, Builder::default().structure(name));
    let registry = Builder::default().int(REGISTRY_VERSION).uint(REGISTRY);
    connection.send(CORE, 5, registry);
    connection.sync(false);
    let mut state = self.state.borrow_mut();
    let state = state
      .as_mut()
      .expect("invariant: started before connecting");
    state.connection = Some(connection);
    self.flush_with(state);
    true
  }

  /// Whether the signals are still there: their owner, the root, isn't
  /// disposed.
  fn alive(&self) -> bool {
    let state = self.state.borrow();
    let alive = state
      .as_ref()
      .and_then(|state| state.signals.nodes.try_with(|_| ()));
    alive.is_some()
  }

  /// Reads what came, takes in each whole message, answers what needs an
  /// answer, and shows the graph once all of it has come.
  fn on_readable(self: &Rc<Self>) {
    if !self.alive() {
      return self.close();
    }
    let mut chunk = [0; 64 * 1024];
    let mut guard = self.state.borrow_mut();
    let state = guard
      .as_mut()
      .expect("invariant: started before connecting");
    let Some(connection) = state.connection.as_mut() else {
      return;
    };
    match connection.stream.read(&mut chunk) {
      Ok(0) => return self.closed(guard, "PipeWire closed the connection"),
      Ok(read) => connection.inbox.extend_from_slice(&chunk[..read]),
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
        return;
      }
      Err(error) => return self.closed(guard, &error.to_string()),
    }
    let inbox = std::mem::take(&mut connection.inbox);
    let mut at = 0;
    while let Some(header) = pod::header(&inbox[at..]) {
      if header.size > MAX_MESSAGE {
        // Reconnecting would only read it again.
        return self.lost(state, "PipeWire sent a message over 1 MiB");
      }
      let Some(body) = inbox.get(at + HEADER..at + HEADER + header.size) else {
        break;
      };
      at += HEADER + header.size;
      if connection.opening == Opening::Open {
        // Past the graph, so a later close is worth a reconnect.
        state.reconnected = false;
      }
      if let Err(Bad(why)) = connection.take(header.id, header.opcode, body) {
        let (id, opcode) = (header.id, header.opcode);
        tracing::warn!(id, opcode, why, "a bad message from PipeWire");
      }
    }
    connection.inbox = inbox[at..].to_vec();
    if connection.opening == Opening::Open {
      connection.graph.show(state.signals);
    }
    self.flush_with(state);
  }

  /// Writes what the socket takes, and waits for room for the rest.
  fn flush(self: &Rc<Self>) {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      self.flush_with(state);
    }
  }

  fn flush_with(self: &Rc<Self>, state: &mut State) {
    let (Some(connection), Some(event_loop)) =
      (state.connection.as_mut(), self.shared.event_loop())
    else {
      return;
    };
    while !connection.outbox.is_empty() {
      match connection.stream.write(&connection.outbox) {
        Ok(written) => {
          connection.outbox.drain(..written);
        }
        Err(error) if error.kind() == ErrorKind::Interrupted => {}
        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
        Err(error) => {
          // PipeWire went; reading sees it end, and decides what follows.
          tracing::debug!(%error, "can't write to PipeWire");
          connection.outbox.clear();
        }
      }
    }
    if connection.outbox.len() > MAX_MESSAGE {
      return self.lost(state, "PipeWire reads nothing the shell sends");
    }
    if connection.outbox.is_empty() {
      if let Some(writer) = connection.writer.take() {
        event_loop.remove(writer);
      }
    } else if connection.writer.is_none() {
      let fd = match connection.stream.as_fd().try_clone_to_owned() {
        Ok(fd) => fd,
        Err(error) => return self.lost(state, &error.to_string()),
      };
      let service = Rc::clone(self);
      let write = Box::new(move || service.flush());
      connection.writer = event_loop.insert_fd(fd, Interest::WRITE, write);
    }
  }

  /// The connection ended for `why`. While the socket is there, this
  /// connects once again and keeps the graph shown until the new one has
  /// come. A connection that ends again before PipeWire sent anything
  /// past its graph is lost.
  fn closed(self: &Rc<Self>, mut guard: RefMut<'_, Option<State>>, why: &str) {
    let state = guard
      .as_mut()
      .expect("invariant: started before connecting");
    let path = self.shared.config.pipewire.as_deref();
    let Some(path) = path.filter(|path| !state.reconnected && path.exists()) else {
      return self.lost(state, why);
    };
    tracing::warn!(why, "PipeWire's connection ended; connecting again");
    state.reconnected = true;
    self.drop_connection(state);
    drop(guard);
    if !self.connect(path)
      && let Some(state) = self.state.borrow_mut().as_mut()
    {
      self.lost(state, "PipeWire's socket can't be reached again");
    }
  }

  /// Drops the connection and empties the graph.
  fn lost(&self, state: &mut State, why: &str) {
    tracing::warn!(why, "lost PipeWire; its graph is empty");
    self.drop_connection(state);
    Graph::default().show(state.signals);
  }

  /// Closes the connection for good, once the signals are gone.
  fn close(&self) {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      self.drop_connection(state);
    }
  }

  fn drop_connection(&self, state: &mut State) {
    if let (Some(connection), Some(event_loop)) =
      (state.connection.take(), self.shared.event_loop())
    {
      event_loop.remove(connection.reader);
      if let Some(writer) = connection.writer {
        event_loop.remove(writer);
      }
    }
  }
}

impl Connection {
  fn send(&mut self, id: u32, opcode: u8, fields: Builder) {
    pod::message(&mut self.outbox, id, opcode, self.seq, &fields.finish());
    self.seq = self.seq.wrapping_add(1);
  }

  /// Asks the core to answer once everything sent before has been
  /// handled.
  fn sync(&mut self, bound: bool) {
    let seq = self.seq.cast_signed();
    self.send(CORE, 2, Builder::default().uint(CORE).int(seq));
    self.opening = Opening::Syncing { seq, bound };
  }

  /// Takes in one message for the object `id`.
  fn take(&mut self, id: u32, opcode: u8, body: &[u8]) -> Result<(), Bad> {
    let mut fields = Fields::of(body)?;
    match (id, opcode) {
      // Done, the answer to a sync.
      (CORE, 1) => {
        let (_, seq) = (fields.uint()?, fields.int()?);
        if let Opening::Syncing { seq: sent, bound } = self.opening
          && seq == sent
        {
          match bound {
            // Their infos come before the answer to a sync sent now.
            true => self.sync(false),
            false => self.opening = Opening::Open,
          }
        }
      }
      // Ping, answered with a pong.
      (CORE, 2) => {
        let (object, seq) = (fields.uint()?, fields.int()?);
        self.send(CORE, 3, Builder::default().uint(object).int(seq));
      }
      (CORE, 3) => {
        let (object, _, res) = (fields.uint()?, fields.int()?, fields.int()?);
        let message = text(fields.text()?.unwrap_or_default());
        tracing::warn!(object, res, message, "PipeWire sent an error");
      }
      // RemoveId: PipeWire let go of a proxy, so its id may be reused.
      (CORE, 4) => {
        let object = fields.uint()?;
        if object > REGISTRY && object < self.next_id && !self.free_ids.contains(&object) {
          self.proxies.remove(&object);
          self.free_ids.push(object);
        }
      }
      // Global.
      (REGISTRY, 0) => {
        let (global, _permissions) = (fields.uint()?, fields.int()?);
        let kind = fields.text()?.unwrap_or_default();
        let version = fields.uint()?;
        let props = fields.dict()?;
        self.global(global, kind, version, &props);
      }
      // GlobalRemove.
      (REGISTRY, 1) => {
        let global = fields.uint()?;
        self.graph.nodes.remove(&global);
        if self.graph.links.remove(&global).is_some() {
          self.proxies.retain(|_, linked| *linked != global);
        }
      }
      // A bound link's info.
      (proxy, 0) if self.proxies.contains_key(&proxy) => {
        let global = self.proxies[&proxy];
        let link = PwLink {
          id: fields.uint()?,
          output_node: fields.uint()?,
          output_port: fields.uint()?,
          input_node: fields.uint()?,
          input_port: fields.uint()?,
          state: {
            let _change_mask = fields.long()?;
            state(fields.int()?)?
          },
        };
        if link.id != global {
          return Err(Bad("a link's info for another link"));
        }
        if let Some(slot) = self.graph.links.get_mut(&global) {
          *slot = Some(link);
        }
      }
      // The core's and this client's infos, and what else comes.
      _ => {}
    }
    Ok(())
  }

  /// Takes in a global the registry announced: a node is kept, a link
  /// bound for its info, and anything else skipped.
  fn global(&mut self, global: u32, kind: &[u8], version: u32, props: &[Entry<'_>]) {
    if kind != NODE_TYPE && kind != LINK_TYPE {
      return;
    }
    let known = self.graph.nodes.contains_key(&global) || self.graph.links.contains_key(&global);
    if !known && self.graph.nodes.len() + self.graph.links.len() >= MAX_OBJECTS {
      if !std::mem::replace(&mut self.full, true) {
        tracing::warn!("PipeWire has more nodes and links than the {MAX_OBJECTS} kept");
      }
      return;
    }
    let prop = |key: &[u8]| {
      let found = props.iter().find(|(name, _)| *name == key);
      found.map(|(_, value)| text(value))
    };
    if kind == NODE_TYPE {
      let name = prop(b"node.name").unwrap_or_default();
      let description = prop(b"node.description")
        .or_else(|| prop(b"node.nick"))
        .unwrap_or_else(|| name.clone());
      let media_class = prop(b"media.class").unwrap_or_default();
      let node = PwNode {
        id: global,
        name,
        description,
        is_stream: media_class.starts_with("Stream/"),
        media_class,
      };
      let device = prop(b"device.id").is_some();
      self.graph.nodes.insert(global, Node { node, device });
    } else if !known {
      let proxy = self.free_ids.pop().unwrap_or_else(|| {
        self.next_id += 1;
        self.next_id - 1
      });
      let bind = Builder::default()
        .uint(global)
        .string("PipeWire:Interface:Link")
        .uint(version.min(LINK_VERSION))
        .uint(proxy);
      self.send(REGISTRY, 1, bind);
      self.proxies.insert(proxy, global);
      self.graph.links.insert(global, None);
      if let Opening::Syncing { bound, .. } = &mut self.opening {
        *bound = true;
      }
    }
  }
}

impl Graph {
  /// Sets each signal whose value changed.
  fn show(&self, signals: Signals) {
    let nodes = self.nodes.values().map(|node| node.node.clone()).collect();
    set_changed(signals.nodes, nodes);
    let links: Vec<PwLink> = self.links.values().flatten().copied().collect();
    let mut groups: BTreeMap<(u32, u32), PwLinkState> = BTreeMap::new();
    for link in &links {
      let best = groups
        .entry((link.output_node, link.input_node))
        .or_insert(link.state);
      *best = (*best).max(link.state);
    }
    let groups: Vec<PwLinkGroup> = groups
      .into_iter()
      .map(|((output_node, input_node), state)| PwLinkGroup {
        output_node,
        input_node,
        state,
      })
      .collect();
    let captured = |node: &&Node| {
      node.node.media_class == "Video/Source"
        && !node.device
        && groups.iter().any(|group| group.output_node == node.node.id)
    };
    let screencasts = self.nodes.values().filter(captured);
    set_changed(
      signals.screencasts,
      screencasts.map(|node| node.node.clone()).collect(),
    );
    set_changed(signals.links, links);
    set_changed(signals.link_groups, groups);
  }
}

/// A link's state as PipeWire numbers it.
fn state(raw: i32) -> Result<PwLinkState, Bad> {
  Ok(match raw {
    -2 => PwLinkState::Error,
    -1 => PwLinkState::Unlinked,
    0 => PwLinkState::Init,
    1 => PwLinkState::Negotiating,
    2 => PwLinkState::Allocating,
    3 => PwLinkState::Paused,
    4 => PwLinkState::Active,
    _ => return Err(Bad("a link state PipeWire doesn't have")),
  })
}

/// A text from PipeWire, cut to 4 KiB.
fn text(bytes: &[u8]) -> String {
  cut(String::from_utf8_lossy(bytes).into_owned())
}
