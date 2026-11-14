use std::cell::{Cell, RefCell};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;

use super::{
  Hyprland, HyprlandEvent, HyprlandMonitor, HyprlandToplevel, HyprlandWorkspace, MAX_LIST,
  MAX_REPLY, MAX_REQUEST, MAX_TEXT,
};
use crate::testing::{Harness, TempDir, capture_logs, offline};
use crate::{Config, ServicesExt};

/// A stand-in for Hyprland: its two sockets in a short temporary
/// directory, since socket paths are capped at 108 bytes. The directory
/// goes on drop.
struct FakeHyprland {
  dir: PathBuf,
  requests: UnixListener,
  events: UnixListener,
}

impl FakeHyprland {
  fn new() -> FakeHyprland {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "crabshell-hypr-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let listen = |name: &str| {
      let listener = UnixListener::bind(dir.join(name)).expect("a socket");
      listener
        .set_nonblocking(true)
        .expect("a non-blocking socket");
      listener
    };
    FakeHyprland {
      requests: listen(".socket.sock"),
      events: listen(".socket2.sock"),
      dir,
    }
  }

  fn config(&self) -> Config {
    Config {
      hyprland: Some(self.dir.clone()),
      ..offline()
    }
  }

  /// The shell's event connection, made at its first `cx.hyprland()`.
  fn events(&self) -> UnixStream {
    let (stream, _) = self.events.accept().expect("an event connection");
    stream.set_nonblocking(false).expect("a blocking stream");
    stream
  }

  /// Every request the shell sent and Hyprland hasn't taken yet, in the
  /// order they came. The shell writes each at once.
  fn requests(&self) -> Vec<Request> {
    let mut requests = Vec::new();
    loop {
      let stream = match self.requests.accept() {
        Ok((stream, _)) => stream,
        Err(error) if error.kind() == ErrorKind::WouldBlock => return requests,
        Err(error) => panic!("accept failed: {error}"),
      };
      stream.set_nonblocking(false).expect("a blocking stream");
      let timeout = Some(Duration::from_secs(1));
      stream.set_read_timeout(timeout).expect("a read timeout");
      let mut request = Request {
        command: String::new(),
        stream,
      };
      // Hyprland reads what has arrived as the whole request.
      let mut bytes = vec![0; 64 * 1024];
      let read = request.stream.read(&mut bytes).expect("a request");
      request.command = String::from_utf8(bytes[..read].to_vec()).expect("UTF-8");
      requests.push(request);
    }
  }

  /// The commands of every waiting request, each answered from
  /// `replies`, or with `[]` when it isn't there.
  fn answer(&self, replies: &[(&str, &str)]) -> Vec<String> {
    let requests = self.requests();
    let commands = requests.iter().map(|request| request.command.clone());
    let commands = commands.collect();
    for request in requests {
      let reply = replies
        .iter()
        .find(|(command, _)| *command == request.command)
        .map_or("[]", |(_, reply)| reply);
      request.answer(reply);
    }
    commands
  }
}

impl Drop for FakeHyprland {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

/// One request the shell sent, waiting for Hyprland's reply.
struct Request {
  command: String,
  stream: UnixStream,
}

impl Request {
  /// Replies and closes, as Hyprland does.
  fn answer(mut self, reply: &str) {
    self.stream.write_all(reply.as_bytes()).expect("a reply");
  }
}

fn run(shell: &mut Harness) -> usize {
  shell.run_until(Instant::now() + Duration::from_millis(30))
}

/// The lists as Hyprland before 0.57 writes them, with fields the service
/// ignores: DP-1 shows workspace 1 and has the focus, HDMI-A-1 shows 2,
/// and 3 is hidden on DP-1.
const WORKSPACES: &str = r#"[
  {"id":2,"name":"2","monitor":"HDMI-A-1","monitorID":1,"windows":0,"hasfullscreen":false,"lastwindow":"0x0","lastwindowtitle":"","ispersistent":false},
  {"id":1,"name":"1","monitor":"DP-1","monitorID":0,"windows":1,"hasfullscreen":false,"lastwindow":"0x5a1","lastwindowtitle":"vim","ispersistent":false},
  {"id":3,"name":"3","monitor":"DP-1","monitorID":0,"windows":1,"hasfullscreen":true,"lastwindow":"0x5b2","lastwindowtitle":"Crabshell","ispersistent":false}
]"#;

const MONITORS: &str = r#"[
  {"id":0,"name":"DP-1","description":"Dell U2720Q","make":"Dell","model":"U2720Q","serial":"X","width":3840,"height":2160,"refreshRate":60.00000,"x":0,"y":0,"activeWorkspace":{"id":1,"name":"1"},"specialWorkspace":{"id":0,"name":""},"reserved":[0,30,0,0],"scale":1.50,"transform":0,"focused":true,"dpmsStatus":true,"vrr":false,"disabled":false},
  {"id":1,"name":"HDMI-A-1","description":"LG","make":"LG","model":"27","serial":"Y","width":1920,"height":1080,"refreshRate":60.00000,"x":2560,"y":0,"activeWorkspace":{"id":2,"name":"2"},"specialWorkspace":{"id":0,"name":""},"reserved":[0,0,0,0],"scale":1.00,"transform":0,"focused":false,"dpmsStatus":true,"vrr":false,"disabled":false}
]"#;

const CLIENTS: &str = r#"[
  {"address":"0x5a1","mapped":true,"hidden":false,"at":[0,30],"size":[2560,1410],"workspace":{"id":1,"name":"1"},"floating":false,"monitor":0,"class":"kitty","title":"vim","initialClass":"kitty","initialTitle":"kitty","pid":10,"xwayland":false,"pinned":false,"fullscreen":0,"fullscreenClient":0,"grouped":[],"tags":[],"swallowing":"0x0","focusHistoryID":0},
  {"address":"0x5b2","mapped":true,"hidden":false,"at":[0,0],"size":[2560,1440],"workspace":{"id":3,"name":"3"},"floating":true,"monitor":0,"class":"firefox","title":"Crabshell","initialClass":"firefox","initialTitle":"Firefox","pid":11,"xwayland":false,"pinned":false,"fullscreen":2,"fullscreenClient":2,"grouped":[],"tags":[],"swallowing":"0x0","focusHistoryID":1}
]"#;

