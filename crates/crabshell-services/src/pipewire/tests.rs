use std::collections::{BTreeMap, HashMap};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;

use super::pod::{self, Bad, Builder, Fields, HEADER, MAX_MESSAGE, Pod};
use super::{MAX_OBJECTS, PipeWire, PwLink, PwLinkGroup, PwLinkState, PwNode};
use crate::hyprland::MAX_TEXT;
use crate::testing::{Harness, capture_logs, offline};
use crate::{Config, ServicesExt};

mod real;

/// A POD of `kind` holding `body`, padded to 8, as PipeWire writes it.
fn raw(kind: u32, body: &[u8]) -> Vec<u8> {
  let mut pod = (body.len() as u32).to_ne_bytes().to_vec();
  pod.extend_from_slice(&kind.to_ne_bytes());
  pod.extend_from_slice(body);
  pod.resize(pod.len().next_multiple_of(8), 0);
  pod
}

fn string(text: &str) -> Vec<u8> {
  raw(8, &[text.as_bytes(), b"\0"].concat())
}

fn int(value: i32) -> Vec<u8> {
  raw(4, &value.to_ne_bytes())
}

fn long(value: i64) -> Vec<u8> {
  raw(5, &value.to_ne_bytes())
}

fn structure(fields: &[Vec<u8>]) -> Vec<u8> {
  raw(14, &fields.concat())
}

fn dict(entries: &[(&str, &str)]) -> Vec<u8> {
  let mut fields = vec![int(entries.len() as i32)];
  for (key, value) in entries {
    fields.extend([string(key), string(value)]);
  }
  structure(&fields)
}

#[test]
fn each_pod_type_reads_back() {
  let cases: Vec<(Vec<u8>, Pod<'static>)> = vec![
    (raw(1, &[]), Pod::None),
    (raw(2, &1u32.to_ne_bytes()), Pod::Bool(true)),
    (raw(3, &7u32.to_ne_bytes()), Pod::Id(7)),
    (int(-2), Pod::Int(-2)),
    (long(1 << 40), Pod::Long(1 << 40)),
    (string("sink"), Pod::String(b"sink")),
    (raw(15, &[0; 16]), Pod::Other(15)),
  ];
  for (bytes, expected) in cases {
    let (pod, taken) = pod::parse(&bytes).expect("a POD");
    assert_eq!(pod, expected);
    assert_eq!(taken, bytes.len(), "with its padding");
  }
  let message = structure(&[int(5), string("a"), dict(&[("k", "v")]), long(3)]);
  let mut fields = Fields::of(&message).expect("a struct");
  assert_eq!(fields.uint(), Ok(5));
  assert_eq!(fields.text(), Ok(Some(&b"a"[..])));
  assert_eq!(fields.dict(), Ok(vec![(&b"k"[..], &b"v"[..])]));
  assert_eq!(fields.long(), Ok(3));
  assert_eq!(fields.int(), Err(Bad("a struct with a field missing")));
}

#[test]
fn a_cut_or_overlong_pod_is_refused() {
  let whole = string("node.name");
  assert_eq!(
    pod::parse(&whole[..6]),
    Err(Bad("a POD cut short in its header"))
  );
  assert_eq!(
    pod::parse(&whole[..12]),
    Err(Bad("a POD longer than its message"))
  );
  // A size far past the bytes there, as a hostile server could send.
  let mut oversize = int(1);
  oversize[..4].copy_from_slice(&u32::MAX.to_ne_bytes());
  assert_eq!(
    pod::parse(&oversize),
    Err(Bad("a POD longer than its message"))
  );
  assert_eq!(
    pod::parse(&raw(4, &[1, 0])),
    Err(Bad("a POD too short for its type"))
  );
  assert_eq!(
    pod::parse(&raw(8, b"no nul")),
    Err(Bad("a string with no closing NUL"))
  );
  assert_eq!(
    Fields::of(&int(1)),
    Err(Bad("a message that isn't a struct"))
  );
  let message = structure(&[string("x")]);
  let mut fields = Fields::of(&message).expect("a struct");
  assert_eq!(fields.int(), Err(Bad("a field that isn't an Int")));
  // A dictionary that counts more entries than it holds.
  let short = structure(&[structure(&[int(2), string("k"), string("v")])]);
  let mut fields = Fields::of(&short).expect("a struct");
  assert_eq!(fields.dict(), Err(Bad("a struct with a field missing")));
  let mistyped = structure(&[structure(&[int(1), string("k"), int(5)])]);
  let mut fields = Fields::of(&mistyped).expect("a struct");
  assert_eq!(fields.dict(), Err(Bad("a field that isn't a String")));
}

