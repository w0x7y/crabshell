//! The audio service against a real pipewire-pulse of its own. Ignored,
//! since they need `pipewire` and `pactl`; run them by hand:
//!
//! ```sh
//! cargo test -p crabshell-services --lib audio::tests::pipewire -- --ignored --test-threads=1
//! ```
//!
//! Each test starts its own private server, from the crate's
//! `tests/pipewire`, which crabshell's headless tests share.

use std::time::Duration;

use super::{Harness, libpulse_switches, logs_of, one_at_a_time};
use crate::ServicesExt;

#[path = "../../../tests/pipewire/mod.rs"]
mod server;

use server::PrivateServer;

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_shown_sink_wakes_nothing_while_it_stays() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("idle");
  let mut shell = Harness::new(&server.address());
  let sink = shell.cx.audio().default_sink();
  shell.run_until(|_| sink.get_untracked().is_some());
  let shown = sink.get_untracked().expect("a sink");
  assert_eq!(shown.name, "test-sink");
  // Let anything the connect started finish.
  shell.run_for(Duration::from_millis(500));
  let switches = libpulse_switches();
  // One wakeup: the one that ends the run.
  assert_eq!(shell.run_for(Duration::from_secs(3)), 1);
  assert_eq!(libpulse_switches(), switches, "libpulse's thread slept");
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn volume_and_mute_reach_the_server() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("commands");
  server.pactl(&["set-sink-volume", "test-sink", "50%"]);
  let mut shell = Harness::new(&server.address());
  let audio = shell.cx.audio();
  let sink = audio.default_sink();
  let percent = move || sink.get_untracked().map(|sink| sink.volume_percent);
  shell.run_until(|_| percent() == Some(50));
  audio.change_volume(5);
  shell.run_until(|_| percent() == Some(55));
  assert!(server.volume().contains(" 55% "), "{}", server.volume());
  audio.toggle_mute();
  shell.run_until(|_| sink.get_untracked().is_some_and(|sink| sink.muted));
  let muted = server.pactl(&["get-sink-mute", "test-sink"]);
  assert_eq!(muted.trim(), "Mute: yes");
  audio.toggle_mute();
  shell.run_until(|_| sink.get_untracked().is_some_and(|sink| !sink.muted));
  let muted = server.pactl(&["get-sink-mute", "test-sink"]);
  assert_eq!(muted.trim(), "Mute: no");
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_fast_scroll_lands_every_step() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("scroll");
  let mut shell = Harness::new(&server.address());
  let audio = shell.cx.audio();
  let sink = audio.default_sink();
  let percent = move || sink.get_untracked().map(|sink| sink.volume_percent);
  for burst in 0..8 {
    server.pactl(&["set-sink-volume", "test-sink", "50%"]);
    shell.run_until(|_| percent() == Some(50));
    // One or two turns of the loop per wheel click, so the server's
    // replies to one step come back before the next.
    for _ in 0..8 {
      audio.change_volume(5);
      for _ in 0..=burst % 2 {
        shell.turn();
      }
    }
    shell.run_for(Duration::from_millis(500));
    assert!(
      server.volume().contains(" 90% "),
      "burst {burst}: {}",
      server.volume()
    );
    assert_eq!(percent(), Some(90), "burst {burst}");
  }
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_killed_server_gives_none_then_the_sink_again() {
  let _one = one_at_a_time();
  let mut server = PrivateServer::start("killed");
  let logs = logs_of(|| {
    let mut shell = Harness::new(&server.address());
    let sink = shell.cx.audio().default_sink();
    shell.run_until(|_| sink.get_untracked().is_some());
    server.kill_pulse();
    shell.run_until(|_| sink.get_untracked().is_none());
    let lost = shell.lost();
    // Tries every 50 ms while the server is down.
    shell.run_for(Duration::from_millis(300));
    assert!(shell.lost() >= lost + 2, "{} tries", shell.lost() - lost);
    server.start_pulse();
    shell.run_until(|_| sink.get_untracked().is_some());
  });
  assert_eq!(logs.matches("no audio server").count(), 1, "{logs}");
  assert_eq!(
    logs.matches("the audio server is back").count(),
    1,
    "{logs}"
  );
}