const ACTIVE_WINDOW: &str = r#"{"address":"0x5a1","class":"kitty","title":"vim"}"#;

fn replies() -> Vec<(&'static str, &'static str)> {
  vec![
    ("j/workspaces", WORKSPACES),
    ("j/monitors", MONITORS),
    ("j/clients", CLIENTS),
    ("j/activewindow", ACTIVE_WINDOW),
  ]
}

fn workspace(
  id: i64,
  monitor: &str,
  windows: u32,
  focused: bool,
  active: bool,
) -> HyprlandWorkspace {
  HyprlandWorkspace {
    id: Some(id),
    name: id.to_string(),
    monitor: monitor.to_owned(),
    windows,
    focused,
    active,
    urgent: false,
  }
}

fn expected_workspaces() -> Vec<HyprlandWorkspace> {
  vec![
    workspace(1, "DP-1", 1, true, true),
    workspace(2, "HDMI-A-1", 0, false, true),
    workspace(3, "DP-1", 1, false, false),
  ]
}

fn expected_toplevels() -> Vec<HyprlandToplevel> {
  vec![
    HyprlandToplevel {
      address: "0x5a1".to_owned(),
      class: "kitty".to_owned(),
      title: "vim".to_owned(),
      workspace: "1".to_owned(),
      monitor: "DP-1".to_owned(),
      floating: false,
      fullscreen: false,
      focused: true,
      urgent: false,
    },
    HyprlandToplevel {
      address: "0x5b2".to_owned(),
      class: "firefox".to_owned(),
      title: "Crabshell".to_owned(),
      workspace: "3".to_owned(),
      monitor: "DP-1".to_owned(),
      floating: true,
      fullscreen: true,
      focused: false,
      urgent: false,
    },
  ]
}

/// A shell connected to `fake`, its first reads answered, and the event
/// connection's other end.
fn connect(fake: &FakeHyprland) -> (Harness, Hyprland, UnixStream) {
  let mut shell = Harness::new(fake.config());
  let hyprland = shell.cx.hyprland();
  let events = fake.events();
  fake.answer(&replies());
  run(&mut shell);
  assert_eq!(hyprland.workspaces().get_untracked(), expected_workspaces());
  (shell, hyprland, events)
}

#[test]
fn the_lists_are_read_whole_at_the_start() {
  let fake = FakeHyprland::new();
  let mut shell = Harness::new(fake.config());
  let hyprland = shell.cx.hyprland();
  let _events = fake.events();
  let commands = fake.answer(&replies());
  assert_eq!(
    commands,
    ["j/workspaces", "j/monitors", "j/clients", "j/activewindow"]
  );
  run(&mut shell);
  assert_eq!(hyprland.workspaces().get_untracked(), expected_workspaces());
  assert_eq!(
    hyprland.monitors().get_untracked(),
    [
      HyprlandMonitor {
        id: 0,
        name: "DP-1".to_owned(),
        description: "Dell U2720Q".to_owned(),
        focused: true,
        active_workspace: "1".to_owned(),
        x: 0,
        y: 0,
        width: 3840,
        height: 2160,
        scale: 1.5,
      },
      HyprlandMonitor {
        id: 1,
        name: "HDMI-A-1".to_owned(),
        description: "LG".to_owned(),
        focused: false,
        active_workspace: "2".to_owned(),
        x: 2560,
        y: 0,
        width: 1920,
        height: 1080,
        scale: 1.0,
      },
    ]
  );
  assert_eq!(hyprland.toplevels().get_untracked(), expected_toplevels());
  shell.cx.hyprland();
  assert!(fake.requests().is_empty(), "one connection per shell");
}

#[test]
fn workspaces_hyprland_0_57_leaves_unnumbered_are_matched_by_address() {
  let fake = FakeHyprland::new();
  let mut shell = Harness::new(fake.config());
  let hyprland = shell.cx.hyprland();
  let _events = fake.events();
  fake.answer(&[
    (
      "j/workspaces",
      r#"[{"address":"1","id":1,"type":"normal","name":"1","monitor":"DP-1","monitorID":0,"windows":1,"hasfullscreen":false},
          {"address":"web","type":"normal","name":"web","monitor":"DP-1","monitorID":0,"windows":1,"hasfullscreen":false}]"#,
    ),
    (
      "j/monitors",
      r#"[{"id":0,"name":"DP-1","description":"D","width":1920,"height":1080,"x":0,"y":0,"activeWorkspace":{"address":"web","type":"normal","name":"web"},"specialWorkspace":{"address":"","type":"","name":""},"scale":1.00,"focused":true}]"#,
    ),
    (
      "j/clients",
      r#"[{"address":"0x5a1","workspace":{"address":"web","type":"normal","name":"web"},"floating":false,"monitor":0,"class":"kitty","title":"vim","fullscreen":false},
          {"address":"0x5b2","workspace":{"address":"1","id":1,"type":"normal","name":"1"},"floating":false,"monitor":0,"class":"foot","title":"maximized","fullscreen":1}]"#,
    ),
    ("j/activewindow", "{}"),
  ]);
  run(&mut shell);
  let web = HyprlandWorkspace {
    id: None,
    name: "web".to_owned(),
    monitor: "DP-1".to_owned(),
    windows: 1,
    focused: true,
    active: true,
    urgent: false,
  };
  assert_eq!(
    hyprland.workspaces().get_untracked(),
    [web, workspace(1, "DP-1", 1, false, false)],
    "named ones first, as Hyprland before 0.57 numbers them below zero"
  );
  let toplevels = hyprland.toplevels().get_untracked();
  assert_eq!(toplevels[0].workspace, "web");
  assert!(!toplevels[0].focused && !toplevels[0].fullscreen);
  assert!(!toplevels[1].fullscreen, "maximized isn't fullscreen");
}

