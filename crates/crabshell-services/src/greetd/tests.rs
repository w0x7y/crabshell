use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{GreetMessage, GreetState, Greetd, MAX_MESSAGE};
use crate::testing::{Harness, capture_logs, offline};
use crate::{Config, ServicesExt};

/// A stand-in for greetd: a socket in a short temporary directory.
struct FakeGreetd {
  dir: PathBuf,
  listener: UnixListener,
}

impl FakeGreetd {
  fn new() -> FakeGreetd {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "crabshell-greetd-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let listener = UnixListener::bind(dir.join("sock")).expect("a socket");
    FakeGreetd { dir, listener }
  }

  fn config(&self) -> Config {
    Config {
      greetd: Some(self.dir.join("sock")),
      ..offline()
    }
  }

  /// The greeter's connection, made at its first request.
  fn accept(&self) -> Peer {
    let (stream, _) = self.listener.accept().expect("the greeter connects");
    stream
      .set_read_timeout(Some(Duration::from_secs(2)))
      .expect("a read timeout");
    Peer(stream)
  }
}

impl Drop for FakeGreetd {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

struct Peer(UnixStream);

impl Peer {
  fn request(&mut self) -> Value {
    let mut length = [0; 4];
    self.0.read_exact(&mut length).expect("a request's length");
    let mut body = vec![0; u32::from_ne_bytes(length) as usize];
    self.0.read_exact(&mut body).expect("a request");
    serde_json::from_slice(&body).expect("JSON")
  }

  fn reply(&mut self, reply: Value) {
    let body = reply.to_string();
    let length = u32::try_from(body.len()).expect("small").to_ne_bytes();
    self.0.write_all(&length).expect("a reply's length");
    self.0.write_all(body.as_bytes()).expect("a reply");
  }
}

fn run(shell: &mut Harness) {
  shell.run_until(Instant::now() + Duration::from_millis(30));
}

fn state(greetd: &Greetd) -> GreetState {
  greetd.state().get_untracked()
}

#[test]
fn a_session_is_made_answered_and_started() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  assert_eq!(state(&greetd), GreetState::Idle);
  greetd.create_session("me");
  assert_eq!(state(&greetd), GreetState::Busy);
  let mut peer = greetd_.accept();
  assert_eq!(
    peer.request(),
    json!({"type": "create_session", "username": "me"})
  );
  peer.reply(
    json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "Password:"}),
  );
  run(&mut shell);
  assert_eq!(
    state(&greetd),
    GreetState::Asking {
      message: String::from("Password:"),
      visible: false
    }
  );
  greetd.respond("hunter2");
  assert_eq!(
    peer.request(),
    json!({"type": "post_auth_message_response", "response": "hunter2"})
  );
  peer.reply(json!({"type": "success"}));
  run(&mut shell);
  assert_eq!(state(&greetd), GreetState::Authenticated);
  greetd.start_session(vec![String::from("sway")], vec![String::from("A=B")]);
  assert_eq!(
    peer.request(),
    json!({"type": "start_session", "cmd": ["sway"], "env": ["A=B"]})
  );
  peer.reply(json!({"type": "success"}));
  run(&mut shell);
  assert_eq!(state(&greetd), GreetState::Started);
}

#[test]
fn informative_messages_are_kept_and_acknowledged() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  peer.request();
  peer.reply(json!({"type": "auth_message", "auth_message_type": "info", "auth_message": "Hello"}));
  run(&mut shell);
  assert_eq!(
    peer.request(),
    json!({"type": "post_auth_message_response", "response": null}),
    "acknowledged with no answer"
  );
  peer.reply(
    json!({"type": "auth_message", "auth_message_type": "error", "auth_message": "Caps lock"}),
  );
  run(&mut shell);
  peer.request();
  peer.reply(
    json!({"type": "auth_message", "auth_message_type": "visible", "auth_message": "Code:"}),
  );
  run(&mut shell);
  assert_eq!(
    greetd.messages().get_untracked(),
    [
      GreetMessage {
        text: String::from("Hello"),
        error: false
      },
      GreetMessage {
        text: String::from("Caps lock"),
        error: true
      }
    ]
  );
  assert_eq!(
    state(&greetd),
    GreetState::Asking {
      message: String::from("Code:"),
      visible: true
    }
  );
}

