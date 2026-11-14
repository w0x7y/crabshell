use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::{Hyprland, HyprlandMonitor, HyprlandToplevel, HyprlandWorkspace};
use crate::testing::{Harness, TempDir, capture_logs, offline};
use crate::{Config, ServicesExt};

/// A stand-in for Hyprland: its request socket in a short temporary
/// directory, since socket paths are capped at 108 bytes. The directory
/// goes on drop.
struct FakeHyprland {
  dir: PathBuf,
  requests: UnixListener,
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
      dir,
    }
  }

  fn config(&self) -> Config {
    Config {
      hyprland: Some(self.dir.clone()),
      ..offline()
    }
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
    },
  ]
}

/// A shell connected to `fake`, its first reads answered.
fn connect(fake: &FakeHyprland) -> (Harness, Hyprland) {
  let mut shell = Harness::new(fake.config());
  let hyprland = shell.cx.hyprland();
  fake.answer(&replies());
  run(&mut shell);
  assert_eq!(hyprland.workspaces().get_untracked(), expected_workspaces());
  (shell, hyprland)
}

#[test]
fn the_lists_are_read_whole_at_the_start() {
  let fake = FakeHyprland::new();
  let mut shell = Harness::new(fake.config());
  let hyprland = shell.cx.hyprland();
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
  fake.answer(&[
    (
      "j/workspaces",
      r#"[{"address":"1","id":1,"type":"normal","name":"1","monitor":"DP-1","monitorID":0,"windows":1,"hasfullscreen":false},
          {"address":"web","type":"normal","name":"web","monitor":"DP-1","monitorID":0,"windows":1,"hasfullscreen":false}]"#,
    ),
    (
      "j/monitors",
      r#"[{"id":0,"name":"DP-1","description":"D","width":1920,"height":1080,"x":0,"y":0,"activeWorkspace":{"address":"web","type":"normal","name":"web"},"scale":1.00,"focused":true}]"#,
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
  assert_eq!(
    logs.matches("can't send Hyprland a request").count(),
    1,
    "{logs}"
  );
  let (_, again) = capture_logs_with(|| shell.cx.hyprland());
  assert!(again.is_empty(), "{again}");
  assert!(hyprland.monitors().get_untracked().is_empty());
  assert_eq!(run(&mut shell), 1, "nothing wakes the shell");
}

#[test]
fn a_quiet_connection_wakes_nothing() {
  let fake = FakeHyprland::new();
  let (mut shell, _hyprland) = connect(&fake);
  let wakeups = shell.run_until(Instant::now() + Duration::from_millis(500));
  assert_eq!(wakeups, 1, "only the run's own deadline");
}

/// What `f` returns and what it logged.
fn capture_logs_with<R>(f: impl FnOnce() -> R) -> (R, String) {
  let mut result = None;
  let logs = capture_logs(|| result = Some(f()));
  (result.expect("f ran"), logs)
}