#[test]
fn without_the_signature_the_lists_stay_empty_and_that_is_logged_once() {
  let mut shell = Harness::new(offline());
  let (hyprland, logs) = capture_logs_with(|| shell.cx.hyprland());
  assert!(logs.contains("HYPRLAND_INSTANCE_SIGNATURE"), "{logs}");
  let (_, again) = capture_logs_with(|| shell.cx.hyprland());
  assert!(again.is_empty(), "{again}");
  assert!(hyprland.workspaces().get_untracked().is_empty());
  assert_eq!(run(&mut shell), 1, "nothing wakes the shell");
}

#[test]
fn missing_sockets_leave_the_lists_empty_and_are_logged_once() {
  let dir = TempDir::new();
  let mut shell = Harness::new(Config {
    hyprland: Some(dir.path().to_owned()),
    ..offline()
  });
  let (hyprland, logs) = capture_logs_with(|| shell.cx.hyprland());
  assert!(logs.contains("can't connect to Hyprland"), "{logs}");
  let (_, again) = capture_logs_with(|| shell.cx.hyprland());
  assert!(again.is_empty(), "{again}");
  assert!(hyprland.monitors().get_untracked().is_empty());
  assert_eq!(run(&mut shell), 1, "nothing wakes the shell");

  // Only the event socket: four reads fail, and one is logged.
  let _events = UnixListener::bind(dir.path().join(".socket2.sock")).expect("a socket");
  let mut shell = Harness::new(Config {
    hyprland: Some(dir.path().to_owned()),
    ..offline()
  });
  let (hyprland, logs) = capture_logs_with(|| shell.cx.hyprland());
  let failures = logs.matches("can't send Hyprland a request").count();
  assert_eq!(failures, 1, "{logs}");
  let service = hyprland.service.as_ref().expect("provided");
  let bursts = service
    .state
    .borrow()
    .as_ref()
    .map(|state| state.bursts.len());
  assert_eq!(bursts, Some(0), "reads that went nowhere wait for nothing");
  assert_eq!(run(&mut shell), 1, "nothing wakes the shell");
}

#[test]
fn an_event_reads_its_lists_again() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  // A switch to workspace 3, as Hyprland announces it: both versions.
  events
    .write_all(b"workspace>>3\nworkspacev2>>3,3\n")
    .expect("events");
  run(&mut shell);
  let monitors = MONITORS.replacen(r#"{"id":1,"name":"1"}"#, r#"{"id":3,"name":"3"}"#, 1);
  let commands = fake.answer(&[("j/workspaces", WORKSPACES), ("j/monitors", &monitors)]);
  assert_eq!(commands, ["j/workspaces", "j/monitors"]);
  run(&mut shell);
  assert_eq!(
    hyprland.workspaces().get_untracked(),
    [
      workspace(1, "DP-1", 1, false, false),
      workspace(2, "HDMI-A-1", 0, false, true),
      workspace(3, "DP-1", 1, true, true),
    ]
  );
  assert_eq!(hyprland.monitors().get_untracked()[0].active_workspace, "3");

  events.write_all(b"activewindowv2>>5b2\n").expect("events");
  run(&mut shell);
  let active = r#"{"address":"0x5b2"}"#;
  assert_eq!(
    fake.answer(&[("j/clients", CLIENTS), ("j/activewindow", active)]),
    ["j/clients", "j/activewindow"]
  );
  run(&mut shell);
  let focused = hyprland.toplevels().get_untracked();
  assert!(!focused[0].focused && focused[1].focused);

  events
    .write_all(b"windowtitlev2>>5b2,New\n")
    .expect("events");
  run(&mut shell);
  assert_eq!(fake.answer(&[("j/clients", CLIENTS)]), ["j/clients"]);
  run(&mut shell);
  events
    .write_all(b"openwindow>>5c3,1,foot,foot\n")
    .expect("events");
  run(&mut shell);
  assert_eq!(fake.answer(&replies()), ["j/workspaces", "j/clients"]);
  run(&mut shell);
  events
    .write_all(b"monitoraddedv2>>2,DP-2,Acer\n")
    .expect("events");
  run(&mut shell);
  assert_eq!(
    fake.answer(&replies()),
    ["j/workspaces", "j/monitors", "j/clients"]
  );
  run(&mut shell);
  events.write_all(b"submap>>resize\n").expect("events");
  run(&mut shell);
  assert!(fake.requests().is_empty(), "nothing shown changes");
}

#[test]
fn events_during_a_read_read_once_more_after_it() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  events.write_all(b"workspacev2>>3,3\n").expect("events");
  run(&mut shell);
  let held = fake.requests();
  let commands: Vec<_> = held.iter().map(|request| request.command.clone()).collect();
  assert_eq!(commands, ["j/workspaces", "j/monitors"]);
  for _ in 0..3 {
    events.write_all(b"workspacev2>>2,2\n").expect("events");
    run(&mut shell);
    assert!(fake.requests().is_empty(), "one read of a list at a time");
  }
  let monitors = MONITORS.replacen(r#"{"id":1,"name":"1"}"#, r#"{"id":3,"name":"3"}"#, 1);
  for request in held {
    let reply = if request.command == "j/monitors" {
      monitors.as_str()
    } else {
      WORKSPACES
    };
    request.answer(reply);
  }
  run(&mut shell);
  assert!(
    hyprland.workspaces().get_untracked()[2].focused,
    "the answers in flight show, as a later event's reads hold nothing"
  );
  assert_eq!(
    fake.answer(&replies()),
    ["j/workspaces", "j/monitors"],
    "then one more read catches up"
  );
  run(&mut shell);
  assert_eq!(hyprland.workspaces().get_untracked(), expected_workspaces());
  assert!(fake.requests().is_empty());
}

#[test]
fn the_event_socket_closing_is_logged_and_empties_the_lists() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, events) = connect(&fake);
  // Hyprland is gone, and its socket with it.
  std::fs::remove_file(fake.dir.join(".socket2.sock")).expect("the socket file");
  drop(events);
  let logs = capture_logs(|| {
    run(&mut shell);
  });
  assert!(logs.contains("Hyprland closed its event socket"), "{logs}");
  assert!(!logs.contains("connecting again"), "{logs}");
  assert!(hyprland.workspaces().get_untracked().is_empty());
  assert!(hyprland.monitors().get_untracked().is_empty());
  assert!(hyprland.toplevels().get_untracked().is_empty());
  assert_eq!(run(&mut shell), 1, "nothing reconnects");
}

