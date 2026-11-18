//! The startup judgement against a fake compositor that describes its
//! second output, `FAKE-2`, a read after the first: the shell judges once
//! the compositor answers a sync sent when it heard of `FAKE-1`, so a bar
//! that works only on `FAKE-2` still counts.

#[allow(dead_code, reason = "these tests use part of the fake")]
#[path = "../../../crabshell-wayland/tests/fake/mod.rs"]
mod fake;

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crabshell_ui::{Cx, FloatingWindow, Output, PanelWindow, Text};
use crabshell_wayland::EventLoop;
use fake::{Fake, Globals};
use smithay_client_toolkit::reexports::client::Connection;

use crate::shell::{Shell, Startup};
use crate::{IntoPanels, ShellExt};

/// A shell on a new fake with a late second output, with `root`.
fn start(root: impl FnOnce(&Cx)) -> (Fake, Shell) {
  let (fake, stream) = Fake::start(Globals {
    late_output: true,
    ..Globals::default()
  });
  let connection = Connection::from_socket(stream).expect("a connection");
  let event_loop = EventLoop::with_connection(connection).expect("the platform starts");
  let shell = Shell::new(event_loop, root).expect("root runs");
  (fake, shell)
}

/// Dispatches until the judgement is in, for at most 5 s.
fn judge(shell: &mut Shell) -> Startup {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    match shell.startup {
      Startup::Waiting | Startup::Syncing(_) => {}
      verdict => return verdict,
    }
    assert!(Instant::now() < deadline, "no judgement");
    shell
      .dispatch(Some(Duration::from_millis(5)))
      .expect("the connection works");
  }
}

/// A bar on every output but `FAKE-1`, whose closure panics.
fn bar_but_on_fake_1(_: &Cx, output: &Output) -> Vec<PanelWindow> {
  assert!(output.name() != "FAKE-1", "no bar for FAKE-1");
  PanelWindow::new(output)
    .height(20)
    .child(Text::new("bar"))
    .into_panels()
}

#[test]
fn a_bar_on_the_output_described_later_counts() {
  let (_fake, mut shell) = start(|cx| cx.per_output(bar_but_on_fake_1));
  assert_eq!(judge(&mut shell), Startup::Passed);
  let outputs: Vec<String> = shell.platform().outputs().map(|o| o.name.clone()).collect();
  assert_eq!(outputs, ["FAKE-1", "FAKE-2"]);
}

#[test]
fn bars_that_all_panic_leave_nothing_to_show() {
  let (_fake, mut shell) = start(|cx| {
    cx.per_output(|_: &Cx, output: &Output| -> PanelWindow {
      panic!("no bar for {}", output.name())
    });
  });
  assert_eq!(judge(&mut shell), Startup::NothingToShow);
}

/// Hidden, as a launcher waiting for its shortcut is, so the fake, which
/// has no xdg-shell, is never asked for its surface.
#[test]
fn a_floating_window_beside_bars_that_all_panic_is_something() {
  let (_fake, mut shell) = start(|cx| {
    cx.floating(
      FloatingWindow::new()
        .visible(false)
        .child(Text::new("launcher")),
    );
    cx.per_output(|_: &Cx, output: &Output| -> PanelWindow {
      panic!("no bar for {}", output.name())
    });
  });
  assert_eq!(judge(&mut shell), Startup::Passed);
}

#[test]
fn a_shell_with_no_window_says_ready_once_judged() {
  let (_fake, mut shell) = start(|cx| cx.per_output(|_: &Cx, _: &Output| Vec::new()));
  let (mut supervisor, ours) = UnixStream::pair().expect("a socket pair");
  shell.follow_supervisor(ours, None);
  assert_eq!(judge(&mut shell), Startup::Passed);
  supervisor
    .set_read_timeout(Some(Duration::from_secs(1)))
    .expect("a timeout");
  let mut said = [0_u8; 19];
  supervisor
    .read_exact(&mut said)
    .expect("the shell says ready");
  assert_eq!(&said, b"can-hand-off\nready\n");
}

