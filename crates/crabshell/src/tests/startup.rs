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
  let mut said = [0_u8; 6];
  supervisor
    .read_exact(&mut said)
    .expect("the shell says ready");
  assert_eq!(&said, b"ready\n");
}
