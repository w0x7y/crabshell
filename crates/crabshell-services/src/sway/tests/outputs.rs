//! The outputs and commands, against the fake Sway.

use super::*;
use crate::sway::SwayOutput;
use crate::sway::ipc::GET_OUTPUTS;

/// A `GET_OUTPUTS` reply as Sway writes it, with fields the service
/// ignores: HEADLESS-1 at `scale`, focused, showing `workspace`, and
/// HEADLESS-2 disabled, which Sway gives a scale of -1 and no focus. And
/// the list it should become.
fn outputs(workspace: &str, scale: f64) -> (String, Vec<SwayOutput>) {
  let json = format!(
    r#"[{{"id":3,"type":"output","name":"HEADLESS-1","rect":{{"x":0,"y":0,"width":1280,"height":720}},"focused":true,"active":true,"power":true,"scale":{scale},"current_workspace":"{workspace}","modes":[]}},{{"type":"output","name":"HEADLESS-2","rect":{{"x":0,"y":0,"width":0,"height":0}},"active":false,"scale":-1.0,"current_workspace":null}}]"#
  );
  let list = vec![
    SwayOutput {
      name: "HEADLESS-1".to_owned(),
      focused: true,
      active: true,
      current_workspace: Some(workspace.to_owned()),
      x: 0,
      y: 0,
      width: 1280,
      height: 720,
      scale,
    },
    SwayOutput {
      name: "HEADLESS-2".to_owned(),
      focused: false,
      active: false,
      current_workspace: None,
      x: 0,
      y: 0,
      width: 0,
      height: 0,
      scale: 1.0,
    },
  ];
  (json, list)
}

#[test]
fn outputs_are_asked_for_on_first_use_and_again_on_each_event() {
  let fake = FakeSway::new();
  let (mut shell, mut peer) = Harness::connect(&fake);
  let sway = shell.cx.sway();
  peer.read_nothing();

  let list = sway.outputs();
  assert_eq!(peer.read(), (SUBSCRIBE, r#"["output"]"#.to_owned()));
  assert_eq!(peer.read(), (GET_OUTPUTS, String::new()));
  assert_eq!(list.get_untracked(), []);
  peer.send(SUBSCRIBE, r#"{"success":true}"#);
  let (json, expected) = outputs("1", 1.0);
  peer.send(GET_OUTPUTS, &json);
  shell.turn();
  assert_eq!(list.get_untracked(), expected);
  assert_eq!(shell.cx.child().sway().outputs(), list, "one list");
  peer.read_nothing();

  // An output event asks for the outputs alone.
  peer.send(ipc::OUTPUT_EVENT, r#"{"change":"unspecified"}"#);
  shell.turn();
  assert_eq!(peer.read(), (GET_OUTPUTS, String::new()));
  let (json, expected) = outputs("1", 2.0);
  peer.send(GET_OUTPUTS, &json);
  shell.turn();
  assert_eq!(list.get_untracked(), expected);

  // A workspace event asks for both, since it moves an output's workspace.
  peer.send(WORKSPACE_EVENT, r#"{"change":"focus"}"#);
  shell.turn();
  assert_eq!(peer.read(), (GET_WORKSPACES, String::new()));
  assert_eq!(peer.read(), (GET_OUTPUTS, String::new()));
  peer.send(GET_WORKSPACES, &two().0);
  let (json, expected) = outputs("2:web", 2.0);
  peer.send(GET_OUTPUTS, &json);
  shell.turn();
  assert_eq!(list.get_untracked(), expected);
  assert_eq!(sway.workspaces().get_untracked(), two().1);
  peer.read_nothing();
}

#[test]
fn a_reconnect_asks_for_both_lists() {
  let fake = FakeSway::new();
  let (mut shell, mut peer) = Harness::connect(&fake);
  let list = shell.cx.sway().outputs();
  peer.read();
  peer.read();
  peer.send(GET_OUTPUTS, &outputs("1", 1.0).0);
  shell.turn();
  assert_eq!(list.get_untracked().len(), 2);

  drop(peer);
  shell.turn();
  assert_eq!(list.get_untracked(), [], "empty while Sway is away");
  shell.run_for(RETRY + RETRY / 2);
  let mut peer = fake.accept();
  let subscribe = r#"["workspace","output"]"#.to_owned();
  assert_eq!(peer.read(), (SUBSCRIBE, subscribe));
  assert_eq!(peer.read(), (GET_WORKSPACES, String::new()));
  assert_eq!(peer.read(), (GET_OUTPUTS, String::new()));
  let (json, expected) = outputs("1", 1.0);
  peer.send(GET_OUTPUTS, &json);
  shell.turn();
  assert_eq!(list.get_untracked(), expected);
}

#[test]
fn a_bad_output_list_is_logged_and_skipped() {
  let logs = capture_logs(|| {
    let fake = FakeSway::new();
    let (mut shell, mut peer) = Harness::connect(&fake);
    let list = shell.cx.sway().outputs();
    peer.read();
    peer.read();
    let (json, expected) = outputs("1", 1.0);
    peer.send(GET_OUTPUTS, &json);
    shell.turn();
    for bad in [
      "[{not json",
      r#"[{"name":"DP-1","active":true}]"#,
      r#"[{"name":"DP-1","active":true,"rect":{"x":0,"y":0,"width":-1,"height":0}}]"#,
    ] {
      peer.send(GET_OUTPUTS, bad);
      shell.turn();
      assert_eq!(list.get_untracked(), expected, "kept after {bad}");
    }
    peer.send(GET_OUTPUTS, &outputs("3", 1.0).0);
    shell.turn();
    assert_eq!(list.get_untracked(), outputs("3", 1.0).1, "still connected");
  });
  assert_eq!(logs.matches("a bad output list").count(), 3, "{logs}");
}

#[test]
fn dispatch_sends_the_command_as_written_or_nothing() {
  let logs = capture_logs(|| {
    let fake = FakeSway::new();
    let (mut shell, mut peer) = Harness::connect(&fake);
    let sway = shell.cx.sway();
    sway.dispatch(r#"layout tabbed; [app_id="foot"] kill"#);
    let command = r#"layout tabbed; [app_id="foot"] kill"#.to_owned();
    assert_eq!(peer.read(), (RUN_COMMAND, command));
    let refused = r#"[{"success":true},{"success":false,"error":"No matching node."}]"#;
    peer.send(RUN_COMMAND, refused);
    shell.turn();

    sway.dispatch(&"x".repeat(ipc::MAX_PAYLOAD + 1));
    peer.read_nothing();
    drop(peer);
    shell.turn();
    sway.dispatch("exit");
  });
  assert!(logs.contains("error=No matching node."), "{logs}");
  assert!(logs.contains("over 4 MiB"), "{logs}");
  assert!(logs.contains("while Sway isn't connected"), "{logs}");
}
