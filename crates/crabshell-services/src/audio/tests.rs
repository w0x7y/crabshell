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

use super::{AudioService, Event, Info, Server, changed, percent};
use crate::{Config, ServicesExt};
use fake::{
    Client, FakePulse, GET_SERVER_INFO, GET_SINK_INFO, SET_SINK_MUTE, SET_SINK_VOLUME, SUBSCRIBE,
};

mod fake;

const NORM: u32 = Volume::NORMAL.0;

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

/// How many libpulse mainloop threads this process runs.
fn libpulse_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("procfs")
        .filter(|task| {
            let comm = task.as_ref().map(|task| task.path().join("comm"));
            comm.is_ok_and(|comm| std::fs::read_to_string(comm).is_ok_and(|c| c == "threaded-ml\n"))
        })
        .count()
}

/// What a shell gives the services, with no compositor.
struct Harness {
    event_loop: EventLoop<'static, ()>,
    cx: Cx,
}

impl Harness {
    /// Services that find the audio server at `server` and try again
    /// every 50 ms.
    fn new(server: &str) -> Harness {
        let event_loop = EventLoop::try_new().expect("an event loop");
        let cx = Cx::new(Runtime::current().new_owner());
        let config = Config {
            localtime: None,
            system_bus: None,
            sway: None,
            audio: Some(Server {
                address: Some(server.to_owned()),
                retry: Duration::from_millis(50),
            }),
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
            self.event_loop
                .dispatch(Some(left), &mut ())
                .expect("the loop runs");
            Runtime::current().flush();
            wakeups += 1;
        }
        wakeups
    }

    /// Runs the loop until libpulse's context is ready and the service
    /// has acted on it, for up to 2 s.
    fn run_until_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let ready = |shell: &Harness| {
            let pulse = shell
                .service()
                .pulse
                .borrow()
                .as_ref()
                .map(|p| p.intro.is_some());
            pulse == Some(true)
        };
        while !ready(self) {
            assert!(Instant::now() < deadline, "never ready");
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
    shell.cx.audio();
    shell.run_for(Duration::from_millis(20));
    let mut client = fake.accept();
    shell.run_until_ready();
    assert_eq!(client.command_numbers(), [SUBSCRIBE, GET_SERVER_INFO]);
    (shell, client)
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

/// A path for a socket nobody listens on, in a directory of its own. The
/// directory goes on drop.
struct NoServer(PathBuf);

impl NoServer {
    fn new(name: &str) -> NoServer {
        let dir =
            std::env::temp_dir().join(format!("crabshell-audio-{}-{name}", std::process::id()));
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
    let before = libpulse_threads();
    let mut shell = Harness::new(&NoServer::new("shared").address());
    let sink = shell.cx.audio().default_sink();
    let output = shell.cx.child();
    assert_eq!(output.audio().default_sink(), sink);
    assert_eq!(
        libpulse_threads(),
        before,
        "libpulse starts on the loop's next turn"
    );
    shell.run_for(Duration::from_millis(10));
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
    let before = libpulse_threads();
    let mut shell = Harness::new(&NoServer::new("dropped").address());
    let audio = shell.cx.audio();
    shell.run_for(Duration::from_millis(100));
    assert_eq!(libpulse_threads(), before + 1);
    drop((audio, shell));
    assert_eq!(libpulse_threads(), before);
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
    shell.send(Event::Sink(4));
    assert_eq!(client.command_numbers(), NOTHING, "another sink changed");
    shell.send(Event::Sink(3));
    assert_eq!(client.command_numbers(), [GET_SERVER_INFO]);
    shell.send(Event::Server);
    assert_eq!(client.command_numbers(), [GET_SERVER_INFO]);
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
    let sent = client.commands();
    let volumes = sent
        .iter()
        .filter(|(command, _)| *command == SET_SINK_VOLUME);
    let volumes: Vec<_> = volumes.map(|(_, rest)| fake::volumes_set(rest)).collect();
    let speakers = |volume| ("speakers".to_owned(), vec![level(volume); 2]);
    assert_eq!(volumes, [speakers(55), speakers(60)]);
    let mutes = sent.iter().filter(|(command, _)| *command == SET_SINK_MUTE);
    let mutes: Vec<_> = mutes.map(|(_, rest)| fake::mute_set(rest).1).collect();
    assert_eq!(mutes, [true, false]);
}
