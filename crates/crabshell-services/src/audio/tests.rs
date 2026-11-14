use std::cell::Cell;
use std::io::{self, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use calloop::EventLoop;
use crabshell_reactive::Runtime;
use crabshell_ui::Cx;
use libpulse_binding::volume::{ChannelVolumes, Volume};

use super::{AudioService, Event, Info, NodeInfo, SETTLE, Server, StreamInfo, changed, percent};
use crate::{Config, ServicesExt};
use fake::{
  Client, FakePulse, GET_SERVER_INFO, GET_SINK_INFO, GET_SINK_INFO_LIST, GET_SINK_INPUT_INFO_LIST,
  GET_SOURCE_INFO_LIST, MOVE_SINK_INPUT, SET_DEFAULT_SINK, SET_DEFAULT_SOURCE, SET_SINK_INPUT_MUTE,
  SET_SINK_INPUT_VOLUME, SET_SINK_MUTE, SET_SINK_VOLUME, SUBSCRIBE,
};

mod fake;
mod pipewire;

const NORM: u32 = Volume::NORMAL.0;

/// What the service asks for to read every sink, source and stream.
const LISTS: [u32; 3] = [
  GET_SINK_INFO_LIST,
  GET_SOURCE_INFO_LIST,
  GET_SINK_INPUT_INFO_LIST,
];

/// No commands.
const NOTHING: [u32; 0] = [];

/// The volume `pactl set-sink-volume` sets for `percent`.
fn level(percent: u32) -> u32 {
  (percent * NORM + 50) / 100
}

fn channels(volumes: &[u32]) -> ChannelVolumes {
  let mut channels = ChannelVolumes::default();
  channels.set_len(u8::try_from(volumes.len()).expect("a few channels"));
  for (channel, &volume) in channels.get_mut().iter_mut().zip(volumes) {
    *channel = Volume(volume);
  }
  channels
}

fn raw(volumes: ChannelVolumes) -> Vec<u32> {
  volumes.get().iter().map(|volume| volume.0).collect()
}

/// What `pactl get-sink-volume` prints for one channel at `volume`.
fn pactl_percent(volume: Volume) -> String {
  let printed = volume.print_verbose(false);
  let (_, percent) = printed
    .split_once(" / ")
    .expect("libpulse prints raw / percent");
  percent.trim().to_owned()
}

#[test]
fn percent_rounds_as_pactl_prints() {
  // libpulse's own printer, which pactl uses, for every volume to 150%.
  for raw in 0..=NORM * 3 / 2 {
    let volume = Volume(raw);
    assert_eq!(
      format!("{}%", percent(volume)),
      pactl_percent(volume),
      "{raw}"
    );
  }
  // 8192 is 12.5% exactly, and a half rounds up.
  assert_eq!(percent(Volume(8192)), 13);
}

#[test]
fn the_percent_is_the_average_of_the_channels() {
  let percent_of = |volumes: &[u32]| {
    let info = Info {
      index: 0,
      name: "sink".to_owned(),
      volumes: channels(volumes),
      muted: false,
    };
    info.sink().volume_percent
  };
  assert_eq!(percent_of(&[NORM, NORM / 2]), 75);
  assert_eq!(percent_of(&[NORM, 0, 0]), 33);
  assert_eq!(percent_of(&[level(40); 2]), 40);
}

#[test]
fn a_change_lands_where_pactl_then_reads() {
  for start in 0..=100 {
    for delta in [-15, -5, -1, 1, 5, 15] {
      let after = changed(channels(&[level(start); 2]), delta);
      let expected = (i64::from(start) + i64::from(delta)).clamp(0, 100);
      for &channel in after.get() {
        assert_eq!(
          pactl_percent(channel),
          format!("{expected}%"),
          "{start}%{delta:+}"
        );
      }
    }
  }
}

#[test]
fn scrolling_moves_five_percent_a_step_and_clamps() {
  let scroll = |volumes, steps: i32| changed(volumes, steps * 5);
  let mut volumes = channels(&[0, 0]);
  for step in 1..=22 {
    volumes = scroll(volumes, 1);
    assert_eq!(percent(volumes.avg()), (step * 5).min(100));
  }
  assert_eq!(raw(volumes), [NORM, NORM], "clamped at 100%");
  assert_eq!(percent(scroll(volumes, -3).avg()), 85);
  assert_eq!(
    raw(scroll(channels(&[level(3); 2]), -1)),
    [0, 0],
    "clamped at 0%"
  );
  // Above 100%, set by another app, the first step comes back to 100%.
  assert_eq!(raw(scroll(channels(&[level(150); 2]), 1)), [NORM, NORM]);
}

#[test]
fn a_change_keeps_the_balance() {
  let after = changed(channels(&[level(60), level(40)]), 10);
  assert_eq!(percent(after.avg()), 60);
  let [left, right] = raw(after)[..] else {
    panic!("two channels");
  };
  assert_eq!(left - right, level(60) - level(40));
  // Past 100% on average, the loudest channel stops at 100%.
  let after = changed(channels(&[level(100), level(90)]), 10);
  assert_eq!(raw(after), [NORM, level(95)]);
  // A channel at an end stops there and the others go on.
  assert_eq!(raw(changed(channels(&[level(98), level(90)]), 5))[0], NORM);
  assert_eq!(
    raw(changed(channels(&[level(10), 0]), -10)),
    [level(10) - level(5), 0]
  );
}

/// The tests that start libpulse run one at a time, so the one that counts
/// libpulse's threads sees only its own.
fn one_at_a_time() -> MutexGuard<'static, ()> {
  static LIBPULSE: Mutex<()> = Mutex::new(());
  LIBPULSE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The `/proc` directories of this process's libpulse mainloop threads.
fn libpulse_tasks() -> Vec<PathBuf> {
  let tasks = std::fs::read_dir("/proc/self/task").expect("procfs");
  let tasks = tasks.map(|task| task.expect("a task").path());
  let is_libpulse = |task: &PathBuf| {
    std::fs::read_to_string(task.join("comm")).is_ok_and(|comm| comm == "threaded-ml\n")
  };
  tasks.filter(is_libpulse).collect()
}

/// How many libpulse mainloop threads this process runs.
fn libpulse_threads() -> usize {
  libpulse_tasks().len()
}

/// [`libpulse_threads`] once it is `expected`, or after a second. The
/// kernel drops an exited thread from `/proc` a moment after
/// `pthread_join` returns, so a count right after libpulse stops can
/// still include it.
fn libpulse_threads_settled(expected: usize) -> usize {
  let give_up = Instant::now() + Duration::from_secs(1);
  while libpulse_threads() != expected && Instant::now() < give_up {
    std::thread::sleep(Duration::from_millis(5));
  }
  libpulse_threads()
}

/// Runs `shell`'s loop until libpulse has started, or for a second: under
/// load, a short run can end before the loop's next turn.
fn run_until_libpulse_starts(shell: &mut Harness) {
  let give_up = Instant::now() + Duration::from_secs(1);
  while libpulse_threads() == 0 && Instant::now() < give_up {
    shell.run_for(Duration::from_millis(10));
  }
}

/// How many times libpulse's threads have been switched out so far.
fn libpulse_switches() -> u64 {
  let switches = |task: PathBuf| -> u64 {
    let status = std::fs::read_to_string(task.join("status")).expect("a task status");
    let counts = status
      .lines()
      .filter(|line| line.contains("ctxt_switches:"));
    let counts = counts.map(|line| line.split_whitespace().nth(1).expect("a count"));
    counts
      .map(|count| count.parse::<u64>().expect("a number"))
      .sum()
  };
  libpulse_tasks().into_iter().map(switches).sum()
}

/// What a shell gives the services, with no compositor.
struct Harness {
  event_loop: EventLoop<'static, ()>,
  cx: Cx,
}

impl Harness {
  /// Services that find the audio server at `address` and try again
  /// every 50 ms for a minute.
  fn new(address: &str) -> Harness {
    Harness::with(Server {
      address: Some(address.to_owned()),
      retry: Duration::from_millis(50),
      retry_for: Duration::from_secs(60),
    })
  }

  fn with(server: Server) -> Harness {
    let event_loop = EventLoop::try_new().expect("an event loop");
    let cx = Cx::new(Runtime::current().new_owner());
    let config = Config {
      localtime: None,
      system_bus: None,
      session_bus: None,
      sway: None,
      audio: Some(server),
      popup_timeout: crate::notifications::POPUP_TIMEOUT,
    };
    crate::provide_with(&cx, &event_loop.handle(), config);
    Harness { event_loop, cx }
  }

  /// Runs the loop as the shell does, for `time`. Returns how many times
  /// it woke.
  fn run_for(&mut self, time: Duration) -> usize {
    let deadline = Instant::now() + time;
    let mut wakeups = 0;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
      if left.is_zero() {
        break;
      }
      self
        .event_loop
        .dispatch(Some(left), &mut ())
        .expect("the loop runs");
      Runtime::current().flush();
      wakeups += 1;
    }
    wakeups
  }

  /// One turn of the loop, as the shell runs it, for whatever comes
  /// first. Waits up to 100 ms for it.
  fn turn(&mut self) {
    self
      .event_loop
      .dispatch(Some(Duration::from_millis(100)), &mut ())
      .expect("the loop runs");
    Runtime::current().flush();
  }

  /// Runs the loop until `done`, for up to 5 s.
  fn run_until(&mut self, done: impl Fn(&Harness) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(self) {
      assert!(Instant::now() < deadline, "timed out");
      self.run_for(Duration::from_millis(10));
    }
  }

  fn service(&self) -> Rc<AudioService> {
    self.cx.service::<AudioService>().expect("provided")
  }

  /// Hands the service `event` from the current connection, as libpulse's
  /// thread would.
  fn send(&self, event: Event) {
    self.service().on_message((self.lost(), event));
  }

  /// How many contexts the service has dropped so far.
  fn lost(&self) -> u32 {
    let service = self.cx.service::<AudioService>().expect("provided");
    let pulse = service.pulse.borrow();
    pulse.as_ref().expect("libpulse started").generation
  }
}

