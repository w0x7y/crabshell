//! The Sway service against a private headless Sway, from
//! crabshell-wayland's test helper. Ignored, since they need `sway` and
//! `swaymsg`; run them without the session's `SWAYSOCK`, which the helper
//! refuses:
//!
//! ```sh
//! env -u SWAYSOCK cargo test -p crabshell-services --lib sway::tests::headless -- --ignored --test-threads=1
//! ```

use super::*;
use crate::sway::SwayOutput;

#[allow(dead_code, reason = "these tests use part of the helper")]
#[path = "../../../../crabshell-wayland/tests/sway/mod.rs"]
mod private;

/// How long any wait may take before the test fails.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A private Sway with one output, HEADLESS-1, and services pointed at it.
fn start() -> (private::Sway, Harness) {
  let sway = private::Sway::start();
  let socket = Socket {
    path: sway.socket(),
    retry: RETRY,
  };
  (sway, Harness::new(Some(socket)))
}

impl Harness {
  /// Runs the loop until `done` holds, failing the test after 10 s.
  fn run_until(&mut self, what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
      assert!(Instant::now() < deadline, "timed out waiting for {what}");
      self.run_for(Duration::from_millis(20));
    }
  }
}

#[test]
#[ignore = "needs sway and swaymsg; run by hand"]
fn outputs_follow_the_private_sway() {
  let (sway, mut shell) = start();
  let outputs = shell.cx.sway().outputs();
  let first = SwayOutput {
    name: "HEADLESS-1".to_owned(),
    focused: true,
    active: true,
    current_workspace: Some("1".to_owned()),
    x: 0,
    y: 0,
    width: 1280,
    height: 720,
    scale: 1.0,
  };
  shell.run_until("the first output", || {
    outputs.get_untracked() == [first.clone()]
  });

  sway.msg(&["output", "HEADLESS-1", "scale", "2"]);
  let scaled = SwayOutput {
    width: 640,
    height: 360,
    scale: 2.0,
    ..first.clone()
  };
  shell.run_until("the new scale", || {
    outputs.get_untracked() == [scaled.clone()]
  });

  // Sway places a new output right of the others, at its own size.
  sway.msg(&["create_output"]);
  let second = SwayOutput {
    name: "HEADLESS-2".to_owned(),
    focused: false,
    current_workspace: Some("2".to_owned()),
    x: 640,
    width: 1920,
    height: 1080,
    ..first.clone()
  };
  shell.run_until("a second output", || {
    outputs.with_untracked(|list| list.len() == 2 && list[1].active)
  });
  assert_eq!(outputs.get_untracked(), [scaled, second]);

  // A workspace event, not an output one, moves the focus.
  sway.msg(&["focus", "output", "HEADLESS-2"]);
  shell.run_until("the focus on the second", || {
    outputs.with_untracked(|list| !list[0].focused && list[1].focused)
  });

  // SwayFX gives a disabled output no scale; Sway gives -1.
  sway.msg(&["output", "HEADLESS-2", "disable"]);
  shell.run_until("the second disabled", || {
    outputs.with_untracked(|list| list.len() == 2 && !list[1].active)
  });
  let disabled = outputs.get_untracked().remove(1);
  assert_eq!((disabled.current_workspace, disabled.scale), (None, 1.0));
}

#[test]
#[ignore = "needs sway and swaymsg; run by hand"]
fn dispatch_runs_commands_in_the_private_sway() {
  let logs = capture_logs(|| {
    let (sway, mut shell) = start();
    let service = shell.cx.sway();
    let workspaces = service.workspaces();
    shell.run_until("the first list", || !workspaces.get_untracked().is_empty());
    service.dispatch("workspace 5; nonsense");
    shell.run_until("workspace 5 focused", || {
      workspaces.with_untracked(|list| list.iter().any(|ws| ws.name == "5" && ws.focused))
    });
    let json = sway.msg(&["-t", "get_workspaces", "-r"]);
    assert!(json.contains(r#""name": "5""#), "{json}");
    // Sway's reply to the command, with its refusal of the second.
    shell.run_for(Duration::from_millis(200));
  });
  assert!(logs.contains("Sway refused a command"), "{logs}");
  assert!(logs.contains("nonsense"), "{logs}");
}
