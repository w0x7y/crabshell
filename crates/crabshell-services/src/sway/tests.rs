use std::cell::Cell;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use calloop::EventLoop;
use crabshell_reactive::Runtime;
use crabshell_ui::Cx;

use super::ipc::{self, GET_WORKSPACES, RUN_COMMAND, SUBSCRIBE, WORKSPACE_EVENT};
use super::{Service, Socket, Sway, Workspace};
use crate::{Config, ServicesExt};

/// The reconnect delay in these tests.
const RETRY: Duration = Duration::from_millis(100);

/// A stand-in for Sway: a socket in a short temporary directory, since
/// socket paths are capped at 108 bytes. The directory goes on drop.
struct FakeSway {
    dir: PathBuf,
    listener: Option<UnixListener>,
}

impl FakeSway {
    fn new() -> FakeSway {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "crabshell-sway-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let mut sway = FakeSway {
            dir,
            listener: None,
        };
        sway.listen();
        sway
    }

    fn socket(&self) -> Socket {
        Socket {
            path: self.dir.join("ipc"),
            retry: RETRY,
        }
    }

    /// Starts taking connections, as Sway does when it starts.
    fn listen(&mut self) {
        let listener = UnixListener::bind(self.socket().path).expect("a socket in the directory");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking socket");
        self.listener = Some(listener);
    }

    /// Stops taking connections, as when Sway is gone.
    fn close(&mut self) {
        self.listener = None;
        std::fs::remove_file(self.socket().path).expect("the socket file");
    }

    /// The client waiting to be accepted, if one is. The service connects
    /// at once, so it is waiting by the time a test asks.
    fn try_accept(&self) -> Option<Peer> {
        let listener = self.listener.as_ref().expect("listening");
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock => return None,
            Err(error) => panic!("accept failed: {error}"),
        };
        stream.set_nonblocking(false).expect("a blocking stream");
        let timeout = Some(Duration::from_secs(1));
        stream.set_read_timeout(timeout).expect("a read timeout");
        Some(Peer(stream))
    }

    fn accept(&self) -> Peer {
        self.try_accept().expect("a client waiting")
    }
}

impl Drop for FakeSway {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Sway's end of one connection.
struct Peer(UnixStream);

impl Peer {
    /// The next message the service sent. It writes at once, so the
    /// message must be there within the read timeout.
    fn read(&mut self) -> (u32, String) {
        let mut header = [0; 14];
        let read = self.0.read_exact(&mut header);
        read.expect("a message from the service");
        assert_eq!(&header[..6], b"i3-ipc");
        let word = |at: usize| u32::from_ne_bytes(header[at..at + 4].try_into().expect("4 bytes"));
        let mut payload = vec![0; word(6) as usize];
        self.0.read_exact(&mut payload).expect("the whole payload");
        (word(10), String::from_utf8(payload).expect("UTF-8"))
    }

    /// Asserts the service sent nothing more and is still connected.
    fn read_nothing(&mut self) {
        self.0.set_nonblocking(true).expect("a non-blocking stream");
        let read = self.0.read(&mut [0; 1]).map_err(|error| error.kind());
        assert_eq!(read, Err(ErrorKind::WouldBlock));
        self.0.set_nonblocking(false).expect("a blocking stream");
    }

    /// Asserts the service closed the connection. A close with bytes left
    /// unread is a reset.
    fn read_end(&mut self) {
        match self.0.read(&mut [0; 1]) {
            Ok(0) => {}
            Err(error) if error.kind() == ErrorKind::ConnectionReset => {}
            other => panic!("still connected: {other:?}"),
        }
    }

    fn send(&mut self, kind: u32, json: &str) {
        let mut bytes = Vec::new();
        ipc::encode(kind, json.as_bytes(), &mut bytes);
        self.0.write_all(&bytes).expect("a write to the service");
    }