#[test]
fn what_the_shell_builds_is_what_pipewire_writes() {
  let built = Builder::default()
    .int(3)
    .uint(u32::MAX)
    .string("PipeWire:Interface:Link")
    .structure(Builder::default().int(0))
    .finish();
  let expected = structure(&[
    int(3),
    int(-1),
    string("PipeWire:Interface:Link"),
    structure(&[int(0)]),
  ]);
  assert_eq!(built, expected);
  let mut message = Vec::new();
  pod::message(&mut message, 2, 1, 9, &built);
  let header = pod::header(&message).expect("a header");
  assert_eq!(
    header,
    pod::Header {
      id: 2,
      opcode: 1,
      size: built.len()
    }
  );
  assert_eq!(&message[HEADER..], &built[..]);
  assert_eq!(pod::header(&message[..15]), None, "until all 16 bytes came");
}

/// A stand-in for PipeWire: its socket in a short temporary directory,
/// since socket paths are capped at 108 bytes. The directory goes on drop.
struct FakePipeWire {
  dir: PathBuf,
  listener: UnixListener,
}

impl FakePipeWire {
  fn new() -> FakePipeWire {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "crabshell-pw-fake-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let listener = UnixListener::bind(dir.join("pipewire-0")).expect("a socket");
    listener
      .set_nonblocking(true)
      .expect("a non-blocking socket");
    FakePipeWire { dir, listener }
  }

  fn config(&self) -> Config {
    Config {
      pipewire: Some(self.dir.join("pipewire-0")),
      ..offline()
    }
  }

  /// The next connection the shell made.
  fn accept(&self) -> Option<Peer> {
    match self.listener.accept() {
      Ok((stream, _)) => {
        stream.set_nonblocking(true).expect("a non-blocking stream");
        Some(Peer::new(stream))
      }
      Err(error) if error.kind() == ErrorKind::WouldBlock => None,
      Err(error) => panic!("accept failed: {error}"),
    }
  }
}

impl Drop for FakePipeWire {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

/// The server's end of one connection, answering as PipeWire does: the
/// registry with every global, a sync with its done, and a bind with the
/// link's info.
struct Peer {
  stream: UnixStream,
  inbox: Vec<u8>,
  outbox: Vec<u8>,
  seq: u32,
  /// Each global, as its registry event's body, by id.
  globals: BTreeMap<u32, Vec<u8>>,
  /// Each link's info, by global id.
  links: HashMap<u32, (u32, u32, u32, u32, i32)>,
  /// The shell's proxies, to the globals bound.
  bound: HashMap<u32, u32>,
  /// Whether the shell asked for the registry.
  registry: bool,
  /// Every request, as object id and opcode, and every bind's new id.
  requests: Vec<(u32, u8)>,
  binds: Vec<u32>,
  pongs: Vec<i32>,
  /// Whether to read nothing, as a stuck server.
  deaf: bool,
  /// Whether to answer binds with nothing, so only the shell's own
  /// writes are left for it to wait on.
  mute: bool,
}

impl Peer {
  fn new(stream: UnixStream) -> Peer {
    Peer {
      stream,
      inbox: Vec::new(),
      outbox: Vec::new(),
      seq: 0,
      globals: BTreeMap::new(),
      links: HashMap::new(),
      bound: HashMap::new(),
      registry: false,
      requests: Vec::new(),
      binds: Vec::new(),
      pongs: Vec::new(),
      deaf: false,
      mute: false,
    }
  }

