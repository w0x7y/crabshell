//! The supervisor's control socket, on a plain calloop loop: commands
//! reach the shell, junk is skipped, and a hang-up stops listening.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use super::capture_logs;
use crate::control::{self, Command};

fn run(event_loop: &mut calloop::EventLoop<'static, ()>) {
  for _ in 0..5 {
    event_loop
      .dispatch(Some(Duration::from_millis(20)), &mut ())
      .expect("the loop runs");
  }
}

#[test]
fn commands_reach_the_shell_and_anything_else_is_skipped() {
  let mut event_loop = calloop::EventLoop::try_new().expect("a loop");
  let (mut supervisor, shell) = UnixStream::pair().expect("a socket pair");
  let heard = Rc::new(RefCell::new(Vec::new()));
  let sink = Rc::clone(&heard);
  control::listen(&event_loop.handle(), shell, move |command| {
    sink.borrow_mut().push(command);
  });
  let logs = capture_logs(|| {
    // A command split over two writes still counts once.
    supervisor.write_all(b"sa").expect("written");
    run(&mut event_loop);
    supervisor.write_all(b"ve\nreload\n").expect("written");
    run(&mut event_loop);
    // And a command after the junk one still counts.
    supervisor.write_all(b"handoff\nexit\n").expect("written");
    run(&mut event_loop);
  });
  assert_eq!(
    *heard.borrow(),
    [Command::Save, Command::Handoff, Command::Exit]
  );
  assert!(
    logs.contains("the supervisor sent something unknown"),
    "{logs}"
  );
  assert!(logs.contains("reload"), "{logs}");

  // The supervisor hangs up: the shell stops listening and runs on.
  drop(supervisor);
  let logs = capture_logs(|| run(&mut event_loop));
  assert!(logs.contains("the supervisor hung up"), "{logs}");
  assert_eq!(heard.borrow().len(), 3);
}

#[test]
fn ready_is_one_line() {
  let (mut supervisor, shell) = UnixStream::pair().expect("a socket pair");
  control::send(&shell, "ready");
  drop(shell);
  let mut said = String::new();
  supervisor.read_to_string(&mut said).expect("read");
  assert_eq!(said, "ready\n");
}

#[test]
fn ready_means_every_shown_panel_drew_and_one_did_at_least() {
  assert!(control::ready([(true, 1), (true, 3)], false, false));
  assert!(
    !control::ready([(true, 1), (true, 0)], false, false),
    "one still to draw"
  );
  // Waiting for its output, or refused: not shown, and not waited for.
  assert!(control::ready([(true, 1), (false, 0)], false, false));
  assert!(
    !control::ready([(false, 0)], false, false),
    "nothing on screen"
  );
  assert!(!control::ready([], false, false), "no panel");
}

#[test]
fn once_started_nothing_shown_is_nothing_to_wait_for() {
  assert!(control::ready([], true, false), "no panel");
  assert!(control::ready([(false, 0)], true, false), "a hidden one");
  assert!(
    !control::ready([(true, 0)], true, false),
    "one still to draw"
  );
}

#[test]
fn a_shell_taking_notifications_over_isnt_ready_yet() {
  assert!(!control::ready([(true, 1)], true, true));
  assert!(!control::ready([], true, true));
}