/// A shell whose audio service is ready on `fake`, with the commands it
/// sends on Ready taken.
fn connected(fake: &FakePulse) -> (Harness, Client) {
  let mut shell = Harness::new(&fake.address());
  let client = connect(&mut shell, fake);
  (shell, client)
}

/// Lets `shell` connect to `fake` and waits until it is ready.
fn connect(shell: &mut Harness, fake: &FakePulse) -> Client {
  shell.cx.audio();
  shell.run_for(Duration::from_millis(60));
  let mut client = fake.accept();
  shell.run_until(|shell| {
    let pulse = shell
      .service()
      .pulse
      .borrow()
      .as_ref()
      .map(|p| p.intro.is_some());
    pulse == Some(true)
  });
  let expected = [[SUBSCRIBE, GET_SERVER_INFO].as_slice(), &LISTS].concat();
  assert_eq!(client.command_numbers(), expected);
  client
}

/// Tries every 20 ms for 150 ms.
fn briefly(address: &str) -> Server {
  Server {
    address: Some(address.to_owned()),
    retry: Duration::from_millis(20),
    retry_for: Duration::from_millis(150),
  }
}

/// A two-channel sink at `percent`.
fn info(index: u32, percent: u32, muted: bool) -> Info {
  Info {
    index,
    name: "speakers".to_owned(),
    volumes: channels(&[level(percent); 2]),
    muted,
  }
}