  /// Queues a message, and writes what the socket takes: the shell runs
  /// on this thread, so a full socket waits for the next pump.
  fn send(&mut self, id: u32, opcode: u8, body: &[u8]) {
    pod::message(&mut self.outbox, id, opcode, self.seq, body);
    self.seq += 1;
    self.flush();
  }

  fn flush(&mut self) {
    while !self.outbox.is_empty() {
      match self.stream.write(&self.outbox) {
        Ok(written) => drop(self.outbox.drain(..written)),
        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
        Err(error) => panic!("write failed: {error}"),
      }
    }
  }

  /// Reads and answers what the shell sent.
  fn pump(&mut self) {
    self.flush();
    if self.deaf {
      return;
    }
    let mut chunk = [0; 64 * 1024];
    loop {
      match self.stream.read(&mut chunk) {
        Ok(0) => break,
        Ok(read) => self.inbox.extend_from_slice(&chunk[..read]),
        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
        Err(error) => panic!("read failed: {error}"),
      }
    }
    while let Some(header) = pod::header(&self.inbox) {
      let Some(body) = self.inbox.get(HEADER..HEADER + header.size) else {
        break;
      };
      let body = body.to_vec();
      self.inbox.drain(..HEADER + header.size);
      self.answer(header.id, header.opcode, &body);
    }
    self.flush();
  }

  fn answer(&mut self, id: u32, opcode: u8, body: &[u8]) {
    self.requests.push((id, opcode));
    let mut fields = Fields::of(body).expect("a struct from the shell");
    match (id, opcode) {
      (0, 5) => {
        let (_version, new_id) = (fields.int().unwrap(), fields.uint().unwrap());
        assert_eq!(new_id, 2, "the registry's id");
        self.registry = true;
        for global in self.globals.values().cloned().collect::<Vec<_>>() {
          self.send(2, 0, &global);
        }
      }
      (0, 2) => {
        let (core, seq) = (fields.uint().unwrap(), fields.int().unwrap());
        self.send(0, 1, &structure(&[int(core as i32), int(seq)]));
      }
      (0, 3) => {
        fields.uint().unwrap();
        self.pongs.push(fields.int().unwrap());
      }
      (2, 1) => {
        let global = fields.uint().unwrap();
        assert_eq!(
          fields.text().unwrap(),
          Some(&b"PipeWire:Interface:Link"[..])
        );
        let _version = fields.uint().unwrap();
        let proxy = fields.uint().unwrap();
        self.binds.push(proxy);
        self.bound.insert(proxy, global);
        if !self.mute {
          self.info(proxy);
        }
      }
      _ => {}
    }
  }

  /// Sends the info of the link bound as `proxy`.
  fn info(&mut self, proxy: u32) {
    let global = self.bound[&proxy];
    let (output_node, output_port, input_node, input_port, state) = self.links[&global];
    let body = structure(&[
      int(global as i32),
      int(output_node as i32),
      int(output_port as i32),
      int(input_node as i32),
      int(input_port as i32),
      long(7),
      int(state),
      raw(1, &[]),
      raw(1, &[]),
      dict(&[]),
    ]);
    self.send(proxy, 0, &body);
  }

  fn global(&mut self, id: u32, kind: &str, props: &[(&str, &str)]) {
    let body = structure(&[
      int(id as i32),
      int(0o777),
      string(kind),
      int(3),
      dict(props),
    ]);
    if self.registry {
      self.send(2, 0, &body);
    }
    self.globals.insert(id, body);
  }

  fn node(&mut self, id: u32, props: &[(&str, &str)]) {
    self.global(id, "PipeWire:Interface:Node", props);
  }

  fn link(&mut self, id: u32, output: (u32, u32), input: (u32, u32), state: i32) {
    self
      .links
      .insert(id, (output.0, output.1, input.0, input.1, state));
    self.global(id, "PipeWire:Interface:Link", &[]);
  }