/// Hyprland closes an event client that falls 64 events behind, and runs
/// on, so its socket is still there.
#[test]
fn a_closed_event_socket_is_connected_again_once() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, events) = connect(&fake);
  drop(events);
  let logs = capture_logs(|| {
    run(&mut shell);
  });
  assert!(logs.contains("connecting again"), "{logs}");
  let mut events = fake.events();
  assert_eq!(
    fake.answer(&replies()),
    ["j/workspaces", "j/monitors", "j/clients", "j/activewindow"],
    "what was missed is read again"
  );
  run(&mut shell);
  assert_eq!(hyprland.workspaces().get_untracked(), expected_workspaces());

  // After an event, a close is followed again.
  events.write_all(b"submap>>resize\n").expect("events");
  run(&mut shell);
  drop(events);
  run(&mut shell);
  let events = fake.events();
  fake.answer(&replies());
  run(&mut shell);

  // A connection that closes with no event in between isn't.
  drop(events);
  let logs = capture_logs(|| {
    run(&mut shell);
  });
  assert!(logs.contains("lost Hyprland"), "{logs}");
  assert!(hyprland.workspaces().get_untracked().is_empty());
  assert!(fake.events.accept().is_err(), "no third connection");
  assert_eq!(run(&mut shell), 1, "nothing retries");
}

#[test]
fn on_event_on_a_context_that_went_is_logged_and_does_nothing() {
  let fake = FakeHyprland::new();
  let (mut shell, _hyprland, mut events) = connect(&fake);
  let child = shell.cx.child();
  let nodes = Runtime::current().node_count();
  let stale = child.hyprland();
  assert_eq!(
    Runtime::current().node_count(),
    nodes,
    "a handle makes no node"
  );
  child.owner().dispose();
  let heard = Rc::new(Cell::new(0));
  let kept = Rc::clone(&heard);
  let logs = capture_logs(|| stale.on_event(move |_| kept.set(kept.get() + 1)));
  assert!(logs.contains("context that is gone"), "{logs}");
  events.write_all(b"submap>>resize\n").expect("events");
  run(&mut shell);
  assert_eq!(heard.get(), 0);
  assert_eq!(Rc::strong_count(&heard), 1, "the handler wasn't kept");
}

#[test]
fn bad_replies_and_lines_are_logged_and_skipped() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  events
    .write_all(b"no separator\nmonitoraddedv2>>2,DP-2,Acer\n")
    .expect("events");
  let logs = capture_logs(|| {
    run(&mut shell);
    // One workspace Hyprland got wrong among good ones.
    let workspaces = WORKSPACES.replacen(r#""windows":0"#, r#""windows":"none""#, 1);
    fake.answer(&[
      ("j/workspaces", &workspaces),
      ("j/monitors", "{not json"),
      ("j/clients", "[1, 2]"),
    ]);
    run(&mut shell);
  });
  assert!(logs.contains("no separator"), "{logs}");
  assert!(logs.contains("a bad monitor list from Hyprland"), "{logs}");
  let mut expected = expected_workspaces();
  expected.remove(1);
  assert_eq!(hyprland.workspaces().get_untracked(), expected);
  assert_eq!(hyprland.monitors().get_untracked().len(), 2, "kept");
  assert!(hyprland.toplevels().get_untracked().is_empty());
}

#[test]
fn a_reply_over_the_cap_is_dropped() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  events
    .write_all(b"openwindow>>5c3,1,a,b\n")
    .expect("events");
  run(&mut shell);
  let clients = take_clients(&fake);
  let writer = std::thread::spawn(move || {
    let mut stream = clients.stream;
    let _ = stream.write_all(&vec![b' '; MAX_REPLY + 1]);
  });
  let logs = capture_logs(|| {
    shell.retry_until("the writer done", || writer.is_finished(), |_| {});
  });
  writer.join().expect("the writer");
  assert!(logs.contains("over 8 MiB"), "{logs}");
  assert_eq!(hyprland.toplevels().get_untracked(), expected_toplevels());
}

#[test]
fn long_texts_are_cut_at_a_character_and_long_lists_capped() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  events
    .write_all(b"openwindow>>5c3,1,a,b\n")
    .expect("events");
  run(&mut shell);
  let long = "é".repeat(MAX_TEXT);
  let window = |n: usize| {
    let title = if n == 0 { long.as_str() } else { "t" };
    format!(
      r#"{{"address":"0x{n:x}","workspace":{{"id":1,"name":"1"}},"floating":false,"monitor":0,"class":"c","title":"{title}","fullscreen":0}}"#
    )
  };
  let clients = format!(
    "[{}]",
    (0..=MAX_LIST).map(window).collect::<Vec<_>>().join(",")
  );
  let request = take_clients(&fake);
  let writer = std::thread::spawn(move || request.answer(&clients));
  let logs = capture_logs(|| {
    shell.retry_until(
      "the long list",
      || !hyprland.toplevels().get_untracked().is_empty() && writer.is_finished(),
      |_| {},
    );
  });
  let toplevels = hyprland.toplevels().get_untracked();
  assert_eq!(toplevels.len(), MAX_LIST);
  assert!(logs.contains("windows"), "{logs}");
  assert_eq!(toplevels[0].title, "é".repeat(MAX_TEXT / 2));
}

#[test]
fn a_quiet_connection_wakes_nothing() {
  let fake = FakeHyprland::new();
  let (mut shell, _hyprland, _events) = connect(&fake);
  let wakeups = shell.run_until(Instant::now() + Duration::from_millis(500));
  assert_eq!(wakeups, 1, "only the run's own deadline");
}