/// The volumes and mutes the service sent, in order.
fn sets(client: &mut Client) -> (Vec<(String, Vec<u32>)>, Vec<bool>) {
  let sent = client.commands();
  let volumes = sent
    .iter()
    .filter(|(command, _)| *command == SET_SINK_VOLUME);
  let mutes = sent.iter().filter(|(command, _)| *command == SET_SINK_MUTE);
  (
    volumes.map(|(_, rest)| fake::volumes_set(rest)).collect(),
    mutes.map(|(_, rest)| fake::mute_set(rest).1).collect(),
  )
}

fn at(name: &str, percent: u32) -> (String, Vec<u32>) {
  (name.to_owned(), vec![level(percent); 2])
}

/// A path for a socket nobody listens on, in a directory of its own. The
/// directory goes on drop.
struct NoServer(PathBuf);

impl NoServer {
  fn new(name: &str) -> NoServer {
    let dir = std::env::temp_dir().join(format!("crabshell-audio-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    NoServer(dir)
  }

  fn socket(&self) -> PathBuf {
    self.0.join("native")
  }

  fn address(&self) -> String {
    format!("unix:{}", self.socket().display())
  }
}

impl Drop for NoServer {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

/// Runs `f` with this thread's logs going to a buffer, and returns them.
fn logs_of(f: impl FnOnce()) -> String {
  #[derive(Clone, Default)]
  struct Buffer(Arc<Mutex<Vec<u8>>>);
  impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
      let mut buffer = self.0.lock().unwrap_or_else(PoisonError::into_inner);
      buffer.extend_from_slice(bytes);
      Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }
  let buffer = Buffer::default();
  let writer = buffer.clone();
  let logger = tracing_subscriber::fmt()
    .with_writer(move || writer.clone())
    .finish();
  tracing::subscriber::with_default(logger, f);
  let bytes = buffer.0.lock().unwrap_or_else(PoisonError::into_inner);
  String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn with_no_server_the_sink_is_none_and_it_tries_again() {
  let _one = one_at_a_time();
  let missing = NoServer::new("missing");
  let logs = logs_of(|| {
    let mut shell = Harness::new(&missing.address());
    let audio = shell.cx.audio();
    assert_eq!(audio.default_sink().get_untracked(), None);
    shell.run_for(Duration::from_millis(300));
    assert!(shell.lost() >= 3, "{} tries in 300 ms", shell.lost());
    assert_eq!(audio.default_sink().get_untracked(), None);
    audio.change_volume(5);
    audio.toggle_mute();
  });
  assert_eq!(logs.matches("no audio server").count(), 1, "{logs}");
}

#[test]
fn a_server_that_hangs_up_gives_none_and_a_retry() {
  let _one = one_at_a_time();
  let server = NoServer::new("hangs-up");
  let listener = UnixListener::bind(server.socket()).expect("a test socket");
  let logs = logs_of(|| {
    let mut shell = Harness::new(&server.address());
    let sink = shell.cx.audio().default_sink();
    // libpulse connects and waits for a reply that never comes.
    shell.run_for(Duration::from_millis(100));
    assert_eq!(shell.lost(), 0);
    drop(listener);
    shell.run_for(Duration::from_millis(200));
    assert!(shell.lost() >= 2, "{} tries", shell.lost());
    assert_eq!(sink.get_untracked(), None);
  });
  assert_eq!(logs.matches("no audio server").count(), 1, "{logs}");
}

#[test]
fn every_caller_shares_one_connection() {
  let _one = one_at_a_time();
  // Every test that starts libpulse holds the lock and stops it.
  let before = libpulse_threads_settled(0);
  assert_eq!(before, 0, "no libpulse thread outlives its test");
  let mut shell = Harness::new(&NoServer::new("shared").address());
  let sink = shell.cx.audio().default_sink();
  let output = shell.cx.child();
  assert_eq!(output.audio().default_sink(), sink);
  assert_eq!(
    libpulse_threads(),
    before,
    "libpulse starts on the loop's next turn"
  );
  run_until_libpulse_starts(&mut shell);
  assert_eq!(libpulse_threads(), before + 1);
  // It belongs to the root, so an output going away leaves it.
  output.owner().dispose();
  assert_eq!(sink.try_get(), Some(None));
}

#[test]
fn without_provide_the_sink_is_none_under_its_context() {
  let cx = Cx::new(Runtime::current().new_owner());
  let audio = cx.audio();
  assert_eq!(audio.default_sink().get(), None);
  audio.change_volume(5);
  audio.toggle_mute();
  cx.owner().dispose();
  assert_eq!(audio.default_sink().try_get(), None);
}

#[test]
fn disposing_the_root_frees_the_sink() {
  let _one = one_at_a_time();
  let runtime = Runtime::current();
  let before = runtime.node_count();
  let mut shell = Harness::new(&NoServer::new("disposed").address());
  let sink = shell.cx.audio().default_sink();
  shell.cx.child().audio();
  shell.run_for(Duration::from_millis(100));
  assert!(shell.lost() >= 1);
  shell.cx.owner().dispose();
  assert_eq!(runtime.node_count(), before);
  assert_eq!(sink.try_get(), None);
  // The next try finds the root gone and stops.
  shell.run_for(Duration::from_millis(100));
  let lost = shell.lost();
  shell.run_for(Duration::from_millis(150));
  assert_eq!(shell.lost(), lost);
}

#[test]
fn dropping_the_shell_stops_libpulse() {
  let _one = one_at_a_time();
  let before = libpulse_threads_settled(0);
  assert_eq!(before, 0, "no libpulse thread outlives its test");
  let mut shell = Harness::new(&NoServer::new("dropped").address());
  let audio = shell.cx.audio();
  run_until_libpulse_starts(&mut shell);
  assert_eq!(libpulse_threads(), before + 1);
  drop((audio, shell));
  assert_eq!(libpulse_threads_settled(before), before);
}

#[test]
fn a_message_from_a_dropped_connection_is_ignored() {
  let _one = one_at_a_time();
  let mut shell = Harness::new(&NoServer::new("stale").address());
  let sink = shell.cx.audio().default_sink();
  shell.run_for(Duration::from_millis(100));
  let lost = shell.lost();
  assert!(lost >= 1);
  let stale = (lost - 1, Event::Info(Some(info(3, 50, false))));
  shell.service().on_message(stale);
  assert_eq!(sink.get_untracked(), None);
  shell.send(Event::Info(Some(info(3, 50, false))));
  assert_eq!(
    sink.get_untracked().map(|sink| sink.volume_percent),
    Some(50)
  );
}

#[test]
fn an_unchanged_sink_is_not_set_again() {
  let _one = one_at_a_time();
  let mut shell = Harness::new(&NoServer::new("unchanged").address());
  let sink = shell.cx.audio().default_sink();
  let runs = Rc::new(Cell::new(0));
  shell.cx.effect({
    let runs = Rc::clone(&runs);
    move || {
      sink.with(|_| ());
      runs.set(runs.get() + 1);
    }
  });
  shell.run_for(Duration::from_millis(20));
  assert_eq!(runs.get(), 1);
  let reads = [
    (Some(info(3, 50, false)), 2),
    (Some(info(3, 50, false)), 2),
    (Some(info(3, 55, false)), 3),
    (Some(info(3, 55, true)), 4),
    (None, 5),
    (None, 5),
  ];
  for (read, runs_after) in reads {
    shell.send(Event::Info(read));
    Runtime::current().flush();
    assert_eq!(runs.get(), runs_after);
  }
}

#[test]
fn reads_follow_the_server_and_the_shown_sink_only() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("reads");
  let (shell, mut client) = connected(&fake);
  let sink = shell.cx.audio().default_sink();
  shell.send(Event::Default(Some("speakers".to_owned())));
  assert_eq!(client.command_numbers(), [GET_SINK_INFO]);
  shell.send(Event::Info(Some(info(3, 50, false))));
  assert_eq!(sink.get_untracked(), Some(info(3, 50, false).sink()));
  // Another sink: only the list of sinks is read again.
  shell.send(Event::Sink(4));
  assert_eq!(
    client.command_numbers(),
    [GET_SINK_INFO_LIST],
    "another sink changed"
  );
  shell.send(Event::Sink(3));
  assert_eq!(
    client.command_numbers(),
    [GET_SERVER_INFO, GET_SINK_INFO_LIST]
  );
  shell.send(Event::Server);
  let expected = [[GET_SERVER_INFO].as_slice(), &LISTS].concat();
  assert_eq!(client.command_numbers(), expected);
  shell.send(Event::Source);
  assert_eq!(client.command_numbers(), [GET_SOURCE_INFO_LIST]);
  shell.send(Event::Stream);
  assert_eq!(client.command_numbers(), [GET_SINK_INPUT_INFO_LIST]);
  shell.send(Event::Default(None));
  assert_eq!(sink.get_untracked(), None);
  assert_eq!(client.command_numbers(), NOTHING);
}

#[test]
fn ready_is_acted_on_once() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("ready");
  let (shell, mut client) = connected(&fake);
  // Several queued state messages can each find the context ready.
  shell.send(Event::State);
  shell.send(Event::State);
  assert_eq!(client.command_numbers(), NOTHING, "no second subscription");
}

#[test]
fn a_second_step_before_any_reply_builds_on_the_first() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("steps");
  let (shell, mut client) = connected(&fake);
  let audio = shell.cx.audio();
  shell.send(Event::Info(Some(info(3, 50, false))));
  audio.change_volume(5);
  audio.change_volume(5);
  audio.toggle_mute();
  audio.toggle_mute();
  let (volumes, mutes) = sets(&mut client);
  assert_eq!(volumes, [at("speakers", 55), at("speakers", 60)]);
  assert_eq!(mutes, [true, false]);
}

