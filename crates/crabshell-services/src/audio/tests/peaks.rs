//! Peak meters against the fake server, which sends no sound: what opens,
//! when, and what closes. The private-server tests check the peaks.

use std::cell::Cell;
use std::rc::Rc;

use crabshell_reactive::Runtime;

use super::fake::{CREATE_RECORD_STREAM, Client, FakePulse};
use super::{
  Harness, NORM, NOTHING, connect, connected, info, logs_of, node_info, one_at_a_time, stream_info,
};
use crate::ServicesExt;
use crate::audio::{Event, NodeInfo, PeakTarget, StreamInfo};

/// Each record stream the service asked for since the last call, as text
/// with the source it names.
fn recorded(client: &mut Client) -> Vec<String> {
  let sent = client.commands();
  let made = sent
    .iter()
    .filter(|(command, _)| *command == CREATE_RECORD_STREAM);
  made
    .map(|(_, rest)| String::from_utf8_lossy(rest).into_owned())
    .collect()
}

/// How many record streams the service holds, open or ended.
fn open(shell: &Harness) -> usize {
  let service = shell.service();
  let pulse = service.pulse.borrow();
  pulse.as_ref().map_or(0, |pulse| pulse.meters.len())
}

fn sink(name: &str) -> Option<PeakTarget> {
  Some(PeakTarget::Sink(name.to_owned()))
}

/// A server with speakers, a microphone and a stream to the speakers.
fn with_nodes(name: &str) -> (FakePulse, Harness, Client) {
  let fake = FakePulse::new(name);
  let (shell, mut client) = connected(&fake);
  shell.send(Event::Sinks(vec![node_info(
    3, "speakers", &[NORM; 2], false,
  )]));
  let microphone = node_info(1, "microphone", &[NORM; 2], false);
  shell.send(Event::Sources(vec![microphone]));
  shell.send(Event::Streams(vec![stream_info(7, 3, &[NORM; 2])]));
  assert_eq!(client.command_numbers(), NOTHING, "nothing records unasked");

  (fake, shell, client)
}

#[test]
fn nothing_records_until_a_meter_asks_and_then_once_its_sink_is_there() {
  let _one = one_at_a_time();
  let (_fake, shell, mut client) = with_nodes("meter-waits");
  let peaks = shell
    .cx
    .audio()
    .peaks(PeakTarget::Sink("headphones".to_owned()));
  Runtime::current().flush();
  assert_eq!(client.command_numbers(), NOTHING, "no such sink yet");
  shell.send(Event::Sinks(vec![
    node_info(3, "speakers", &[NORM; 2], false),
    node_info(4, "headphones", &[NORM; 2], false),
  ]));
  let sent = client.commands();
  assert_eq!(sent.len(), 1);
  let (command, rest) = &sent[0];
  assert_eq!(*command, CREATE_RECORD_STREAM);
  assert!(String::from_utf8_lossy(rest).contains("headphones.monitor"));
  // 32-bit floats, as many channels as the sink, 25 a second.
  let float = if cfg!(target_endian = "little") { 5 } else { 6 };
  let spec = [b'a', float, 2, 0, 0, 0, 25];
  assert!(rest.windows(spec.len()).any(|w| w == spec), "{rest:?}");
  assert_eq!(peaks.get_untracked(), Vec::<f32>::new());
  // Another read of the same sinks opens nothing more.
  shell.send(Event::Sinks(vec![
    node_info(3, "speakers", &[NORM; 2], false),
    node_info(4, "headphones", &[NORM; 2], false),
  ]));
  assert_eq!(client.command_numbers(), NOTHING);
}

