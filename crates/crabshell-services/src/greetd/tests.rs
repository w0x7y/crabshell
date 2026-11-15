use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{GreetMessage, GreetState, Greetd, MAX_MESSAGE, MAX_MESSAGES};
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
  // An error means greetd had no session left to end.
  for reply in [
    json!({"type": "success"}),
    json!({"type": "error", "error_type": "error", "description": "none"}),
  ] {
    let greetd_ = FakeGreetd::new();
    let mut shell = Harness::new(greetd_.config());
    let greetd = shell.cx.greetd();
    greetd.create_session("me");
    let mut peer = greetd_.accept();
    peer.request();
    peer.reply(secret());
    run(&mut shell);
    greetd.cancel();
    assert_eq!(peer.request(), json!({"type": "cancel_session"}));
    peer.reply(reply.clone());
    run(&mut shell);
    assert_eq!(state(&greetd), GreetState::Idle, "after {reply}");
  }
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

fn secret() -> Value {
  json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "Password:"})
}

impl Peer {
  /// Asserts the greeter sends nothing more for a while.
  fn quiet(&mut self) {
    let timeout = self.0.read_timeout().expect("a read timeout");
    self
      .0
      .set_read_timeout(Some(Duration::from_millis(200)))
      .expect("a read timeout");
    let mut byte = [0];
    let read = self.0.read(&mut byte);
    assert!(read.is_err(), "the greeter sent more: {read:?}");
    self.0.set_read_timeout(timeout).expect("a read timeout");
  }
}

#[test]
fn an_answer_to_a_superseded_request_is_dropped() {
  for reply in [
    json!({"type": "auth_message", "auth_message_type": "info", "auth_message": "Hi"}),
    json!({"type": "error", "error_type": "error", "description": "bad"}),
    secret(),
  ] {
    // Another user's session asked for while greetd answers the first.
    let greetd_ = FakeGreetd::new();
    let mut shell = Harness::new(greetd_.config());
    let greetd = shell.cx.greetd();
    greetd.create_session("me");
    let mut peer = greetd_.accept();
    peer.request();
    greetd.create_session("you");
    peer.reply(reply.clone());
    run(&mut shell);
    assert_eq!(state(&greetd), GreetState::Busy, "after {reply}");
    assert_eq!(greetd.messages().get_untracked(), []);
    assert_eq!(peer.request(), json!({"type": "cancel_session"}));
    peer.reply(json!({"type": "success"}));
    run(&mut shell);
    assert_eq!(
      peer.request(),
      json!({"type": "create_session", "username": "you"})
    );
    peer.reply(secret());
    run(&mut shell);
    greetd.respond("pw");
    assert_eq!(
      peer.request(),
      json!({"type": "post_auth_message_response", "response": "pw"}),
      "after {reply}"
    );

    // A cancel while greetd answers.
    let greetd_ = FakeGreetd::new();
    let mut shell = Harness::new(greetd_.config());
    let greetd = shell.cx.greetd();
    greetd.create_session("me");
    let mut peer = greetd_.accept();
    peer.request();
    greetd.cancel();
    peer.reply(reply.clone());
    run(&mut shell);
    assert_eq!(peer.request(), json!({"type": "cancel_session"}));
    peer.reply(json!({"type": "success"}));
    run(&mut shell);
    assert_eq!(state(&greetd), GreetState::Idle, "after {reply}");
    peer.quiet();
  }
}

#[test]
fn an_answer_or_start_out_of_turn_is_dropped() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  peer.request();
  greetd.start_session(vec![String::from("sway")], Vec::new());
  peer.reply(secret());
  run(&mut shell);
  peer.quiet();
  // Enter pressed twice.
  greetd.respond("pw");
  capture_logs(|| greetd.respond("pw"));
  peer.request();
  peer.reply(json!({"type": "success"}));
  run(&mut shell);
  assert_eq!(state(&greetd), GreetState::Authenticated);
  peer.quiet();
}

#[test]
fn a_failed_start_ends_its_session_before_the_next() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  peer.request();
  peer.reply(json!({"type": "success"}));
  run(&mut shell);
  greetd.start_session(vec![String::from("sway")], Vec::new());
  peer.request();
  peer.reply(json!({"type": "error", "error_type": "error", "description": "no"}));
  run(&mut shell);
  assert!(matches!(
    state(&greetd),
    GreetState::Failed { auth: false, .. }
  ));
  greetd.create_session("me");
  assert_eq!(peer.request(), json!({"type": "cancel_session"}));
}