#[test]
fn a_disconnect_leaves_no_message_behind() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("disconnect");
  let (mut shell, _client) = connected(&fake);
  let service = shell.service();
  let mut started = service.pulse.borrow_mut();
  started.as_mut().expect("started").disconnect();
  drop(started);
  // One wakeup: the one that ends the run.
  assert_eq!(shell.run_for(Duration::from_millis(100)), 1);
}

#[test]
fn a_stale_read_between_steps_is_not_built_on() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("stale-read");
  let (shell, mut client) = connected(&fake);
  let audio = shell.cx.audio();
  shell.send(Event::Info(Some(info(3, 50, false))));
  // pipewire-pulse answers a read sent after a set with the sink as it
  // was before the set.
  audio.change_volume(5);
  shell.send(Event::Info(Some(info(3, 50, false))));
  audio.change_volume(5);
  audio.toggle_mute();
  shell.send(Event::Info(Some(info(3, 50, false))));
  audio.toggle_mute();
  let (volumes, mutes) = sets(&mut client);
  assert_eq!(volumes, [at("speakers", 55), at("speakers", 60)]);
  assert_eq!(mutes, [true, false]);
}

#[test]
fn an_echo_another_sink_or_time_ends_the_last_step_standing_in() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("settle");
  let (shell, mut client) = connected(&fake);
  let audio = shell.cx.audio();
  shell.send(Event::Info(Some(info(3, 50, false))));
  audio.change_volume(5);
  // The server echoes the step, then another app sets 30%.
  shell.send(Event::Info(Some(info(3, 55, false))));
  shell.send(Event::Info(Some(info(3, 30, false))));
  audio.change_volume(5);
  // The default sink changes.
  let headphones = |percent| Info {
    name: "headphones".to_owned(),
    ..info(4, percent, false)
  };
  shell.send(Event::Info(Some(headphones(20))));
  audio.change_volume(5);
  // A read with no echo wins once the step has had time to land.
  shell.send(Event::Info(Some(headphones(10))));
  std::thread::sleep(SETTLE);
  audio.change_volume(5);
  let (volumes, _) = sets(&mut client);
  let expected = [
    at("speakers", 55),
    at("speakers", 35),
    at("headphones", 25),
    at("headphones", 15),
  ];
  assert_eq!(volumes, expected);
}