/// The waiting read of the windows. The other waiting reads are answered.
fn take_clients(fake: &FakeHyprland) -> Request {
  let (mut clients, others): (Vec<_>, Vec<_>) = fake
    .requests()
    .into_iter()
    .partition(|request| request.command == "j/clients");
  for request in others {
    request.answer(WORKSPACES);
  }
  clients.pop().expect("a read of the windows")
}

/// What `f` returns and what it logged.
fn capture_logs_with<R>(f: impl FnOnce() -> R) -> (R, String) {
  let mut result = None;
  let logs = capture_logs(|| result = Some(f()));
  (result.expect("f ran"), logs)
}

#[test]
fn dispatch_sends_its_command_and_logs_a_refusal() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, _events) = connect(&fake);
  let logs = capture_logs(|| {
    hyprland.dispatch("workspace 2");
    let commands = fake.answer(&[("dispatch workspace 2", "ok")]);
    assert_eq!(commands, ["dispatch workspace 2"]);
    run(&mut shell);
  });
  assert!(logs.is_empty(), "{logs}");
  let logs = capture_logs(|| {
    hyprland.dispatch("bogus");
    fake.answer(&[("dispatch bogus", "Invalid dispatcher")]);
    run(&mut shell);
    hyprland.dispatch(&"x".repeat(MAX_REQUEST));
  });
  assert!(logs.contains("Invalid dispatcher"), "{logs}");
  assert!(logs.contains("too long"), "{logs}");
  assert!(fake.requests().is_empty(), "a long command isn't sent");

  let shell = Harness::new(offline());
  let logs = capture_logs(|| shell.cx.hyprland().dispatch("workspace 2"));
  assert!(logs.contains("no Hyprland to dispatch to"), "{logs}");
}

#[test]
fn every_event_reaches_on_event_and_parse_splits_its_fields() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  let heard = Rc::new(RefCell::new(Vec::new()));
  let kept = Rc::clone(&heard);
  hyprland.on_event(move |event| kept.borrow_mut().push(event.clone()));
  events
    .write_all(b"submap>>resize\nwindowtitlev2>>5a1,vim, the editor\n")
    .expect("events");
  run(&mut shell);
  let heard = heard.borrow();
  let event = |name: &str, data: &str| HyprlandEvent {
    name: name.to_owned(),
    data: data.to_owned(),
  };
  assert_eq!(
    *heard,
    [
      event("submap", "resize"),
      event("windowtitlev2", "5a1,vim, the editor")
    ]
  );
  assert_eq!(heard[1].parse(2), ["5a1", "vim, the editor"]);
  assert_eq!(heard[1].parse(5), ["5a1", "vim", " the editor"]);
  assert_eq!(heard[0].parse(1), ["resize"]);
}

#[test]
fn a_panicking_on_event_is_logged_with_where_it_was_called_and_the_rest_runs() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  let panics = Rc::new(Cell::new(0));
  let counted = Rc::clone(&panics);
  hyprland.on_event(move |event| {
    counted.set(counted.get() + 1);
    panic!("bad event {}", event.name);
  });
  let heard = Rc::new(Cell::new(0));
  let kept = Rc::clone(&heard);
  let inner = hyprland.clone();
  hyprland.on_event(move |_| {
    // Nothing of the service is borrowed while a handler runs.
    inner.dispatch("workspace 1");
    inner.on_event(|_| {});
    kept.set(kept.get() + 1);
  });
  events.write_all(b"workspacev2>>3,3\n").expect("events");
  let logs = capture_logs(|| {
    run(&mut shell);
  });
  let at = "Hyprland on_event (called at crates/crabshell-services/src/hyprland/tests.rs:";
  assert!(logs.contains(at), "{logs}");
  let panicked = "panicked on workspacev2: bad event workspacev2";
  assert!(logs.contains(panicked), "{logs}");
  assert_eq!(heard.get(), 1);
  assert_eq!(
    fake.answer(&replies()),
    ["j/workspaces", "j/monitors", "dispatch workspace 1"],
    "the event still reads its lists"
  );
  events.write_all(b"submap>>resize\n").expect("events");
  run(&mut shell);
  assert_eq!(
    panics.get(),
    1,
    "a handler that panicked isn't called again"
  );
  assert_eq!(heard.get(), 2);
}

#[test]
fn an_on_event_goes_with_its_context() {
  let fake = FakeHyprland::new();
  let (mut shell, _hyprland, mut events) = connect(&fake);
  let nodes = Runtime::current().node_count();
  let child = shell.cx.child();
  let heard = Rc::new(Cell::new(0));
  let kept = Rc::clone(&heard);
  child.hyprland().on_event(move |_| kept.set(kept.get() + 1));
  events.write_all(b"submap>>resize\n").expect("events");
  run(&mut shell);
  assert_eq!(heard.get(), 1);
  child.owner().dispose();
  assert_eq!(Runtime::current().node_count(), nodes);
  assert_eq!(Rc::strong_count(&heard), 1, "the handler was dropped");
  events.write_all(b"submap>>resize\n").expect("events");
  run(&mut shell);
  assert_eq!(heard.get(), 1);
}

#[test]
fn urgent_marks_a_window_and_its_workspace_until_it_is_focused_or_closed() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  let urgent = || {
    let toplevels = hyprland.toplevels().get_untracked();
    let workspaces = hyprland.workspaces().get_untracked();
    let windows: Vec<_> = toplevels.iter().map(|toplevel| toplevel.urgent).collect();
    let spaces: Vec<_> = workspaces
      .iter()
      .map(|workspace| workspace.urgent)
      .collect();
    (windows, spaces)
  };
  events.write_all(b"urgent>>5b2\n").expect("events");
  run(&mut shell);
  assert_eq!(urgent(), (vec![false, true], vec![false, false, true]));
  events.write_all(b"activewindowv2>>5b2\n").expect("events");
  run(&mut shell);
  assert_eq!(urgent(), (vec![false; 2], vec![false; 3]), "focused");
  fake.answer(&[
    ("j/clients", CLIENTS),
    ("j/activewindow", r#"{"address":"0x5b2"}"#),
  ]);
  run(&mut shell);

  events.write_all(b"urgent>>5a1\n").expect("events");
  run(&mut shell);
  assert_eq!(urgent().0, [true, false]);
  events.write_all(b"closewindow>>5a1\n").expect("events");
  run(&mut shell);
  assert_eq!(urgent().0, [false, false], "closed");
}