#[test]
fn a_meter_follows_its_target_and_closes_with_its_context() {
  let _one = one_at_a_time();
  let (_fake, shell, mut client) = with_nodes("meter-moves");
  let runtime = Runtime::current();
  let before = runtime.node_count();
  let cx = shell.cx.child();
  let which = cx.signal(sink("speakers"));
  let peaks = cx.audio().peaks(move || which.get());
  runtime.flush();
  let made = recorded(&mut client);
  assert!(
    made.len() == 1 && made[0].contains("speakers.monitor"),
    "{made:?}"
  );
  which.set(Some(PeakTarget::Source("microphone".to_owned())));
  runtime.flush();
  let made = recorded(&mut client);
  assert!(
    made.len() == 1 && made[0].contains("microphone"),
    "{made:?}"
  );
  assert_eq!(open(&shell), 1, "the sink's meter closed");
  // A stream's meter records its sink's monitor, kept to stream 7.
  which.set(Some(PeakTarget::Stream(7)));
  runtime.flush();
  let sent = client.commands();
  let rest = &sent[0].1;
  assert!(String::from_utf8_lossy(rest).contains("speakers.monitor"));
  assert!(rest.windows(5).any(|w| w == [b'L', 0, 0, 0, 7]), "{rest:?}");
  which.set(None);
  runtime.flush();
  assert_eq!(open(&shell), 0);
  assert!(shell.service().meters.borrow().is_empty());
  which.set(sink("speakers"));
  runtime.flush();
  assert_eq!(recorded(&mut client).len(), 1);
  cx.owner().dispose();
  assert_eq!(open(&shell), 0);
  assert!(shell.service().meters.borrow().is_empty());
  assert_eq!(runtime.node_count(), before);
  assert_eq!(peaks.try_get(), None);
}

#[test]
fn a_meter_on_the_default_sink_stays_through_a_volume_step() {
  let _one = one_at_a_time();
  let (_fake, shell, mut client) = with_nodes("meter-stays");
  let runtime = Runtime::current();
  let audio = shell.cx.audio();
  let default = audio.default_sink();
  let peaks = audio.peaks(move || default.get().map(|s| PeakTarget::Sink(s.name)));
  shell.send(Event::Info(Some(info(3, 50, false))));
  runtime.flush();
  assert_eq!(recorded(&mut client).len(), 1);
  shell.service().meters.borrow()[0].peaks.set(vec![0.5, 0.5]);
  // The default sink's volume and mute change, but not its name.
  shell.send(Event::Info(Some(info(3, 55, false))));
  shell.send(Event::Info(Some(info(3, 55, true))));
  runtime.flush();
  assert_eq!(recorded(&mut client), Vec::<String>::new(), "not reopened");
  assert_eq!(peaks.get_untracked(), [0.5, 0.5], "not blanked");
}

#[test]
fn a_meter_does_not_follow_the_peaks_it_sets() {
  let _one = one_at_a_time();
  let (_fake, shell, mut client) = with_nodes("meter-untracked");
  let runtime = Runtime::current();
  // Idle, so opening its meter shows silence from inside the effect.
  let idle = NodeInfo {
    running: false,
    ..node_info(3, "speakers", &[NORM; 2], false)
  };
  shell.send(Event::Sinks(vec![idle]));
  let peaks = shell.cx.audio().peaks(move || sink("speakers"));
  runtime.flush();
  assert_eq!(peaks.get_untracked(), [0.0, 0.0]);
  assert_eq!(recorded(&mut client).len(), 1);
  // As peaks arrive.
  let peaks = shell.service().meters.borrow()[0].peaks;
  peaks.set(vec![0.5, 0.5]);
  runtime.flush();
  assert_eq!(
    recorded(&mut client),
    Vec::<String>::new(),
    "the meter stayed"
  );
}

#[test]
fn an_effect_that_sets_volumes_and_opens_a_meter_runs_once() {
  let _one = one_at_a_time();
  let (_fake, shell, _client) = with_nodes("meter-effect");
  let runtime = Runtime::current();
  let runs = Rc::new(Cell::new(0));
  let audio = shell.cx.audio();
  shell.cx.effect({
    let runs = Rc::clone(&runs);
    move || {
      runs.set(runs.get() + 1);
      audio.set_sink_volumes("speakers", &[50, 40]);
      audio.set_source_volumes("microphone", &[50, 40]);
      audio.set_stream_volumes(7, &[50, 40]);
      audio.set_sink_volume("speakers", 50);
      audio.change_volume(5);
      audio.toggle_mute();
      audio.peaks(PeakTarget::Sink("speakers".to_owned()));
    }
  });
  runtime.flush();
  // The server answers with new volumes, and peaks arrive.
  shell.send(Event::Sinks(vec![node_info(
    3,
    "speakers",
    &[NORM / 2; 2],
    false,
  )]));
  shell.service().meters.borrow()[0].peaks.set(vec![0.5, 0.5]);
  runtime.flush();
  assert_eq!(runs.get(), 1);
}