#[test]
fn it_stops_trying_after_a_while_and_then_wakes_nothing() {
  let _one = one_at_a_time();
  let missing = NoServer::new("gives-up");
  let logs = logs_of(|| {
    let mut shell = Harness::with(briefly(&missing.address()));
    let sink = shell.cx.audio().default_sink();
    shell.run_for(Duration::from_millis(300));
    let tries = shell.lost();
    assert!((2..=9).contains(&tries), "{tries} tries in 150 ms");
    let switches = libpulse_switches();
    // One wakeup: the one that ends the run.
    assert_eq!(shell.run_for(Duration::from_millis(500)), 1);
    assert_eq!(libpulse_switches(), switches, "libpulse's thread slept");
    assert_eq!(shell.lost(), tries);
    assert_eq!(sink.get_untracked(), None);
  });
  assert_eq!(logs.matches("no audio server").count(), 1, "{logs}");
  assert_eq!(logs.matches("stopped trying").count(), 1, "{logs}");
}

#[test]
fn each_drop_of_a_working_connection_gets_a_new_while() {
  let _one = one_at_a_time();
  let mut fake = FakePulse::new("drops");
  let logs = logs_of(|| {
    let mut shell = Harness::with(briefly(&fake.address()));
    let client = connect(&mut shell, &fake);
    // Well past the while that began at startup.
    shell.run_for(Duration::from_millis(200));
    fake.close();
    drop(client);
    shell.run_for(Duration::from_millis(60));
    assert!(shell.lost() >= 2, "{} tries after the drop", shell.lost());
    fake.listen();
    let client = connect(&mut shell, &fake);
    shell.run_for(Duration::from_millis(200));
    fake.close();
    drop(client);
    shell.run_for(Duration::from_millis(300));
    let tries = shell.lost();
    assert_eq!(shell.run_for(Duration::from_millis(300)), 1);
    assert_eq!(shell.lost(), tries);
  });
  assert_eq!(logs.matches("no audio server").count(), 2, "{logs}");
  assert_eq!(
    logs.matches("the audio server is back").count(),
    1,
    "{logs}"
  );
  assert_eq!(logs.matches("stopped trying").count(), 1, "{logs}");
}