/// What `j/status` says: a Lua config and a hyprlang one, as Hyprland
/// 0.55 to 0.57 write it, and what earlier ones answer.
const STATUS_LUA: &str = "\n{\n    \"configProvider\": \"lua\",\n    \"backend\": \"drm\"\n}\n";
const STATUS_HYPRLANG: &str =
  "\n{\n    \"configProvider\": \"hyprlang\",\n    \"backend\": \"drm\"\n}\n";
const STATUS_OLD: &str = "unknown request";

#[test]
fn focus_workspace_asks_the_config_once_and_speaks_its_syntax() {
  for (status, two, web) in [
    (
      STATUS_LUA,
      r#"dispatch hl.dsp.focus({ workspace = "2" })"#,
      r#"dispatch hl.dsp.focus({ workspace = "name:web" })"#,
    ),
    (
      STATUS_HYPRLANG,
      "dispatch workspace 2",
      "dispatch workspace name:web",
    ),
    (
      STATUS_OLD,
      "dispatch workspace 2",
      "dispatch workspace name:web",
    ),
  ] {
    let fake = FakeHyprland::new();
    let (mut shell, hyprland, _events) = connect(&fake);
    hyprland.focus_workspace("2");
    hyprland.focus_workspace("name:web");
    let logs = capture_logs(|| {
      assert_eq!(
        fake.answer(&[("j/status", status)]),
        ["j/status"],
        "asked once, and the switches wait for it"
      );
      run(&mut shell);
    });
    assert!(!logs.contains("names no config"), "{status}: {logs}");
    assert_eq!(fake.answer(&[]), [two, web], "after {status}");
    hyprland.focus_workspace("2");
    assert_eq!(fake.answer(&[]), [two], "the config is kept");
  }
}

#[test]
fn a_workspace_name_is_a_lua_string_that_runs_nothing() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, _events) = connect(&fake);
  hyprland.focus_workspace("x\" }) hl.dsp.exec_cmd(\"rm -rf ~\") --\\\n\u{7f}é");
  fake.answer(&[("j/status", STATUS_LUA)]);
  run(&mut shell);
  assert_eq!(
    fake.answer(&[]),
    [
      r#"dispatch hl.dsp.focus({ workspace = "x\" }) hl.dsp.exec_cmd(\"rm -rf ~\") --\\\010\127é" })"#
    ]
  );
}

/// The old syntax sends a name as it is, so one with a space or a line
/// break could say more than a workspace.
#[test]
fn the_old_syntax_refuses_a_name_with_spaces_or_control_characters() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, _events) = connect(&fake);
  hyprland.focus_workspace("name:my web");
  fake.answer(&[("j/status", STATUS_HYPRLANG)]);
  let logs = capture_logs(|| {
    run(&mut shell);
    for target in ["x\ty", "x\ny", "x\u{7f}", "x\u{a0}y"] {
      hyprland.focus_workspace(target);
    }
    hyprland.focus_workspace("name:web");
  });
  assert_eq!(logs.matches("can't switch to it").count(), 5, "{logs}");
  assert_eq!(fake.answer(&[]), ["dispatch workspace name:web"]);
}

#[test]
fn a_status_that_says_nothing_is_logged_and_read_as_the_old_syntax() {
  for status in ["ok", r#"{"configProvider":"error","backend":"drm"}"#] {
    let fake = FakeHyprland::new();
    let (mut shell, hyprland, _events) = connect(&fake);
    hyprland.focus_workspace("2");
    let logs = capture_logs(|| {
      fake.answer(&[("j/status", status)]);
      run(&mut shell);
    });
    assert!(logs.contains("Hyprland's status"), "{status}: {logs}");
    assert_eq!(fake.answer(&[]), ["dispatch workspace 2"], "{status}");
  }

  let shell = Harness::new(offline());
  let logs = capture_logs(|| shell.cx.hyprland().focus_workspace("2"));
  assert!(logs.contains("no Hyprland to dispatch to"), "{logs}");
}

#[test]
fn at_most_eight_requests_wait_for_hyprland_and_the_rest_queue() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, _events) = connect(&fake);
  for n in 0..20 {
    hyprland.dispatch(&format!("workspace {n}"));
  }
  let mut sent = Vec::new();
  for batch in [8, 8, 4] {
    let commands = fake.answer(&[]);
    assert_eq!(commands.len(), batch, "{commands:?}");
    sent.extend(commands);
    run(&mut shell);
  }
  let expected: Vec<_> = (0..20).map(|n| format!("dispatch workspace {n}")).collect();
  assert_eq!(sent, expected, "in the order they were asked");
  assert!(fake.requests().is_empty());
}

#[test]
fn past_64_waiting_dispatches_one_is_refused() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, _events) = connect(&fake);
  let logs = capture_logs(|| {
    for n in 0..70 {
      hyprland.dispatch(&format!("workspace {n}"));
    }
  });
  assert_eq!(logs.matches("64 dispatches wait").count(), 6, "{logs}");
  let mut sent = 0;
  loop {
    let answered = fake.answer(&[]).len();
    if answered == 0 {
      break;
    }
    sent += answered;
    run(&mut shell);
  }
  assert_eq!(sent, 64);
}

/// Hyprland 0.56 and older read a request 1023 bytes at a time until a
/// shorter read or its end, so a request of exactly 1023 bytes would
/// hang it unless the shell ends its writing.
#[test]
fn a_request_ends_its_writing() {
  let fake = FakeHyprland::new();
  let (_shell, hyprland, _events) = connect(&fake);
  let command = "x".repeat(1023 - "dispatch ".len());
  hyprland.dispatch(&command);
  let (mut stream, _) = fake.requests.accept().expect("a request");
  stream.set_nonblocking(false).expect("a blocking stream");
  let timeout = Some(Duration::from_millis(500));
  stream.set_read_timeout(timeout).expect("a read timeout");
  let mut request = Vec::new();
  stream
    .read_to_end(&mut request)
    .expect("the request, then its end");
  assert_eq!(request, format!("dispatch {command}").as_bytes());
}