  /// Changes a bound link's state, as PipeWire tells its proxies.
  fn set_state(&mut self, id: u32, state: i32) {
    self.links.get_mut(&id).expect("a link").4 = state;
    let proxies: Vec<u32> = (self.bound.iter())
      .filter(|(_, global)| **global == id)
      .map(|(proxy, _)| *proxy)
      .collect();
    for proxy in proxies {
      self.info(proxy);
    }
  }

  /// Removes a global, and lets go of the proxies bound to it.
  fn remove(&mut self, id: u32) {
    self.globals.remove(&id);
    self.links.remove(&id);
    self.send(2, 1, &structure(&[int(id as i32)]));
    let proxies: Vec<u32> = (self.bound.iter())
      .filter(|(_, global)| **global == id)
      .map(|(proxy, _)| *proxy)
      .collect();
    for proxy in proxies {
      self.bound.remove(&proxy);
      self.send(0, 4, &structure(&[int(proxy as i32)]));
    }
  }
}

/// A sink, a player playing to it on two channels, and a recorder.
fn graph(peer: &mut Peer) {
  peer.node(
    30,
    &[
      ("node.name", "speakers"),
      ("node.description", "Speakers"),
      ("media.class", "Audio/Sink"),
      ("device.id", "40"),
    ],
  );
  peer.node(
    31,
    &[
      ("node.name", "mpv"),
      ("node.nick", "Player"),
      ("media.class", "Stream/Output/Audio"),
    ],
  );
  peer.node(
    32,
    &[("node.name", "obs"), ("media.class", "Stream/Input/Audio")],
  );
  peer.link(50, (31, 1), (30, 3), 4);
  peer.link(51, (31, 2), (30, 4), 1);
  peer.link(52, (30, 5), (32, 6), 3);
}

fn node(id: u32, name: &str, description: &str, class: &str) -> PwNode {
  PwNode {
    id,
    name: name.to_owned(),
    description: description.to_owned(),
    media_class: class.to_owned(),
    is_stream: class.starts_with("Stream/"),
  }
}

fn link(id: u32, output: (u32, u32), input: (u32, u32), state: PwLinkState) -> PwLink {
  PwLink {
    id,
    output_node: output.0,
    output_port: output.1,
    input_node: input.0,
    input_port: input.1,
    state,
  }
}

fn group(output_node: u32, input_node: u32, state: PwLinkState) -> PwLinkGroup {
  PwLinkGroup {
    output_node,
    input_node,
    state,
  }
}

/// Runs the shell and the fake in turn for `time`.
fn run_for(shell: &mut Harness, peer: &mut Peer, time: Duration) {
  let end = Instant::now() + time;
  while Instant::now() < end {
    peer.pump();
    shell.run_until(Instant::now() + Duration::from_millis(10));
  }
  peer.pump();
}

fn run(shell: &mut Harness, peer: &mut Peer) {
  run_for(shell, peer, Duration::from_millis(100));
}

/// A shell connected to `fake`, which has the graph `build` makes, all of
/// it shown.
fn connect(fake: &FakePipeWire, build: impl Fn(&mut Peer)) -> (Harness, PipeWire, Peer) {
  let mut shell = Harness::new(fake.config());
  let pipewire = shell.cx.pipewire();
  let mut peer = fake.accept().expect("the shell connected");
  build(&mut peer);
  let opened = |peer: &Peer| peer.requests.len() >= 4;
  let give_up = Instant::now() + Duration::from_secs(5);
  run(&mut shell, &mut peer);
  while !opened(&peer) || !peer.inbox.is_empty() || !peer.outbox.is_empty() {
    assert!(Instant::now() < give_up, "no graph after 5 s");
    run(&mut shell, &mut peer);
  }
  (shell, pipewire, peer)
}

#[test]
fn the_shell_says_hello_and_names_itself_before_asking_for_the_registry() {
  let fake = FakePipeWire::new();
  let (_shell, _pipewire, peer) = connect(&fake, |_| {});
  assert_eq!(&peer.requests[..4], &[(0, 1), (1, 2), (0, 5), (0, 2)]);
}

#[test]
fn the_graph_shows_once_every_link_has_said_its_state() {
  let fake = FakePipeWire::new();
  let mut shell = Harness::new(fake.config());
  let pipewire = shell.cx.pipewire();
  let mut peer = fake.accept().expect("the shell connected");
  graph(&mut peer);
  // Answer the registry and the first sync, but not yet the binds.
  shell.run_until(Instant::now() + Duration::from_millis(20));
  peer.pump();
  shell.run_until(Instant::now() + Duration::from_millis(20));
  assert!(
    pipewire.nodes().get_untracked().is_empty(),
    "nothing before the links' states"
  );
  run(&mut shell, &mut peer);
  assert_eq!(
    pipewire.nodes().get_untracked(),
    vec![
      node(30, "speakers", "Speakers", "Audio/Sink"),
      node(31, "mpv", "Player", "Stream/Output/Audio"),
      node(32, "obs", "obs", "Stream/Input/Audio"),
    ]
  );
  assert_eq!(
    pipewire.links().get_untracked(),
    vec![
      link(50, (31, 1), (30, 3), PwLinkState::Active),
      link(51, (31, 2), (30, 4), PwLinkState::Negotiating),
      link(52, (30, 5), (32, 6), PwLinkState::Paused),
    ]
  );
  assert_eq!(
    pipewire.link_groups().get_untracked(),
    vec![
      group(30, 32, PwLinkState::Paused),
      group(31, 30, PwLinkState::Active)
    ],
    "the best state of each pair's links"
  );
  assert_eq!(
    peer.binds,
    vec![3, 4, 5],
    "each link bound, with the next ids"
  );
}

#[test]
fn node_links_follows_the_groups_to_and_from_one_node() {
  let fake = FakePipeWire::new();
  let (mut shell, pipewire, mut peer) = connect(&fake, graph);
  let runtime = Runtime::current();
  let before = runtime.node_count();
  let scope = shell.cx.child();
  let player = scope.pipewire().node_links(31);
  let sink = scope.pipewire().node_links(30);
  assert_eq!(player.get(), vec![group(31, 30, PwLinkState::Active)]);
  assert_eq!(sink.get().len(), 2);
  peer.remove(50);
  peer.remove(51);
  run(&mut shell, &mut peer);
  assert_eq!(player.get(), vec![]);
  assert_eq!(sink.get(), vec![group(30, 32, PwLinkState::Paused)]);
  assert_eq!(pipewire.links().get_untracked().len(), 1);
  scope.owner().dispose();
  assert_eq!(
    runtime.node_count(),
    before,
    "the memos went with their context"
  );
}

#[test]
fn one_tracker_follows_a_changing_node() {
  let fake = FakePipeWire::new();
  let (shell, _pipewire, _peer) = connect(&fake, graph);
  let runtime = Runtime::current();
  let before = runtime.node_count();
  let row = shell.cx.child();
  let node = row.signal(31);
  let links = row.pipewire().node_links(move || node.get());
  assert_eq!(links.get(), vec![group(31, 30, PwLinkState::Active)]);
  node.set(32);
  assert_eq!(links.get(), vec![group(30, 32, PwLinkState::Paused)]);
  row.owner().dispose();
  assert_eq!(
    runtime.node_count(),
    before,
    "the tracker went with its row"
  );
}

#[test]
fn a_panicking_node_closure_is_logged_with_where_and_the_rest_runs() {
  let fake = FakePipeWire::new();
  let (shell, pipewire, _peer) = connect(&fake, graph);
  let broken = shell.cx.signal(true);
  let mut line = 0;
  let logs = capture_logs(|| {
    line = line!() + 1;
    let links = pipewire.node_links(move || {
      assert!(!broken.get(), "no node yet");
      31
    });
    assert_eq!(links.get(), vec![]);
    let other = pipewire.node_links(30);
    assert_eq!(other.get().len(), 2, "the other tracker still runs");
    broken.set(false);
    assert_eq!(links.get(), vec![group(31, 30, PwLinkState::Active)]);
  });
  let at = format!("cx.pipewire().node_links (called at {}:{line}", file!());
  assert!(logs.contains(&at), "{logs}");
  assert!(
    logs.contains("panicked choosing its node: no node yet"),
    "{logs}"
  );
}

#[test]
fn a_tracker_asked_on_a_context_that_went_gives_nothing() {
  let fake = FakePipeWire::new();
  let (shell, _pipewire, _peer) = connect(&fake, graph);
  let row = shell.cx.child();
  let pipewire = row.pipewire();
  row.owner().dispose();
  let logs = capture_logs(|| {
    assert_eq!(pipewire.node_links(31).get(), vec![]);
  });
  assert!(
    logs.contains("on a context that is gone gives nothing"),
    "{logs}"
  );
}

#[test]
fn a_link_changes_state_and_a_removed_ones_id_is_reused() {
  let fake = FakePipeWire::new();
  let (mut shell, pipewire, mut peer) = connect(&fake, graph);
  peer.set_state(51, 4);
  peer.set_state(52, -2);
  run(&mut shell, &mut peer);
  let states: Vec<_> = (pipewire.links().get_untracked().iter())
    .map(|link| link.state)
    .collect();
  assert_eq!(
    states,
    [PwLinkState::Active, PwLinkState::Active, PwLinkState::Error]
  );
  peer.remove(51);
  peer.link(53, (31, 2), (32, 7), 0);
  run(&mut shell, &mut peer);
  assert_eq!(peer.binds, vec![3, 4, 5, 4], "the id PipeWire gave back");
  let ids: Vec<_> = (pipewire.links().get_untracked().iter())
    .map(|link| link.id)
    .collect();
  assert_eq!(ids, [50, 52, 53]);
  peer.remove(30);
  run(&mut shell, &mut peer);
  let nodes = pipewire.nodes().get_untracked();
  assert_eq!(
    nodes.iter().map(|node| node.id).collect::<Vec<_>>(),
    [31, 32]
  );
}

#[test]
fn a_ping_is_answered() {
  let fake = FakePipeWire::new();
  let (mut shell, _pipewire, mut peer) = connect(&fake, |_| {});
  peer.send(0, 2, &structure(&[int(0), int(77)]));
  run(&mut shell, &mut peer);
  assert_eq!(peer.pongs, vec![77]);
}

#[test]
fn a_program_s_captured_video_source_is_a_screencast() {
  let fake = FakePipeWire::new();
  let (mut shell, pipewire, mut peer) = connect(&fake, |peer| {
    peer.node(
      60,
      &[
        ("node.name", "xdpw-stream"),
        ("media.class", "Video/Source"),
      ],
    );
    peer.node(
      61,
      &[
        ("node.name", "camera"),
        ("media.class", "Video/Source"),
        ("device.id", "9"),
      ],
    );
    peer.node(
      62,
      &[
        ("node.name", "firefox"),
        ("media.class", "Stream/Input/Video"),
      ],
    );
    peer.link(70, (61, 1), (62, 2), 4);
  });
  assert!(
    pipewire.screencasts().get_untracked().is_empty(),
    "a camera isn't one"
  );
  peer.link(71, (60, 3), (62, 4), 3);
  run(&mut shell, &mut peer);
  let shared = node(60, "xdpw-stream", "xdpw-stream", "Video/Source");
  assert_eq!(pipewire.screencasts().get_untracked(), vec![shared]);
  peer.remove(71);
  run(&mut shell, &mut peer);
  assert!(pipewire.screencasts().get_untracked().is_empty());
}

#[test]
fn past_the_object_cap_globals_are_skipped_and_held_back_binds_still_go() {
  let fake = FakePipeWire::new();
  let mut shell = Harness::new(fake.config());
  let pipewire = shell.cx.pipewire();
  let mut peer = fake.accept().expect("the shell connected");
  for id in 0..MAX_OBJECTS as u32 + 2 {
    peer.link(1000 + id, (1, 1), (2, 2), 4);
  }
  let logs = capture_logs(|| {
    // More binds than the socket holds: the registry is answered, then
    // nothing is read until the shell has queued them all, and nothing
    // comes back to read after.
    peer.pump();
    (peer.deaf, peer.mute) = (true, true);
    run_for(&mut shell, &mut peer, Duration::from_millis(300));
    peer.deaf = false;
    let give_up = Instant::now() + Duration::from_secs(10);
    while peer.binds.len() < MAX_OBJECTS {
      assert!(Instant::now() < give_up, "not every bind after 10 s");
      run(&mut shell, &mut peer);
    }
    peer.mute = false;
    for id in 0..MAX_OBJECTS as u32 {
      peer.set_state(1000 + id, 4);
    }
    while pipewire.links().get_untracked().len() < MAX_OBJECTS {
      assert!(Instant::now() < give_up, "not every link after 10 s");
      run(&mut shell, &mut peer);
    }
  });
  assert_eq!(
    logs.matches("more nodes and links than").count(),
    1,
    "{logs}"
  );
  assert_eq!(
    peer.binds.len(),
    MAX_OBJECTS,
    "a bind each, past a full socket"
  );
  assert_eq!(pipewire.link_groups().get_untracked().len(), 1);
}

#[test]
fn long_texts_are_cut_at_a_character() {
  let fake = FakePipeWire::new();
  let long = "é".repeat(MAX_TEXT);
  let (_shell, pipewire, _peer) = connect(&fake, |peer| {
    peer.node(30, &[("node.name", &long), ("media.class", "Audio/Sink")]);
  });
  let nodes = pipewire.nodes().get_untracked();
  assert_eq!(nodes[0].name.len(), MAX_TEXT);
  assert_eq!(
    nodes[0].description, nodes[0].name,
    "the name, as no description came"
  );
}

#[test]
fn a_bad_message_is_logged_and_the_rest_read() {
  let fake = FakePipeWire::new();
  let (mut shell, pipewire, mut peer) = connect(&fake, graph);
  let logs = capture_logs(|| {
    peer.set_state(50, 9);
    peer.send(2, 0, &int(1));
    peer.set_state(51, 3);
    run(&mut shell, &mut peer);
  });
  assert!(
    logs.contains("a link state PipeWire doesn't have"),
    "{logs}"
  );
  assert!(logs.contains("a message that isn't a struct"), "{logs}");
  let states: Vec<_> = (pipewire.links().get_untracked().iter())
    .map(|link| link.state)
    .collect();
  assert_eq!(
    states,
    [
      PwLinkState::Active,
      PwLinkState::Paused,
      PwLinkState::Paused
    ]
  );
}

#[test]
fn a_message_over_the_cap_loses_the_connection_for_good() {
  let fake = FakePipeWire::new();
  let (mut shell, pipewire, mut peer) = connect(&fake, graph);
  let logs = capture_logs(|| {
    let header = [2u32, MAX_MESSAGE as u32 + 1, 0, 0];
    let header: Vec<u8> = header.iter().flat_map(|word| word.to_ne_bytes()).collect();
    peer.stream.write_all(&header).expect("a header");
    run(&mut shell, &mut peer);
  });
  assert!(logs.contains("a message over 1 MiB"), "{logs}");
  assert!(pipewire.nodes().get_untracked().is_empty());
  assert!(fake.accept().is_none(), "nothing reconnects");
}

#[test]
fn a_closed_connection_is_made_again_once() {
  let fake = FakePipeWire::new();
  let (mut shell, pipewire, mut peer) = connect(&fake, graph);
  // Something past the graph, so the close is worth a reconnect.
  peer.set_state(51, 4);
  run(&mut shell, &mut peer);
  drop(peer);
  shell.run_until(Instant::now() + Duration::from_millis(50));
  let mut again = fake.accept().expect("connected again");
  assert_eq!(pipewire.nodes().get_untracked().len(), 3, "kept meanwhile");
  again.node(33, &[("node.name", "new"), ("media.class", "Audio/Sink")]);
  run(&mut shell, &mut again);
  let ids: Vec<_> = (pipewire.nodes().get_untracked().iter())
    .map(|node| node.id)
    .collect();
  assert_eq!(ids, [33], "the new graph replaced the old");
  // Past the graph again, so the next close is worth a reconnect too.
  again.node(34, &[("node.name", "later"), ("media.class", "Audio/Sink")]);
  run(&mut shell, &mut again);
  drop(again);
  shell.run_until(Instant::now() + Duration::from_millis(50));
  let mut third = fake.accept().expect("connected again");
  run(&mut shell, &mut third);
  // Closed again before anything past the graph: lost.
  drop(third);
  let logs = capture_logs(|| {
    shell.run_until(Instant::now() + Duration::from_millis(50));
  });
  assert!(logs.contains("lost PipeWire"), "{logs}");
  assert!(fake.accept().is_none(), "nothing reconnects");
  assert!(pipewire.nodes().get_untracked().is_empty());
}

#[test]
fn a_connection_whose_socket_went_is_lost() {
  let fake = FakePipeWire::new();
  let (mut shell, pipewire, mut peer) = connect(&fake, graph);
  peer.set_state(51, 4);
  run(&mut shell, &mut peer);
  std::fs::remove_file(fake.dir.join("pipewire-0")).expect("the socket");
  drop(peer);
  shell.run_until(Instant::now() + Duration::from_millis(50));
  assert!(pipewire.nodes().get_untracked().is_empty());
}

#[test]
fn a_server_that_reads_nothing_is_lost() {
  let fake = FakePipeWire::new();
  let (mut shell, pipewire, mut peer) = connect(&fake, graph);
  peer.deaf = true;
  let logs = capture_logs(|| {
    // Each ping's pong waits, until over 1 MiB of them.
    let pings: Vec<u8> = (0..60_000)
      .flat_map(|seq| {
        let mut message = Vec::new();
        pod::message(&mut message, 0, 2, 0, &structure(&[int(0), int(seq)]));
        message
      })
      .collect();
    let writer = peer.stream.try_clone().expect("a second handle");
    writer.set_nonblocking(false).expect("a blocking stream");
    let sent = std::thread::spawn(move || {
      let _ = (&writer).write_all(&pings);
    });
    let give_up = Instant::now() + Duration::from_secs(10);
    while !pipewire.nodes().get_untracked().is_empty() {
      assert!(Instant::now() < give_up, "still connected after 10 s");
      shell.run_until(Instant::now() + Duration::from_millis(10));
    }
    drop(peer);
    sent.join().expect("the pings went");
  });
  assert!(logs.contains("reads nothing the shell sends"), "{logs}");
}

#[test]
fn without_pipewire_the_lists_stay_empty_and_nothing_wakes() {
  let shell = Harness::new(offline());
  let logs = capture_logs(|| {
    shell.cx.pipewire();
  });
  assert!(logs.contains("no runtime directory"), "{logs}");

  let fake = FakePipeWire::new();
  let config = fake.config();
  drop(fake);
  let mut shell = Harness::new(config);
  let logs = capture_logs(|| {
    let pipewire = shell.cx.pipewire();
    assert!(pipewire.nodes().get_untracked().is_empty());
  });
  assert!(logs.contains("can't connect to PipeWire"), "{logs}");
  let logs = capture_logs(|| {
    shell.cx.pipewire();
  });
  assert!(logs.is_empty(), "{logs}");
  let wakeups = shell.run_until(Instant::now() + Duration::from_millis(300));
  assert_eq!(wakeups, 1, "nothing retries");
}

#[test]
fn a_shell_that_never_asks_never_connects() {
  let fake = FakePipeWire::new();
  let mut shell = Harness::new(fake.config());
  shell.run_until(Instant::now() + Duration::from_millis(50));
  assert!(fake.accept().is_none());
}

#[test]
fn a_quiet_graph_wakes_nothing() {
  let fake = FakePipeWire::new();
  let (mut shell, _pipewire, _peer) = connect(&fake, graph);
  let wakeups = shell.run_until(Instant::now() + Duration::from_millis(500));
  assert_eq!(wakeups, 1, "only the run's own deadline");
}
