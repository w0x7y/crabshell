use std::io::{self, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use calloop::EventLoop;
use crabshell_reactive::Runtime;
use crabshell_ui::Cx;

use super::{AudioService, Server};
use crate::{Config, ServicesExt};

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

    /// Runs the loop as the shell does, for `time`.
    fn run_for(&mut self, time: Duration) {
        let deadline = Instant::now() + time;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            self.event_loop
                .dispatch(Some(left), &mut ())
                .expect("the loop runs");
            Runtime::current().flush();
        }
    }

    /// How many contexts the service has dropped so far.
    fn lost(&self) -> u32 {
        let service = self.cx.service::<AudioService>().expect("provided");
        let pulse = service.pulse.borrow();
        pulse.as_ref().expect("libpulse started").generation
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