#[test]
fn a_shown_special_workspace_is_active_and_focused_on_the_focused_monitor() {
  let fake = FakeHyprland::new();
  let mut shell = Harness::new(fake.config());
  let hyprland = shell.cx.hyprland();
  let _events = fake.events();
  let special = |name: &str, id: i64, monitor: &str| {
    format!(
      r#"{{"id":{id},"name":"special:{name}","monitor":"{monitor}","monitorID":0,"windows":1,"hasfullscreen":false}},"#
    )
  };
  let shown = format!(
    "{}{}",
    special("magic", -98, "DP-1"),
    special("notes", -97, "HDMI-A-1")
  );
  let workspaces = WORKSPACES.replacen('[', &format!("[{shown}"), 1);
  let monitors = MONITORS
    .replacen(
      r#""specialWorkspace":{"id":0,"name":""}"#,
      r#""specialWorkspace":{"id":-98,"name":"special:magic"}"#,
      1,
    )
    .replacen(
      r#""specialWorkspace":{"id":0,"name":""}"#,
      r#""specialWorkspace":{"id":-97,"name":"special:notes"}"#,
      1,
    );
  fake.answer(&[
    ("j/workspaces", &workspaces),
    ("j/monitors", &monitors),
    ("j/clients", CLIENTS),
    ("j/activewindow", ACTIVE_WINDOW),
  ]);
  run(&mut shell);
  let list = hyprland.workspaces().get_untracked();
  let flags = |name: &str| {
    let workspace = list.iter().find(|workspace| workspace.name == name);
    workspace.map(|workspace| (workspace.active, workspace.focused))
  };
  assert_eq!(
    flags("special:magic"),
    Some((true, true)),
    "on DP-1, focused"
  );
  assert_eq!(flags("special:notes"), Some((true, false)));
  assert_eq!(flags("1"), Some((true, true)), "still under it");
}

#[test]
fn the_lists_show_once_no_read_is_under_way() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  // Workspace 1 empties and goes, and a new 4 on DP-1 has the focus.
  events
    .write_all(b"workspacev2>>4,4\ndestroyworkspacev2>>1,1\n")
    .expect("events");
  run(&mut shell);
  let (workspaces, monitors): (Vec<_>, Vec<_>) = fake
    .requests()
    .into_iter()
    .partition(|request| request.command == "j/workspaces");
  let list = WORKSPACES.replacen(
    r#"{"id":1,"name":"1","monitor":"DP-1""#,
    r#"{"id":4,"name":"4","monitor":"DP-1""#,
    1,
  );
  for request in workspaces {
    request.answer(&list);
  }
  run(&mut shell);
  assert_eq!(
    hyprland.workspaces().get_untracked(),
    expected_workspaces(),
    "nothing shows while the monitors are read"
  );
  let switched = MONITORS.replacen(r#"{"id":1,"name":"1"}"#, r#"{"id":4,"name":"4"}"#, 1);
  for request in monitors {
    request.answer(&switched);
  }
  run(&mut shell);
  let shown = hyprland.workspaces().get_untracked();
  let focused = shown.iter().find(|workspace| workspace.focused);
  assert_eq!(focused.map(|workspace| workspace.id), Some(Some(4)));
}

#[test]
fn every_event_reads_the_lists_it_can_change() {
  const WMC: &[&str] = &["j/workspaces", "j/monitors", "j/clients"];
  let table: &[(&[&str], &[&str])] = &[
    (
      &[
        "workspace",
        "workspacev2",
        "focusedmon",
        "focusedmonv2",
        "createworkspace",
        "createworkspacev2",
        "destroyworkspace",
        "destroyworkspacev2",
        "activespecial",
        "activespecialv2",
      ],
      &WMC[..2],
    ),
    (
      &[
        "moveworkspace",
        "moveworkspacev2",
        "renameworkspace",
        "changeworkspaceid",
        "monitoradded",
        "monitoraddedv2",
        "monitorremoved",
        "monitorremovedv2",
        "configreloaded",
      ],
      WMC,
    ),
    (
      &["openwindow", "closewindow", "movewindow", "movewindowv2"],
      &["j/workspaces", "j/clients"],
    ),
    (
      &[
        "windowtitle",
        "windowtitlev2",
        "changefloatingmode",
        "fullscreen",
      ],
      &["j/clients"],
    ),
    (
      &["activewindow", "activewindowv2"],
      &["j/clients", "j/activewindow"],
    ),
    (&["submap", "urgent", "openlayer", "screencast"], &[]),
  ];
  let fake = FakeHyprland::new();
  let (mut shell, _hyprland, mut events) = connect(&fake);
  for (names, reads) in table {
    for name in *names {
      events
        .write_all(format!("{name}>>1,2\n").as_bytes())
        .expect("events");
      run(&mut shell);
      assert_eq!(fake.answer(&replies()), *reads, "after {name}");
      run(&mut shell);
    }
  }
}

#[test]
fn at_most_4096_windows_are_kept_urgent() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  let lines: String = (0..=MAX_LIST).map(|n| format!("urgent>>{n:x}\n")).collect();
  events.write_all(lines.as_bytes()).expect("events");
  let service = hyprland.service.as_ref().expect("provided");
  let urgent = || {
    let state = service.state.borrow();
    state.as_ref().map_or(0, |state| state.urgent.len())
  };
  shell.retry_until("the urgent events read", || urgent() >= MAX_LIST, |_| {});
  run(&mut shell);
  assert_eq!(urgent(), MAX_LIST);
}