/// The lines the shell has said to `supervisor`, waiting up to 1 s for
/// `last`.
fn said_until(shell: &mut Shell, supervisor: &mut UnixStream, last: &str) -> String {
  supervisor.set_nonblocking(true).expect("nonblocking");
  let mut said = Vec::new();
  let deadline = Instant::now() + Duration::from_secs(1);
  while !String::from_utf8_lossy(&said)
    .lines()
    .any(|line| line == last)
  {
    assert!(
      Instant::now() < deadline,
      "no {last:?}: {}",
      String::from_utf8_lossy(&said)
    );
    shell
      .dispatch(Some(Duration::from_millis(5)))
      .expect("the connection works");
    let mut chunk = [0_u8; 64];
    if let Ok(read) = supervisor.read(&mut chunk) {
      said.extend_from_slice(&chunk[..read]);
    }
  }
  String::from_utf8(said).expect("UTF-8")
}

#[test]
fn told_to_hand_off_the_shell_saves_says_so_and_runs_on() {
  use std::io::Write;

  let (_fake, mut shell) = start(|cx| {
    cx.persist("clicks", 3_u32).set(4);
    cx.per_output(|_: &Cx, _: &Output| Vec::new());
  });
  let dir = std::env::temp_dir().join(format!("crabshell-handoff-{}", std::process::id()));
  let file = dir.join("crabshell").join("state.json");
  let (mut supervisor, ours) = UnixStream::pair().expect("a socket pair");
  shell.follow_supervisor(ours, Some(file.clone()));
  said_until(&mut shell, &mut supervisor, "ready");
  supervisor.write_all(b"handoff\n").expect("written");
  assert_eq!(said_until(&mut shell, &mut supervisor, "saved"), "saved\n");
  let saved = crate::persist::load(&file);
  assert_eq!(saved.get("clicks"), Some(&serde_json::json!(4)));
  assert_eq!(
    saved.get(crate::persist::HANDOFF_KEY),
    Some(&serde_json::json!(true)),
    "the next shell runs beside this one"
  );
  let _ = std::fs::remove_dir_all(&dir);
  // Still running: it goes on dispatching.
  shell
    .dispatch(Some(Duration::from_millis(5)))
    .expect("the connection works");
}

#[test]
fn reload_asks_the_supervisor_and_without_one_is_logged() {
  let (_fake, mut shell) = start(|cx| cx.per_output(|_: &Cx, _: &Output| Vec::new()));
  let logs = super::capture_logs(|| shell.cx().reload());
  assert!(logs.contains("cx.reload needs crabshell run"), "{logs}");
  let (mut supervisor, ours) = UnixStream::pair().expect("a socket pair");
  shell.follow_supervisor(ours, None);
  shell.cx().reload();
  let said = said_until(&mut shell, &mut supervisor, "reload");
  assert!(said.starts_with("can-hand-off\n"), "{said}");
}

/// A shell started from values saved for a handoff runs beside the old
/// one: it registers no global shortcut, which Hyprland would refuse by
/// ending its connection, until the supervisor says `alone`.
#[test]
fn a_shell_saved_for_a_handoff_waits_for_alone() {
  use std::io::Write;

  let (_fake, stream) = Fake::start(Globals::default());
  let connection = Connection::from_socket(stream).expect("a connection");
  let event_loop = EventLoop::with_connection(connection).expect("the platform starts");
  let mut saved = serde_json::Map::new();
  saved.insert(crate::persist::HANDOFF_KEY.to_owned(), true.into());
  let mut shell = None;
  let logs = super::capture_logs(|| {
    let built = Shell::build(event_loop, saved, None, |cx| {
      let _ = cx.global_shortcut("crabshell", "launcher", "Show the launcher");
      cx.per_output(|_: &Cx, _: &Output| Vec::new());
    });
    let mut built = built.expect("root runs");
    judge(&mut built);
    shell = Some(built);
  });
  let mut shell = shell.expect("the shell");
  assert!(
    !logs.contains("global shortcuts are never pressed"),
    "{logs}"
  );
  let (mut supervisor, ours) = UnixStream::pair().expect("a socket pair");
  shell.follow_supervisor(ours, None);
  supervisor.write_all(b"alone\n").expect("written");
  let logs = super::capture_logs(|| {
    for _ in 0..10 {
      shell
        .dispatch(Some(Duration::from_millis(5)))
        .expect("the connection works");
    }
  });
  assert!(logs.contains("running alone"), "{logs}");
  assert!(
    logs.contains("global shortcuts are never pressed"),
    "{logs}"
  );
}