#[test]
fn a_panicking_target_is_logged_and_the_next_change_tries_again() {
  let _one = one_at_a_time();
  let (_fake, shell, mut client) = with_nodes("meter-panics");
  let runtime = Runtime::current();
  let logs = logs_of(|| {
    let which = shell.cx.signal(false);
    let peaks = shell.cx.audio().peaks(move || {
      assert!(which.get(), "no target yet");
      sink("speakers")
    });
    runtime.flush();
    assert_eq!(peaks.get_untracked(), Vec::<f32>::new());
    assert_eq!(client.command_numbers(), NOTHING);
    which.set(true);
    runtime.flush();
    assert_eq!(recorded(&mut client).len(), 1);
  });
  let logged = "cx.audio().peaks (called at crates/crabshell-services/src/audio/tests/peaks.rs";
  assert!(logs.contains(logged), "{logs}");
  assert!(
    logs.contains("panicked choosing its target: no target yet"),
    "{logs}"
  );
}

#[test]
fn a_meter_records_again_after_a_reconnect() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("meter-reconnects");
  let (mut shell, mut client) = connected(&fake);
  let peaks = shell
    .cx
    .audio()
    .peaks(PeakTarget::Sink("speakers".to_owned()));
  Runtime::current().flush();
  shell.send(Event::Sinks(vec![node_info(
    3, "speakers", &[NORM; 2], false,
  )]));
  assert_eq!(recorded(&mut client).len(), 1);
  // The server hangs up, and the service tries again in 50 ms.
  drop(client);
  shell.run_until(|shell| open(shell) == 0);
  assert_eq!(peaks.get_untracked(), Vec::<f32>::new());
  let mut client = connect(&mut shell, &fake);
  shell.send(Event::Sinks(vec![node_info(
    3, "speakers", &[NORM; 2], false,
  )]));
  assert_eq!(recorded(&mut client).len(), 1);
}

#[test]
fn an_idle_sink_or_a_paused_stream_shows_silence() {
  let _one = one_at_a_time();
  let (_fake, shell, _client) = with_nodes("meter-quiet");
  let audio = shell.cx.audio();
  let sink = audio.peaks(PeakTarget::Sink("speakers".to_owned()));
  let stream = audio.peaks(PeakTarget::Stream(7));
  Runtime::current().flush();
  let nothing_yet = Vec::<f32>::new();
  assert_eq!(stream.get_untracked(), nothing_yet);
  // Neither sends anything more, so the last peaks would stay.
  let paused = StreamInfo {
    corked: true,
    ..stream_info(7, 3, &[NORM; 2])
  };
  shell.send(Event::Streams(vec![paused]));
  assert_eq!(stream.get_untracked(), [0.0, 0.0]);
  assert_eq!(sink.get_untracked(), nothing_yet, "the sink still plays");
  let idle = NodeInfo {
    running: false,
    ..node_info(3, "speakers", &[NORM; 2], false)
  };
  shell.send(Event::Sinks(vec![idle]));
  assert_eq!(sink.get_untracked(), [0.0, 0.0]);
}

#[test]
fn each_peak_is_its_channels_loudest_sample_at_most_one() {
  let frames = [[0.25_f32, -0.5], [-0.75, 0.125], [f32::NAN, 2.0]];
  let mut bytes: Vec<u8> = frames
    .iter()
    .flatten()
    .flat_map(|s| s.to_ne_bytes())
    .collect();
  // Half a frame, left out.
  bytes.extend(0.9_f32.to_ne_bytes());
  let mut peaks = vec![0.0; 2];
  crate::audio::peaks::louder(&mut peaks, &bytes);
  assert_eq!(peaks, [0.75, 1.0]);
}