/// A window whose title changes faster than Hyprland answers `j/clients`
/// keeps the windows reading, which holds no other list off the screen.
#[test]
fn a_list_that_keeps_reading_holds_no_other_off_the_screen() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  events.write_all(b"workspacev2>>3,3\n").expect("events");
  run(&mut shell);
  let held = fake.requests();
  events.write_all(b"windowtitlev2>>5a1,a\n").expect("events");
  run(&mut shell);
  let mut clients = fake.requests();
  assert_eq!(clients.len(), 1);
  let monitors = MONITORS.replacen(r#"{"id":1,"name":"1"}"#, r#"{"id":3,"name":"3"}"#, 1);
  for request in held {
    let reply = if request.command == "j/monitors" {
      monitors.clone()
    } else {
      WORKSPACES.to_owned()
    };
    request.answer(&reply);
  }
  run(&mut shell);
  assert!(
    hyprland.workspaces().get_untracked()[2].focused,
    "the switch shows while the windows are read for a later event"
  );
  for round in 0..20 {
    events.write_all(b"windowtitlev2>>5a1,b\n").expect("events");
    run(&mut shell);
    let request = clients.pop().expect("a read of the windows");
    let title = format!("title {round}");
    request.answer(&CLIENTS.replacen("vim", &title, 1));
    run(&mut shell);
    clients = fake.requests();
    assert_eq!(clients.len(), 1, "round {round}");
    let toplevels = hyprland.toplevels().get_untracked();
    assert_eq!(
      toplevels[0].title, title,
      "round {round}: each answer shows"
    );
  }
}

/// Events Hyprland dropped before closing the socket may have focused or
/// closed an urgent window.
#[test]
fn a_reconnect_clears_the_urgency_the_events_it_missed_ended() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, mut events) = connect(&fake);
  events
    .write_all(b"urgent>>5a1\nurgent>>5b2\nurgent>>5c3\nopenwindow>>5e5,1,x,y\n")
    .expect("events");
  run(&mut shell);
  // Reads sent before the socket closed, answered after.
  let held = fake.requests();
  assert_eq!(held.len(), 2);
  drop(events);
  run(&mut shell);
  let mut events = fake.events();
  // Raised after the reconnect, before the window shows in a list.
  events.write_all(b"urgent>>5d4\n").expect("events");
  run(&mut shell);
  for request in held {
    request.answer("[]");
  }
  run(&mut shell);
  let replies = [
    ("j/workspaces", WORKSPACES),
    ("j/monitors", MONITORS),
    ("j/clients", CLIENTS),
    ("j/activewindow", r#"{"address":"0x5b2"}"#),
  ];
  while !fake.answer(&replies).is_empty() {
    run(&mut shell);
  }
  let service = hyprland.service.as_ref().expect("provided");
  let state = service.state.borrow();
  let state = state.as_ref().expect("started");
  let mut urgent: Vec<_> = state.urgent.iter().cloned().collect();
  urgent.sort();
  assert_eq!(
    urgent,
    ["0x5a1", "0x5d4"],
    "0x5b2 was focused and 0x5c3 closed meanwhile"
  );
  assert!(state.recheck.is_none(), "checked once");
}

#[test]
fn a_status_that_cant_be_asked_drops_its_switches_and_is_asked_again() {
  let mut fake = FakeHyprland::new();
  let (mut shell, hyprland, _events) = connect(&fake);
  let socket = fake.dir.join(".socket.sock");
  std::fs::remove_file(&socket).expect("the request socket");
  let logs = capture_logs(|| hyprland.focus_workspace("2"));
  assert!(logs.contains("can't send Hyprland a request"), "{logs}");
  fake.requests = UnixListener::bind(&socket).expect("a socket");
  fake
    .requests
    .set_nonblocking(true)
    .expect("a non-blocking socket");
  hyprland.focus_workspace("3");
  assert_eq!(
    fake.answer(&[("j/status", STATUS_HYPRLANG)]),
    ["j/status"],
    "asked again"
  );
  run(&mut shell);
  assert_eq!(fake.answer(&[]), ["dispatch workspace 3"]);
}

#[test]
fn losing_hyprland_drops_what_waited_to_be_sent() {
  let fake = FakeHyprland::new();
  let (mut shell, hyprland, events) = connect(&fake);
  for n in 0..10 {
    hyprland.dispatch(&format!("workspace {n}"));
  }
  // Behind the 8 out: two dispatches and the status a switch waits for.
  hyprland.focus_workspace("2");
  let held = fake.requests();
  assert_eq!(held.len(), 8);
  std::fs::remove_file(fake.dir.join(".socket2.sock")).expect("the event socket");
  drop(events);
  capture_logs(|| {
    run(&mut shell);
  });
  hyprland.dispatch("after");
  hyprland.focus_workspace("3");
  assert_eq!(
    fake.answer(&[("j/status", STATUS_HYPRLANG)]),
    ["dispatch after", "j/status"]
  );
  run(&mut shell);
  assert_eq!(fake.answer(&[]), ["dispatch workspace 3"]);
  drop(held);
}

#[test]
fn a_shown_special_workspace_hyprland_0_57_names_by_address_is_marked() {
  let fake = FakeHyprland::new();
  let mut shell = Harness::new(fake.config());
  let hyprland = shell.cx.hyprland();
  let _events = fake.events();
  fake.answer(&[
    (
      "j/workspaces",
      r#"[{"address":"1","id":1,"type":"normal","name":"1","monitor":"DP-1","monitorID":0,"windows":0,"hasfullscreen":false},
          {"address":"special:magic","type":"special","name":"special:magic","monitor":"DP-1","monitorID":0,"windows":1,"hasfullscreen":false}]"#,
    ),
    (
      "j/monitors",
      r#"[{"id":0,"name":"DP-1","description":"D","width":1920,"height":1080,"x":0,"y":0,"activeWorkspace":{"address":"1","id":1,"type":"normal","name":"1"},"specialWorkspace":{"address":"special:magic","type":"special","name":"special:magic"},"scale":1.00,"focused":true}]"#,
    ),
    ("j/clients", "[]"),
    ("j/activewindow", "{}"),
  ]);
  run(&mut shell);
  let list = hyprland.workspaces().get_untracked();
  let special = list
    .iter()
    .find(|workspace| workspace.name == "special:magic");
  assert_eq!(
    special.map(|workspace| (workspace.active, workspace.focused)),
    Some((true, true))
  );
}