#[test]
fn a_lost_connection_is_made_again_only_for_the_next_request() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  peer.request();
  peer.reply(json!({"type": "error", "error_type": "auth_error", "description": "nope"}));
  // The cancel that follows can't be written.
  peer
    .0
    .shutdown(std::net::Shutdown::Read)
    .expect("a shutdown");
  capture_logs(|| run(&mut shell));
  greetd_.listener.set_nonblocking(true).expect("nonblocking");
  assert!(greetd_.listener.accept().is_err(), "nothing to send");
  greetd_.listener.set_nonblocking(false).expect("blocking");
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  assert_eq!(
    peer.request(),
    json!({"type": "create_session", "username": "me"})
  );
}

#[test]
fn a_reply_in_pieces_is_read_whole() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  peer.request();
  let body = secret().to_string();
  let length = u32::try_from(body.len()).expect("small").to_ne_bytes();
  peer.0.write_all(&length[..2]).expect("written");
  run(&mut shell);
  peer.0.write_all(&length[2..]).expect("written");
  peer.0.write_all(&body.as_bytes()[..10]).expect("written");
  run(&mut shell);
  assert_eq!(state(&greetd), GreetState::Busy);
  peer.0.write_all(&body.as_bytes()[10..]).expect("written");
  run(&mut shell);
  assert!(matches!(state(&greetd), GreetState::Asking { .. }));
}

#[test]
fn bad_json_fails_the_attempt() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  peer.request();
  peer.0.write_all(&3u32.to_ne_bytes()).expect("written");
  peer.0.write_all(b"{x}").expect("written");
  capture_logs(|| run(&mut shell));
  assert!(matches!(
    state(&greetd),
    GreetState::Failed { auth: false, .. }
  ));
}

#[test]
fn only_the_latest_messages_are_kept() {
  let greetd_ = FakeGreetd::new();
  let mut shell = Harness::new(greetd_.config());
  let greetd = shell.cx.greetd();
  greetd.create_session("me");
  let mut peer = greetd_.accept();
  peer.request();
  for n in 0..MAX_MESSAGES + 5 {
    peer.reply(
      json!({"type": "auth_message", "auth_message_type": "info", "auth_message": n.to_string()}),
    );
    run(&mut shell);
    peer.request();
  }
  let messages = greetd.messages().get_untracked();
  assert_eq!(messages.len(), MAX_MESSAGES);
  assert_eq!(messages[0].text, "5", "the oldest go");
}

/// greetd itself and a real PAM. greetd runs this as its greeter, with
/// `GREETD_SOCK` set, and it logs `CRABSHELL_REAL_GREETD_USER` in, once
/// with a wrong password and then with `CRABSHELL_REAL_GREETD_PASSWORD`,
/// then starts `CRABSHELL_REAL_GREETD_SESSION`, a shell line, as their
/// session, which greetd runs once this exits. Never on a real desktop: the wrong
/// answer counts toward `pam_faillock`. It is for a throwaway container.
#[test]
#[ignore = "needs a throwaway system whose greetd runs this as its greeter"]
fn the_real_greetd_logs_in_and_starts_the_session() {
  let var = |name| std::env::var(name).ok().filter(|value| !value.is_empty());
  let (Some(user), Some(password), Some(session), Some(sock)) = (
    var("CRABSHELL_REAL_GREETD_USER"),
    var("CRABSHELL_REAL_GREETD_PASSWORD"),
    var("CRABSHELL_REAL_GREETD_SESSION"),
    var("GREETD_SOCK"),
  ) else {
    eprintln!("the CRABSHELL_REAL_GREETD_* variables or GREETD_SOCK are unset, so nothing ran");
    return;
  };
  let mut shell = Harness::new(Config {
    greetd: Some(PathBuf::from(sock)),
    ..offline()
  });
  let greetd = shell.cx.greetd();
  // Real PAM pauses after a wrong answer, so this waits longer than 5 s.
  let mut wait = |what: &str, like: fn(&GreetState) -> bool| {
    let give_up = Instant::now() + Duration::from_secs(30);
    while !like(&state(&greetd)) {
      let now = state(&greetd);
      assert!(Instant::now() < give_up, "no {what} after 30 s: {now:?}");
      shell.run_until(Instant::now() + Duration::from_millis(100));
    }
  };
  let asking = |state: &GreetState| matches!(state, GreetState::Asking { .. });
  greetd.create_session(&user);
  wait("question", asking);
  greetd.respond("not the password");
  wait("failure", |state| {
    matches!(state, GreetState::Failed { auth: true, .. })
  });
  greetd.create_session(&user);
  wait("second question", asking);
  greetd.respond(&password);
  wait("authentication", |state| {
    *state == GreetState::Authenticated
  });
  // greetd runs the joined command with sh, so the line goes as it is.
  greetd.start_session(vec![session], Vec::new());
  wait("start", |state| *state == GreetState::Started);
}
