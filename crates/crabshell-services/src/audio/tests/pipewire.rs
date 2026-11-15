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
use crate::{AudioChannel, ServicesExt};

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

#[test]
#[ignore = "needs pipewire; run by hand"]
fn sinks_sources_and_streams_show_and_follow_the_server() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("lists");
  let mut shell = Harness::new(&server.address());
  let audio = shell.cx.audio();
  let (sinks, sources, streams) = (audio.sinks(), audio.sources(), audio.streams());
  shell.run_until(|_| sinks.get_untracked().len() == 2 && sources.get_untracked().len() == 1);
  let names: Vec<String> = sinks
    .get_untracked()
    .into_iter()
    .map(|sink| sink.name)
    .collect();
  assert!(names.contains(&"test-sink".to_owned()), "{names:?}");
  let other = sinks
    .get_untracked()
    .into_iter()
    .find(|sink| sink.name == "other-sink");
  assert_eq!(
    other.map(|sink| sink.description),
    Some("Other Sink".to_owned())
  );
  // The sinks' monitors aren't listed.
  assert_eq!(sources.get_untracked()[0].name, "test-source");
  let default_source = audio.default_source();
  shell.run_until(|_| default_source.get_untracked().is_some());
  assert_eq!(
    default_source.get_untracked().map(|s| s.name),
    Some("test-source".to_owned())
  );

  // With no session manager, nothing links the stream to a sink, so
  // its sink and moves are checked against the fake server instead.
  let mut player = server.play();
  shell.run_until(|_| streams.get_untracked().len() == 1);
  let stream = streams.get_untracked()[0].clone();
  assert_eq!(stream.app_name, "pacat");
  audio.set_stream_mute(stream.index, true);
  shell.run_until(|_| streams.get_untracked().first().is_some_and(|s| s.muted));
  player.kill().expect("pacat killed");
  player.wait().expect("pacat reaped");
  shell.run_until(|_| streams.get_untracked().is_empty());
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn defaults_volumes_and_mutes_reach_the_server() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("setters");
  let mut shell = Harness::new(&server.address());
  let audio = shell.cx.audio();
  let (sink, sinks, source) = (audio.default_sink(), audio.sinks(), audio.default_source());
  shell.run_until(|_| sinks.get_untracked().len() == 2 && source.get_untracked().is_some());

  // A new default needs a session manager, which this server has none
  // of; the commands are checked against the fake server instead.
  assert_eq!(
    sink.get_untracked().map(|s| s.name),
    Some("test-sink".to_owned())
  );
  audio.set_sink_volume("test-sink", 40);
  let test_sink = move || {
    sinks
      .get_untracked()
      .into_iter()
      .find(|s| s.name == "test-sink")
  };
  shell.run_until(|_| test_sink().map(|s| s.volume_percent) == Some(40));
  assert!(server.volume().contains(" 40% "), "{}", server.volume());
  audio.set_sink_mute("test-sink", true);
  shell.run_until(|_| test_sink().is_some_and(|s| s.muted));

  audio.set_source_volume("test-source", 70);
  shell.run_until(|_| source.get_untracked().map(|s| s.volume_percent) == Some(70));
  audio.set_source_mute("test-source", true);
  shell.run_until(|_| source.get_untracked().is_some_and(|s| s.muted));
  let muted = server.pactl(&["get-source-mute", "test-source"]);
  assert_eq!(muted.trim(), "Mute: yes");
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn each_channel_shows_and_reaches_the_server() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("channels");
  server.pactl(&["set-sink-volume", "test-sink", "80%", "40%"]);
  let mut shell = Harness::new(&server.address());
  let audio = shell.cx.audio();
  let sinks = audio.sinks();
  let test_sink = move || {
    let sinks = sinks.get_untracked();
    sinks.into_iter().find(|s| s.name == "test-sink")
  };
  shell.run_until(|_| test_sink().is_some_and(|s| s.volumes == [80, 40]));
  let sink = test_sink().expect("test-sink");
  let stereo = [AudioChannel::FrontLeft, AudioChannel::FrontRight];
  assert_eq!(sink.channels, stereo);
  assert_eq!(sink.volume_percent, 60);
  audio.set_sink_volumes("test-sink", &[30, 70]);
  shell.run_until(|_| test_sink().is_some_and(|s| s.volumes == [30, 70]));
  let volume = server.volume();
  assert!(
    volume.contains(" 30% ") && volume.contains(" 70% "),
    "{volume}"
  );
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_playing_stream_wakes_nothing() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("playing");
  let mut shell = Harness::new(&server.address());
  let streams = shell.cx.audio().streams();
  let mut player = server.play();
  shell.run_until(|_| streams.get_untracked().len() == 1);
  shell.run_for(Duration::from_millis(500));
  // One wakeup: the one that ends the run.
  assert_eq!(shell.run_for(Duration::from_secs(3)), 1);
  player.kill().expect("pacat killed");
  player.wait().expect("pacat reaped");
}
