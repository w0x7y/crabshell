//! The audio service against a real pipewire-pulse of its own. Ignored,
//! since they need `pipewire` and `pactl`; run them by hand:
//!
//! ```sh
//! cargo test -p crabshell-services --lib audio::tests::pipewire -- --ignored --test-threads=1
//! ```
//!
//! Each test starts its own private server, from the crate's
//! `tests/pipewire`, which crabshell's headless tests share.

use std::cell::Cell;
use std::process::Child;
use std::rc::Rc;
use std::time::Duration;

use crabshell_reactive::Runtime;

use super::{Harness, libpulse_switches, logs_of, one_at_a_time};
use crate::{AudioChannel, PeakTarget, ServicesExt};

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

/// Whether `peaks` are `expected`, give or take a little.
fn near(peaks: &[f32], expected: &[f32]) -> bool {
  let close = |(peak, expected): (&f32, &f32)| (peak - expected).abs() < 0.01;
  peaks.len() == expected.len() && peaks.iter().zip(expected).all(close)
}

fn stop(mut player: Child) {
  player.kill().expect("pacat killed");
  player.wait().expect("pacat reaped");
}

fn test_sink() -> PeakTarget {
  PeakTarget::Sink("test-sink".to_owned())
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_sink_meter_follows_a_tone_and_silence_but_not_the_volume() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("peaks");
  let mut shell = Harness::new(&server.address());
  let peaks = shell.cx.audio().peaks(test_sink());
  let tone = server.play_square(0.5, 0.25);
  shell.run_until(|_| {
    server.link();
    near(&peaks.get_untracked(), &[0.5, 0.25])
  });
  // On PipeWire, a sink's volume isn't in what its monitor records.
  server.pactl(&["set-sink-volume", "test-sink", "50%"]);
  shell.run_for(Duration::from_millis(300));
  assert!(near(&peaks.get_untracked(), &[0.5, 0.25]));
  let silence = server.play();
  stop(tone);
  shell.run_until(|_| {
    server.link();
    near(&peaks.get_untracked(), &[0.0, 0.0])
  });
  // Silence sets the peaks no more.
  let sets = Rc::new(Cell::new(0));
  shell.cx.effect({
    let sets = Rc::clone(&sets);
    move || peaks.with(|_| sets.set(sets.get() + 1))
  });
  Runtime::current().flush();
  let woke = shell.run_for(Duration::from_secs(1));
  eprintln!("a meter in silence woke the shell {woke} times in 1 s");
  assert_eq!(sets.get(), 1);
  stop(silence);
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_stream_meter_hears_only_its_stream() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("stream-peaks");
  let mut shell = Harness::new(&server.address());
  let audio = shell.cx.audio();
  let streams = audio.streams();
  let silence = server.play();
  shell.run_until(|_| streams.get_untracked().len() == 1);
  let quiet = streams.get_untracked()[0].index;
  let tone = server.play_square(0.5, 0.25);
  // Once linked, each stream's sink is known.
  shell.run_until(|_| {
    server.link();
    let streams = streams.get_untracked();
    streams.len() == 2 && streams.iter().all(|stream| stream.sink == "test-sink")
  });
  let loud = streams
    .get_untracked()
    .iter()
    .find(|s| s.index != quiet)
    .expect("the tone")
    .index;
  let (quiet, loud, sink) = (
    audio.peaks(PeakTarget::Stream(quiet)),
    audio.peaks(PeakTarget::Stream(loud)),
    audio.peaks(test_sink()),
  );
  shell.run_until(|_| {
    server.link();
    near(&quiet.get_untracked(), &[0.0, 0.0])
      && near(&loud.get_untracked(), &[0.5, 0.25])
      && near(&sink.get_untracked(), &[0.5, 0.25])
  });
  stop(tone);
  stop(silence);
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_meter_records_only_while_its_context_lives() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("meter-idle");
  let mut shell = Harness::new(&server.address());
  let audio = shell.cx.audio();
  let streams = audio.streams();
  let tone = server.play_square(0.5, 0.25);
  shell.run_until(|_| {
    server.link();
    streams
      .get_untracked()
      .first()
      .is_some_and(|s| s.sink == "test-sink")
  });
  shell.run_for(Duration::from_millis(500));
  let recordings = || server.pactl(&["list", "source-outputs", "short"]);
  assert_eq!(recordings(), "", "nothing records until a meter asks");
  let without = shell.run_for(Duration::from_secs(1));
  let cx = shell.cx.child();
  let peaks = cx.audio().peaks(test_sink());
  shell.run_until(|_| {
    server.link();
    near(&peaks.get_untracked(), &[0.5, 0.25])
  });
  let with = shell.run_for(Duration::from_secs(1));
  eprintln!("in 1 s of a tone the shell woke {without} times without a meter, {with} with one");
  assert_eq!(without, 1);
  assert!((10..=60).contains(&with), "{with} wakeups");
  cx.owner().dispose();
  shell.run_until(|_| recordings().is_empty());
  shell.run_for(Duration::from_millis(500));
  let switches = libpulse_switches();
  // One wakeup: the one that ends the run.
  assert_eq!(shell.run_for(Duration::from_secs(3)), 1);
  assert_eq!(libpulse_switches(), switches, "libpulse's thread slept");
  stop(tone);
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_sink_meter_falls_silent_when_playing_stops_and_keeps_nothing_awake() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("meter-stops");
  let mut shell = Harness::new(&server.address());
  let sinks = shell.cx.audio().sinks();
  shell.run_until(|_| sinks.get_untracked().len() == 2);
  let peaks = shell.cx.audio().peaks(test_sink());
  // Nothing plays, so it is silent, and the meter doesn't wake the sink.
  shell.run_until(|_| near(&peaks.get_untracked(), &[0.0, 0.0]));
  let tone = server.play_square(0.5, 0.25);
  shell.run_until(|_| {
    server.link();
    near(&peaks.get_untracked(), &[0.5, 0.25])
  });
  stop(tone);
  shell.run_until(|_| near(&peaks.get_untracked(), &[0.0, 0.0]));
  // Corked, so a server that sends an idle sink's silence, as PulseAudio
  // does, sends none.
  let corked = || {
    server
      .pactl(&["list", "source-outputs"])
      .contains("Corked: yes")
  };
  shell.run_until(|_| corked());
  shell.run_for(Duration::from_millis(500));
  let switches = libpulse_switches();
  // One wakeup: the one that ends the run.
  assert_eq!(shell.run_for(Duration::from_secs(2)), 1);
  assert_eq!(libpulse_switches(), switches, "libpulse's thread slept");
  // It records again once the sink plays.
  let tone = server.play_square(0.25, 0.5);
  shell.run_until(|_| {
    server.link();
    near(&peaks.get_untracked(), &[0.25, 0.5])
  });
  stop(tone);
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_source_meter_records_with_nothing_else_recording() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("source-peaks");
  let mut shell = Harness::new(&server.address());
  let peaks = shell
    .cx
    .audio()
    .peaks(PeakTarget::Source("test-source".to_owned()));
  shell.run_until(|_| {
    server.link();
    near(&peaks.get_untracked(), &[0.0, 0.0])
  });
  let woke = shell.run_for(Duration::from_secs(1));
  assert!((10..=60).contains(&woke), "{woke} wakeups");
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_meter_gone_before_the_server_made_its_stream_leaves_nothing_recording() {
  let _one = one_at_a_time();
  let server = PrivateServer::start("meter-early");
  let mut shell = Harness::new(&server.address());
  let audio = shell.cx.audio();
  let (sinks, streams) = (audio.sinks(), audio.streams());
  shell.run_until(|_| sinks.get_untracked().len() == 2);
  // The sink is idle and nothing links the source yet, so the server
  // makes neither stream.
  let cx = shell.cx.child();
  cx.audio().peaks(test_sink());
  cx.audio()
    .peaks(PeakTarget::Source("test-source".to_owned()));
  shell.run_for(Duration::from_millis(500));
  cx.owner().dispose();
  shell.run_for(Duration::from_millis(100));
  // Now the sink plays and everything is linked, so they could start.
  let tone = server.play_square(0.5, 0.25);
  shell.run_until(|_| {
    server.link();
    streams
      .get_untracked()
      .first()
      .is_some_and(|s| s.sink == "test-sink")
  });
  let recordings = || server.pactl(&["list", "source-outputs", "short"]);
  shell.run_until(|_| {
    server.link();
    recordings().is_empty()
  });
  shell.run_for(Duration::from_millis(500));
  let switches = libpulse_switches();
  // One wakeup: the one that ends the run.
  assert_eq!(shell.run_for(Duration::from_secs(2)), 1);
  assert_eq!(libpulse_switches(), switches, "libpulse's thread slept");
  assert_eq!(recordings(), "");
  stop(tone);
}