#[test]
fn a_wrong_answer_fails_ends_the_attempt_and_lets_another_begin() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  peer.request();
  peer.reply(
    json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "Password:"}),
  );
  run(&mut shell);
  greetd.respond("wrong");
  peer.request();
  peer.reply(json!({"type": "error", "error_type": "auth_error", "description": "nope"}));
  run(&mut shell);
  let failed = GreetState::Failed {
    message: String::from("nope"),
    auth: true,
  };
  assert_eq!(state(&greetd), failed);
  assert_eq!(peer.request(), json!({"type": "cancel_session"}));
  peer.reply(json!({"type": "success"}));
  run(&mut shell);
  assert_eq!(state(&greetd), failed, "the failure still shows");

  greetd.create_session("me");
  assert_eq!(
    peer.request(),
    json!({"type": "create_session", "username": "me"})
  );
}

#[test]
fn a_new_session_while_one_is_made_cancels_it_first() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  peer.request();
  peer.reply(json!({"type": "success"}));
  run(&mut shell);
  assert_eq!(state(&greetd), GreetState::Authenticated);
  greetd.create_session("you");
  assert_eq!(peer.request(), json!({"type": "cancel_session"}));
  peer.reply(json!({"type": "success"}));
  run(&mut shell);
  assert_eq!(
    peer.request(),
    json!({"type": "create_session", "username": "you"})
  );
}

#[test]
fn cancel_ends_the_session_being_made() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  peer.request();
  peer.reply(
    json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "Password:"}),
  );
  run(&mut shell);
  greetd.cancel();
  assert_eq!(peer.request(), json!({"type": "cancel_session"}));
  peer.reply(json!({"type": "success"}));
  run(&mut shell);
  assert_eq!(state(&greetd), GreetState::Idle);
}

#[test]
fn without_greetd_a_request_fails_and_says_why() {
  let mut shell = Harness::new(offline());
  let greetd = shell.cx.greetd();
  capture_logs(|| greetd.create_session("me"));
  run(&mut shell);
  let GreetState::Failed { message, auth } = state(&greetd) else {
    panic!("{:?}", state(&greetd));
  };
  assert!(message.contains("GREETD_SOCK isn't set"), "{message}");
  assert!(!auth);
}

#[test]
fn greetd_going_away_or_sending_too_much_fails_the_attempt() {
  for oversize in [false, true] {
    let greetd_ = FakeGreetd::new();
    let mut shell = Harness::new(greetd_.config());
    let greetd = shell.cx.greetd();
    greetd.create_session("me");
    let mut peer = greetd_.accept();
    peer.request();
    if oversize {
      let length = u32::try_from(MAX_MESSAGE + 1).unwrap().to_ne_bytes();
      peer.0.write_all(&length).unwrap();
    } else {
      drop(peer);
    }
    capture_logs(|| run(&mut shell));
    assert!(
      matches!(state(&greetd), GreetState::Failed { auth: false, .. }),
      "{:?}",
      state(&greetd)
    );
  }
}

#[test]
fn an_answer_is_never_logged() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  let logs = capture_logs(|| {
    greetd.create_session("me");
    let mut peer = greetd_.accept();
    peer.request();
    peer.reply(
      json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "Password:"}),
    );
    run(&mut shell);
    greetd.respond("correct-horse");
    peer.request();
    peer.reply(json!({"type": "error", "error_type": "auth_error", "description": "nope"}));
    run(&mut shell);
    drop(peer);
    run(&mut shell);
  });
  assert!(!logs.contains("correct-horse"), "{logs}");
}

#[test]
fn a_quiet_connection_wakes_nothing() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  peer.request();
  peer.reply(
    json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "Password:"}),
  );
  run(&mut shell);
  let wakeups = shell.run_until(Instant::now() + Duration::from_millis(500));
  assert_eq!(wakeups, 1, "only the run's own deadline");
}