#[test]
fn new_defaults_are_asked_of_the_server_by_name() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("defaults");
  let (shell, mut client) = connected(&fake);
  let audio = shell.cx.audio();
  audio.set_default_sink("headphones");
  audio.set_default_source("microphone");
  let sent = client.commands();
  let named = |number| {
    sent
      .iter()
      .find(|(command, _)| *command == number)
      .map(|(_, rest)| String::from_utf8_lossy(rest).into_owned())
  };
  assert!(named(SET_DEFAULT_SINK).is_some_and(|rest| rest.contains("headphones")));
  assert!(named(SET_DEFAULT_SOURCE).is_some_and(|rest| rest.contains("microphone")));
}

#[test]
fn stream_commands_name_their_stream_and_an_unknown_one_sends_nothing() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("streams");
  let (shell, mut client) = connected(&fake);
  let audio = shell.cx.audio();
  shell.send(Event::Streams(vec![StreamInfo {
    index: 7,
    app_name: "player".to_owned(),
    title: "a song".to_owned(),
    icon_name: String::new(),
    sink: 3,
    volumes: channels(&[level(50); 2]),
    muted: false,
  }]));
  let streams = audio.streams().get_untracked();
  assert_eq!(streams.len(), 1);
  assert_eq!(streams[0].volume_percent, 50);
  audio.set_stream_volume(7, 30);
  audio.set_stream_mute(7, true);
  audio.move_stream(7, "headphones");
  audio.set_stream_volume(8, 30);
  let sent = client.commands();
  let numbers: Vec<u32> = sent.iter().map(|(command, _)| *command).collect();
  assert_eq!(
    numbers,
    [SET_SINK_INPUT_VOLUME, SET_SINK_INPUT_MUTE, MOVE_SINK_INPUT]
  );
  let moved = String::from_utf8_lossy(&sent[2].1).into_owned();
  assert!(moved.contains("headphones"), "{moved}");
}

