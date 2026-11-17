//! The supervisor's control socket, on a plain calloop loop: `save`
//! reaches the shell, junk is skipped, and a hang-up stops listening.

use std::cell::Cell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use super::capture_logs;
use crate::control;

fn run(event_loop: &mut calloop::EventLoop<'static, ()>) {
  for _ in 0..5 {
    event_loop
      .dispatch(Some(Duration::from_millis(20)), &mut ())
      .expect("the loop runs");
  }
}

#[test]
fn save_reaches_the_shell_and_anything_else_is_skipped() {
  let mut event_loop = calloop::EventLoop::try_new().expect("a loop");
  let (mut supervisor, shell) = UnixStream::pair().expect("a socket pair");
  let saves = Rc::new(Cell::new(0));
  let counted = Rc::clone(&saves);
  control::listen(&event_loop.handle(), shell, move || {
    counted.set(counted.get() + 1);
  });
  let logs = capture_logs(|| {
    // A command split over two writes still counts once.
    supervisor.write_all(b"sa").expect("written");
    run(&mut event_loop);
    supervisor.write_all(b"ve\nreload\n").expect("written");
    run(&mut event_loop);
    // And a command after the junk one still counts.
    supervisor.write_all(b"save\n").expect("written");
    run(&mut event_loop);
  });
  assert_eq!(saves.get(), 2);
  assert!(
    logs.contains("the supervisor sent something unknown"),
    "{logs}"
  );
  assert!(logs.contains("reload"), "{logs}");

  // The supervisor hangs up: the shell stops listening and runs on.
  drop(supervisor);
  let logs = capture_logs(|| run(&mut event_loop));
  assert!(logs.contains("the supervisor hung up"), "{logs}");
  assert_eq!(saves.get(), 2);
}

#[test]
fn ready_is_one_line() {
  let (mut supervisor, mut shell) = UnixStream::pair().expect("a socket pair");
  control::send_ready(&mut shell);
  drop(shell);
  let mut said = String::new();
  supervisor.read_to_string(&mut said).expect("read");
  assert_eq!(said, "ready\n");
}

#[test]
fn ready_means_every_shown_panel_drew_and_one_did_at_least() {
  assert!(control::ready([(true, 1), (true, 3)], false));
  assert!(
    !control::ready([(true, 1), (true, 0)], false),
    "one still to draw"
  );
  // Waiting for its output, or refused: not shown, and not waited for.
  assert!(control::ready([(true, 1), (false, 0)], false));
  assert!(!control::ready([(false, 0)], false), "nothing on screen");
  assert!(!control::ready([], false), "no panel");
}

#[test]
fn once_started_nothing_shown_is_nothing_to_wait_for() {
  assert!(control::ready([], true), "no panel");
  assert!(control::ready([(false, 0)], true), "a hidden one");
  assert!(!control::ready([(true, 0)], true), "one still to draw");
}