    /// Takes the service's opening messages and answers with `list`.
    fn greet(&mut self, list: &str) {
        self.read_opening();
        self.send(SUBSCRIBE, r#"{"success":true}"#);
        self.send(GET_WORKSPACES, list);
    }

    /// Takes the service's opening messages: the subscription first, so no
    /// change between the list and the subscription goes unseen.
    fn read_opening(&mut self) {
        assert_eq!(self.read(), (SUBSCRIBE, r#"["workspace"]"#.to_owned()));
        assert_eq!(self.read(), (GET_WORKSPACES, String::new()));
    }

    /// Sends a workspace event and takes the fetch it causes.
    fn send_event(&mut self, shell: &mut Harness) {
        self.send(WORKSPACE_EVENT, "{}");
        shell.turn();
        assert_eq!(self.read(), (GET_WORKSPACES, String::new()));
    }
}

/// A `GET_WORKSPACES` reply as Sway writes it, with fields the service
/// ignores, for workspaces on DP-1 given as `(id, name, focused)`. And the
/// list it should become.
fn reply(workspaces: &[(i64, &str, bool)]) -> (String, Vec<Workspace>) {
    let json = workspaces.iter().map(|&(id, name, focused)| {
        format!(
            r#"{{"id":{id},"type":"workspace","num":-1,"name":"{name}","rect":{{"x":0,"y":0,"width":1920,"height":1080}},"output":"DP-1","focused":{focused},"visible":{focused},"urgent":false}}"#
        )
    });
    let list = workspaces.iter().map(|&(id, name, focused)| Workspace {
        id,
        name: name.to_owned(),
        output: "DP-1".to_owned(),
        focused,
        visible: focused,
        urgent: false,
    });
    (
        format!("[{}]", json.collect::<Vec<_>>().join(",")),
        list.collect(),
    )
}

fn one() -> (String, Vec<Workspace>) {
    reply(&[(4, "1", true)])
}

fn two() -> (String, Vec<Workspace>) {
    reply(&[(4, "1", false), (7, "2:web", true)])
}

/// What a shell gives the services, with no compositor: a loop and a root
/// context.
struct Harness {
    event_loop: EventLoop<'static, ()>,
    cx: Cx,
}

impl Harness {
    fn new(sway: Option<Socket>) -> Harness {
        let event_loop = EventLoop::try_new().expect("an event loop");
        let cx = Cx::new(Runtime::current().new_owner());
        let config = Config {
            localtime: None,
            system_bus: None,
            sway,
            audio: None,
        };
        crate::provide_with(&cx, &event_loop.handle(), config);
        Harness { event_loop, cx }
    }

    /// One turn of the loop, as the shell runs it, for something the test
    /// made ready. Waits up to 1 s for it.
    fn turn(&mut self) {
        self.event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut ())
            .expect("the loop runs");
        Runtime::current().flush();
    }

    /// Runs the loop for `time`. Returns how many times it woke.
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

    /// Connects to `fake` and takes the list it sends.
    fn connect(fake: &FakeSway) -> (Harness, Peer) {
        let mut shell = Harness::new(Some(fake.socket()));
        let workspaces = shell.cx.sway().workspaces();
        let mut peer = fake.accept();
        let (list, expected) = one();
        peer.greet(&list);
        shell.turn();
        assert_eq!(workspaces.get_untracked(), expected);
        (shell, peer)
    }
}

/// Runs `f` and returns everything it logged through `tracing`.
fn capture_logs(f: impl FnOnce()) -> String {
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("log lock").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let buffer = Buffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = buffer.0.lock().expect("log lock").clone();
    String::from_utf8(bytes).expect("UTF-8 logs")
}

#[test]
fn the_list_arrives_and_each_event_fetches_it_again() {
    let fake = FakeSway::new();
    let mut shell = Harness::new(Some(fake.socket()));
    let sway = shell.cx.sway();
    let workspaces = sway.workspaces();
    let mut peer = fake.accept();
    assert_eq!(workspaces.get_untracked(), []);

    let (list, expected) = one();
    peer.greet(&list);
    shell.turn();
    assert_eq!(workspaces.get_untracked(), expected);
    assert_eq!(sway.changed_at(), None, "the first list follows no event");

    let sent = Instant::now();
    peer.send(
        WORKSPACE_EVENT,
        r#"{"change":"focus","current":{},"old":{}}"#,
    );
    shell.turn();
    assert_eq!(peer.read(), (GET_WORKSPACES, String::new()));
    let (list, expected) = two();
    let replied = Instant::now();
    peer.send(GET_WORKSPACES, &list);
    shell.turn();
    assert_eq!(workspaces.get_untracked(), expected);
    let changed_at = sway.changed_at().expect("a list fetched for an event");
    assert!(
        sent <= changed_at && changed_at < replied,
        "the event's time"
    );
    peer.read_nothing();
}

#[test]
fn events_before_the_first_list_are_answered_in_order() {
    let fake = FakeSway::new();
    let mut shell = Harness::new(Some(fake.socket()));
    let sway = shell.cx.sway();
    let mut peer = fake.accept();
    peer.read_opening();
    peer.send(SUBSCRIBE, r#"{"success":true}"#);
    peer.send_event(&mut shell);
    let between = Instant::now();
    peer.send_event(&mut shell);

    // Sway answers in the order asked: the first list, then one per event.
    let lists = [one(), two(), reply(&[(9, "3", true)])];
    let mut changed = Vec::new();
    for (list, expected) in lists {
        peer.send(GET_WORKSPACES, &list);
        shell.turn();
        assert_eq!(sway.workspaces().get_untracked(), expected);
        changed.push(sway.changed_at());
    }
    assert_eq!(changed[0], None, "the first list follows no event");
    assert!(changed[1].is_some_and(|at| at < between), "the first event");
    assert!(
        changed[2].is_some_and(|at| at > between),
        "the second event"
    );
    peer.read_nothing();
}

#[test]
fn an_unchanged_list_is_not_set_again() {
    let fake = FakeSway::new();
    let (mut shell, mut peer) = Harness::connect(&fake);
    let sway = shell.cx.sway();
    let workspaces = sway.workspaces();
    let runs = Rc::new(Cell::new(0));
    let counted = Rc::clone(&runs);
    shell.cx.effect(move || {
        workspaces.with(|_| ());
        counted.set(counted.get() + 1);
    });
    peer.send_event(&mut shell);
    peer.send(GET_WORKSPACES, &two().0);
    shell.turn();
    let (changed_at, ran) = (sway.changed_at(), runs.get());
    assert!(changed_at.is_some());

    peer.send_event(&mut shell);
    peer.send(GET_WORKSPACES, &two().0);
    shell.turn();
    assert_eq!(sway.changed_at(), changed_at);
    assert_eq!(runs.get(), ran, "the effect didn't run again");
}

#[test]
fn a_sway_that_reads_nothing_is_dropped() {
    let logs = capture_logs(|| {
        let fake = FakeSway::new();
        let (mut shell, mut peer) = Harness::connect(&fake);
        let workspaces = shell.cx.sway().workspaces();
        // Each event queues a fetch Sway never reads.
        let mut events = Vec::new();
        for _ in 0..4096 {
            ipc::encode(WORKSPACE_EVENT, b"{}", &mut events);
        }
        let mut turns = 0;
        while !workspaces.get_untracked().is_empty() {
            assert!(turns < 200, "still connected after {turns} turns");
            peer.0.write_all(&events).expect("a write to the service");
            shell.turn();
            turns += 1;
        }
    });
    assert!(logs.contains("Sway reads nothing"), "{logs}");
}

#[test]
fn messages_split_byte_by_byte_or_joined_are_read_whole() {
    let fake = FakeSway::new();
    let mut shell = Harness::new(Some(fake.socket()));
    let workspaces = shell.cx.sway().workspaces();
    let mut peer = fake.accept();
    peer.read_opening();
    peer.send(SUBSCRIBE, r#"{"success":true}"#);

    let (list, expected) = one();
    let mut bytes = Vec::new();
    ipc::encode(GET_WORKSPACES, list.as_bytes(), &mut bytes);
    for &byte in &bytes {
        assert_eq!(workspaces.get_untracked(), []);
        peer.0.write_all(&[byte]).expect("a write to the service");
        shell.turn();
    }
    assert_eq!(workspaces.get_untracked(), expected);

    // An event, a command reply and a list, in one read.
    let (list, expected) = two();
    let mut bytes = Vec::new();
    ipc::encode(WORKSPACE_EVENT, b"{}", &mut bytes);
    ipc::encode(RUN_COMMAND, br#"[{"success":true}]"#, &mut bytes);
    ipc::encode(GET_WORKSPACES, list.as_bytes(), &mut bytes);
    peer.0.write_all(&bytes).expect("a write to the service");
    shell.turn();
    assert_eq!(workspaces.get_untracked(), expected);
    assert_eq!(peer.read(), (GET_WORKSPACES, String::new()));
}

#[test]
fn every_caller_shares_one_connection() {
    let fake = FakeSway::new();
    let shell = Harness::new(Some(fake.socket()));
    let first = shell.cx.sway().workspaces();
    let output = shell.cx.child();
    assert_eq!(output.sway().workspaces(), first);
    fake.accept();
    assert!(fake.try_accept().is_none());
    // The list belongs to the root, so an output going away leaves it.
    output.owner().dispose();
    assert_eq!(first.try_get(), Some(Vec::new()));
}

#[test]
fn started_reads_the_service_without_connecting() {
    let fake = FakeSway::new();
    let mut shell = Harness::new(Some(fake.socket()));
    assert!(Sway::started(&shell.cx).is_none());
    shell.run_for(Duration::from_millis(50));
    assert!(fake.try_accept().is_none(), "nothing connected");

    shell.cx.sway();
    let mut peer = fake.accept();
    let (list, _) = one();
    peer.greet(&list);
    shell.turn();
    peer.send_event(&mut shell);
    let (list, expected) = two();
    peer.send(GET_WORKSPACES, &list);
    shell.turn();
    let started = Sway::started(&shell.cx.child()).expect("started by cx.sway()");
    assert_eq!(started.workspaces().get_untracked(), expected);
    assert_eq!(started.changed_at(), shell.cx.sway().changed_at());
    assert!(started.changed_at().is_some());
    assert!(fake.try_accept().is_none(), "still one connection");
}

#[test]
fn without_provide_the_service_is_inert() {
    let cx = Cx::new(Runtime::current().new_owner());
    let sway = cx.sway();
    assert_eq!(sway.workspaces().get_untracked(), []);
    assert_eq!(sway.changed_at(), None);
    sway.focus_workspace("1");
    cx.owner().dispose();
    assert_eq!(sway.workspaces().try_get(), None);
}

#[test]
fn without_swaysock_the_list_stays_empty_with_no_retries() {
    let logs = capture_logs(|| {
        let mut shell = Harness::new(None);
        let workspaces = shell.cx.sway().workspaces();
        shell.cx.child().sway();
        assert_eq!(shell.run_for(Duration::from_millis(300)), 1);
        assert_eq!(workspaces.get_untracked(), []);
    });
    assert_eq!(logs.matches("SWAYSOCK is unset").count(), 1, "{logs}");
}

#[test]
fn disposing_the_root_frees_the_list_and_closes_the_socket() {
    let runtime = Runtime::current();
    let before = runtime.node_count();
    let fake = FakeSway::new();
    let (mut shell, mut peer) = Harness::connect(&fake);
    shell.cx.child().sway();
    shell.cx.owner().dispose();
    assert_eq!(runtime.node_count(), before);
    // The next message finds the list gone and closes the socket for good.
    peer.send(WORKSPACE_EVENT, "{}");
    shell.turn();
    peer.read_end();
    shell.run_for(RETRY * 2);
    assert!(fake.try_accept().is_none());
}

#[test]
fn a_silent_sway_never_wakes_the_shell() {
    let fake = FakeSway::new();
    let (mut shell, _peer) = Harness::connect(&fake);
    shell.run_for(Duration::from_millis(50));
    // One wakeup: the one that ends the run.
    assert_eq!(shell.run_for(Duration::from_millis(500)), 1);
}

#[test]
fn bad_json_is_logged_and_skipped() {
    let logs = capture_logs(|| {
        let fake = FakeSway::new();
        let (mut shell, mut peer) = Harness::connect(&fake);
        let workspaces = shell.cx.sway().workspaces();
        let (_, expected) = one();
        for bad in ["[{not json", r#"[{"id":"seven"}]"#, r#"{"id":7}"#] {
            peer.send(GET_WORKSPACES, bad);
            shell.turn();
            assert_eq!(workspaces.get_untracked(), expected, "kept after {bad}");
        }
        peer.send(SUBSCRIBE, r#"{"success":false,"error":"no such event"}"#);
        let (list, expected) = two();
        peer.send(GET_WORKSPACES, &list);
        shell.turn();
        assert_eq!(workspaces.get_untracked(), expected, "still connected");
    });
    assert_eq!(logs.matches("a bad workspace list").count(), 3, "{logs}");
    assert!(logs.contains("error=no such event"), "{logs}");
}

#[test]
fn a_closed_socket_empties_the_list_until_it_reconnects() {
    let fake = FakeSway::new();
    let (mut shell, mut peer) = Harness::connect(&fake);
    let sway = shell.cx.sway();
    peer.send(WORKSPACE_EVENT, "{}");
    shell.turn();
    assert_eq!(peer.read(), (GET_WORKSPACES, String::new()));
    peer.send(GET_WORKSPACES, &two().0);
    shell.turn();
    assert!(sway.changed_at().is_some());

    // A clean close: everything sent was read.
    drop(peer);
    shell.turn();
    assert_eq!(sway.workspaces().get_untracked(), []);
    assert_eq!(sway.changed_at(), None);
    shell.run_for(RETRY / 2);
    assert!(fake.try_accept().is_none(), "no reconnect before the delay");

    shell.run_for(RETRY);
    let mut peer = fake.accept();
    let (list, expected) = two();
    peer.greet(&list);
    shell.turn();
    assert_eq!(sway.workspaces().get_untracked(), expected);

    // A reset: Sway goes with a request unread.
    peer.send(WORKSPACE_EVENT, "{}");
    shell.turn();
    drop(peer);
    shell.turn();
    assert_eq!(sway.workspaces().get_untracked(), []);
}

#[test]
fn an_oversize_message_drops_the_connection() {
    let logs = capture_logs(|| {
        let fake = FakeSway::new();
        let (mut shell, mut peer) = Harness::connect(&fake);
        let mut header = b"i3-ipc".to_vec();
        header.extend_from_slice(&(ipc::MAX_PAYLOAD as u32 + 1).to_ne_bytes());
        header.extend_from_slice(&GET_WORKSPACES.to_ne_bytes());
        peer.0.write_all(&header).expect("a write to the service");
        shell.turn();
        assert_eq!(shell.cx.sway().workspaces().get_untracked(), []);
        peer.read_end();
    });
    assert!(logs.contains("-byte cap"), "{logs}");
}

#[test]
fn a_dead_socket_is_logged_once_and_never_retried() {
    let logs = capture_logs(|| {
        let mut fake = FakeSway::new();
        fake.close();
        let mut shell = Harness::new(Some(fake.socket()));
        let workspaces = shell.cx.sway().workspaces();
        shell.cx.child().sway();
        // One wakeup: the one that ends the run.
        assert_eq!(shell.run_for(RETRY * 3 + RETRY / 2), 1);
        fake.listen();
        shell.run_for(RETRY * 2);
        assert!(fake.try_accept().is_none(), "no retry");
        assert_eq!(workspaces.get_untracked(), []);
    });
    assert_eq!(logs.matches("can't connect to Sway").count(), 1, "{logs}");
}

#[test]
fn a_drop_is_logged_once_and_retried_quietly_until_sway_is_back() {
    let logs = capture_logs(|| {
        let mut fake = FakeSway::new();
        let (mut shell, peer) = Harness::connect(&fake);
        let workspaces = shell.cx.sway().workspaces();
        fake.close();
        drop(peer);
        shell.turn();
        assert_eq!(workspaces.get_untracked(), []);
        shell.run_for(RETRY * 3 + RETRY / 2);
        fake.listen();
        shell.run_for(RETRY);
        let mut peer = fake.accept();
        let (list, expected) = one();
        peer.greet(&list);
        shell.turn();
        assert_eq!(workspaces.get_untracked(), expected);
    });
    assert_eq!(logs.matches("lost the connection").count(), 1, "{logs}");
    assert_eq!(logs.matches("can't connect").count(), 0, "{logs}");
    assert_eq!(logs.matches("connected to Sway again").count(), 1, "{logs}");
}

#[test]
fn disposing_the_root_during_an_outage_stops_the_retries() {
    let fake = FakeSway::new();
    let (mut shell, peer) = Harness::connect(&fake);
    drop(peer);
    shell.turn();
    shell.cx.owner().dispose();
    shell.run_for(RETRY * 2);
    assert!(
        fake.try_accept().is_none(),
        "no connect once the root is gone"
    );
    // The retry timer went too.
    assert_eq!(shell.run_for(RETRY * 3), 1);
}

#[test]
fn focus_workspace_sends_the_quoted_name_or_nothing() {
    let logs = capture_logs(|| {
        let fake = FakeSway::new();
        let (mut shell, mut peer) = Harness::connect(&fake);
        let sway = shell.cx.sway();
        sway.focus_workspace(r#"say "hi""#);
        let quoted = r#"workspace "say "'"'"hi"'"'"""#;
        assert_eq!(peer.read(), (RUN_COMMAND, quoted.to_owned()));
        let refused = r#"[{"success":false,"parse_error":false,"error":"No workspace"}]"#;
        peer.send(RUN_COMMAND, refused);
        shell.turn();
        sway.focus_workspace("next");
        peer.read_nothing();

        // A click while Sway is away is dropped, not sent on reconnect.
        drop(peer);
        shell.turn();
        sway.focus_workspace("1");
        shell.run_for(RETRY + RETRY / 2);
        let mut peer = fake.accept();
        peer.greet(&one().0);
        shell.turn();
        peer.read_nothing();
    });
    assert!(logs.contains("error=No workspace"), "{logs}");
    assert!(
        logs.contains("can't switch to this workspace by name"),
        "{logs}"
    );
    assert!(logs.contains("while Sway isn't connected"), "{logs}");
}

#[test]
fn a_full_socket_sends_the_rest_as_sway_reads() {
    let fake = FakeSway::new();
    let (mut shell, mut peer) = Harness::connect(&fake);
    let name = "x".repeat(64 * 1024);
    // Far more than the socket holds while Sway reads nothing.
    for _ in 0..20 {
        shell.cx.sway().focus_workspace(&name);
    }
    let service = shell.cx.service::<Service>().expect("provided");
    let queued = |service: &Service| {
        let state = service.state.borrow();
        let connection = state.as_ref().and_then(|state| state.connection.as_ref());
        connection.map(|connection| connection.outbox.len())
    };
    assert!(queued(&service) > Some(0), "the socket filled up");

    let sway = std::thread::spawn(move || {
        let read: Vec<_> = (0..20).map(|_| peer.read()).collect();
        (read, peer)
    });
    while !sway.is_finished() {
        shell.run_for(Duration::from_millis(10));
    }
    let (read, _peer) = sway.join().expect("every message, whole");
    let command = (RUN_COMMAND, format!(r#"workspace "{name}""#));
    assert!(read.iter().all(|message| *message == command));
    assert_eq!(queued(&service), Some(0));
    // With nothing left to write, nothing wakes the shell.
    assert_eq!(shell.run_for(Duration::from_millis(200)), 1);
}