fn node_info(index: u32, name: &str, volumes: &[u32], monitor: bool) -> NodeInfo {
  NodeInfo {
    index,
    name: name.to_owned(),
    description: name.to_owned(),
    volumes: channels(volumes),
    muted: false,
    monitor,
  }
}

#[test]
fn a_stream_names_its_sink_and_a_monitor_can_be_the_default_source() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("names");
  let (shell, _client) = connected(&fake);
  let audio = shell.cx.audio();
  shell.send(Event::Sinks(vec![node_info(
    3, "speakers", &[NORM; 2], false,
  )]));
  shell.send(Event::Streams(vec![StreamInfo {
    index: 7,
    app_name: "player".to_owned(),
    title: String::new(),
    icon_name: String::new(),
    sink: 3,
    volumes: channels(&[NORM; 2]),
    muted: false,
  }]));
  assert_eq!(audio.streams().get_untracked()[0].sink, "speakers");

  // Recording what plays: the default source is the sink's monitor,
  // which the list leaves out.
  shell.send(Event::Sources(vec![
    node_info(1, "microphone", &[NORM; 2], false),
    node_info(2, "speakers.monitor", &[NORM; 2], true),
  ]));
  shell.send(Event::DefaultSource(Some("speakers.monitor".to_owned())));
  let names: Vec<String> = audio
    .sources()
    .get_untracked()
    .into_iter()
    .map(|s| s.name)
    .collect();
  assert_eq!(names, ["microphone"]);
  let default = audio.default_source().get_untracked().map(|s| s.name);
  assert_eq!(default, Some("speakers.monitor".to_owned()));
}

#[test]
fn setting_a_volume_keeps_the_balance() {
  // 80% left and 100% right, an average of 90%, set to 50%.
  let set = super::at(channels(&[level(80), level(100)]), 50);
  let percents: Vec<u32> = raw(set).into_iter().map(|v| percent(Volume(v))).collect();
  assert_eq!(percents, [40, 60]);
}
