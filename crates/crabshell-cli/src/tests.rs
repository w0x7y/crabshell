//! The command line and the template, without running anything.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::build::{Built, Report, read};
use super::ipc::{self, Ask};
use super::notify::escape;
use super::policy::{Binary, Exit, Next, Policy};
use super::watch::relevant;
use super::{FROM_CRATES_IO, TEMPLATE_MANIFEST, Task, parse, toml_string};

fn args(line: &str) -> Vec<String> {
  line.split_whitespace().map(String::from).collect()
}

#[test]
fn commands_parse_and_anything_else_is_refused() {
  assert_eq!(parse(&args("init")), Ok(Task::Init { path: None }));
  assert_eq!(
    parse(&args("init --path /src/crabshell")),
    Ok(Task::Init {
      path: Some(PathBuf::from("/src/crabshell"))
    })
  );
  assert_eq!(parse(&args("check")), Ok(Task::Check));
  assert_eq!(parse(&args("run")), Ok(Task::Run));
  assert_eq!(
    parse(&args("run now")),
    Err(String::from("can't make sense of `run now`"))
  );
  assert_eq!(parse(&args("--help")), Ok(Task::Help));
  assert_eq!(
    parse(&args("help init")),
    Err(String::from("can't make sense of `help init`"))
  );
  assert_eq!(parse(&args("")), Err(String::from("no command given")));
  assert_eq!(
    parse(&args("init --force")),
    Err(String::from("can't make sense of `init --force`"))
  );
  assert_eq!(
    parse(&args("build")),
    Err(String::from("unknown command `build`"))
  );
}

#[test]
fn a_path_becomes_a_toml_string() {
  let quoted = |path: &str| toml_string(Path::new(path)).expect("UTF-8");
  assert_eq!(quoted("/home/a b/x"), r#""/home/a b/x""#);
  assert_eq!(quoted(r#"/odd"\name"#), r#""/odd\"\\name""#);
  assert_eq!(quoted("/new\nline\ttab"), "\"/new\\u000Aline\ttab\"");
  use std::os::unix::ffi::OsStrExt;
  let not_utf8 = Path::new(std::ffi::OsStr::from_bytes(b"/src/\xff"));
  assert_eq!(toml_string(not_utf8), None);
}

#[test]
fn the_template_has_the_line_path_replaces() {
  assert_eq!(TEMPLATE_MANIFEST.matches(FROM_CRATES_IO).count(), 1);
}

fn secs(secs: u64) -> Duration {
  Duration::from_secs(secs)
}

fn restart(after: u64, binary: Binary) -> Next {
  Next::Restart {
    after: secs(after),
    binary,
  }
}

#[test]
fn a_status_reads_as_an_exit() {
  assert_eq!(Exit::from_code(Some(0)), Exit::Clean);
  assert_eq!(Exit::from_code(Some(2)), Exit::NoCompositor);
  assert_eq!(Exit::from_code(Some(1)), Exit::Crashed);
  assert_eq!(Exit::from_code(Some(101)), Exit::Crashed);
  assert_eq!(Exit::from_code(None), Exit::Crashed, "a signal");
}

#[test]
fn a_clean_exit_or_a_lost_compositor_stops() {
  let start = Instant::now();
  for exit in [Exit::Clean, Exit::NoCompositor] {
    let mut policy = Policy::new(Binary::Build);
    policy.started(start);
    assert_eq!(policy.exited(start + secs(1), exit, true), Next::Stop);
  }
}

#[test]
fn crashes_wait_longer_each_time_up_to_30_s_and_health_resets_the_wait() {
  let start = Instant::now();
  // On last-good, so the build's fallback never steps in.
  let mut policy = Policy::new(Binary::LastGood);
  let mut waits = Vec::new();
  for crash in 0..7 {
    policy.started(start + secs(crash * 100));
    match policy.exited(start + secs(crash * 100 + 1), Exit::Crashed, true) {
      Next::Restart { after, binary } => {
        assert_eq!(binary, Binary::LastGood);
        waits.push(after.as_secs());
      }
      Next::Stop => panic!("a crash restarts"),
    }
  }
  assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
  policy.healthy();
  assert_eq!(
    policy.exited(start + secs(900), Exit::Crashed, true),
    restart(1, Binary::LastGood)
  );
}

#[test]
fn three_quick_crashes_of_a_new_build_fall_back_to_last_good() {
  let start = Instant::now();
  let mut policy = Policy::new(Binary::Build);
  policy.started(start);
  assert_eq!(
    policy.exited(start + secs(1), Exit::Crashed, true),
    restart(1, Binary::Build)
  );
  policy.started(start + secs(2));
  assert_eq!(
    policy.exited(start + secs(3), Exit::Crashed, true),
    restart(2, Binary::Build)
  );
  policy.started(start + secs(5));
  assert_eq!(
    policy.exited(start + secs(6), Exit::Crashed, true),
    restart(1, Binary::LastGood)
  );
  assert_eq!(policy.binary, Binary::LastGood);
  // last-good crashing gets the same waits, and stays last-good.
  policy.started(start + secs(7));
  assert_eq!(
    policy.exited(start + secs(8), Exit::Crashed, true),
    restart(1, Binary::LastGood)
  );
}

#[test]
fn without_last_good_or_after_60_s_the_build_keeps_restarting() {
  let start = Instant::now();
  let mut policy = Policy::new(Binary::Build);
  for crash in 0..4 {
    policy.started(start + secs(crash * 2));
    let next = policy.exited(start + secs(crash * 2 + 1), Exit::Crashed, false);
    assert!(
      matches!(
        next,
        Next::Restart {
          binary: Binary::Build,
          ..
        }
      ),
      "{next:?}"
    );
  }

  let mut policy = Policy::new(Binary::Build);
  policy.started(start);
  for crash in 0..4 {
    // Each crash comes more than 60 s after the build first started.
    let next = policy.exited(start + secs(61 + crash), Exit::Crashed, true);
    assert!(
      matches!(
        next,
        Next::Restart {
          binary: Binary::Build,
          ..
        }
      ),
      "{next:?}"
    );
  }
}

#[test]
fn cargos_messages_name_the_one_program_built() {
  let library =
    r#"{"reason":"compiler-artifact","target":{"kind":["lib"]},"executable":null,"fresh":true}"#;
  let program = r#"{"reason":"compiler-artifact","target":{"kind":["bin"]},"executable":"/t/debug/my-shell","fresh":false}"#;
  let other = r#"{"reason":"compiler-artifact","target":{"kind":["bin"]},"executable":"/t/debug/second","fresh":true}"#;
  let script =
    r#"{"reason":"compiler-artifact","target":{"kind":["custom-build"]},"executable":null}"#;
  let finished = r#"{"reason":"build-finished","success":true}"#;
  let messages = [library, script, "not json", program, finished].join("\n");
  assert_eq!(
    read(&messages).built,
    Ok(Built {
      program: PathBuf::from("/t/debug/my-shell"),
      fresh: false,
    })
  );
  assert_eq!(
    read(other).built,
    Ok(Built {
      program: PathBuf::from("/t/debug/second"),
      fresh: true,
    })
  );
  assert_eq!(
    read(&[library, finished].join("\n")).built,
    Err(String::from("the config built no program"))
  );
  assert!(read(&[program, other].join("\n")).built.is_err());
}

#[test]
fn the_first_error_is_kept_for_the_notification() {
  let warning = r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused variable: `x`","rendered":"warning: unused variable: `x`\n"}}"#;
  let error = r#"{"reason":"compiler-message","message":{"level":"error","message":"expected `;`, found `}`","rendered":"error: expected `;`, found `}`\n"}}"#;
  let aborting = r#"{"reason":"compiler-message","message":{"level":"error","message":"aborting due to 1 previous error","rendered":"error: aborting due to 1 previous error\n"}}"#;
  let report = read(&[warning, error, aborting].join("\n"));
  assert_eq!(
    report,
    Report {
      rendered: String::from(
        "warning: unused variable: `x`\nerror: expected `;`, found `}`\nerror: aborting due to 1 previous error\n"
      ),
      first_error: Some(String::from("expected `;`, found `}`")),
      built: Err(String::from("the config built no program")),
    }
  );
}

#[test]
fn a_new_build_starts_over() {
  let start = Instant::now();
  let mut policy = Policy::new(Binary::Build);
  for crash in 0..3 {
    policy.started(start + secs(crash * 4));
    policy.exited(start + secs(crash * 4 + 1), Exit::Crashed, true);
  }
  assert_eq!(policy.binary, Binary::LastGood);
  policy.new_build();
  assert_eq!(policy.binary, Binary::Build);
  policy.started(start + secs(100));
  assert_eq!(
    policy.exited(start + secs(101), Exit::Crashed, true),
    restart(1, Binary::Build),
    "no crashes counted from before"
  );
}

#[test]
fn a_notification_body_escapes_markup() {
  assert_eq!(
    escape("expected `Option<u32>`, found `&str`"),
    "expected `Option&lt;u32&gt;`, found `&amp;str`"
  );
}

#[test]
fn editors_scratch_files_dont_count_as_edits() {
  use std::ffi::OsStr;
  for name in ["main.rs", "bar.rs", "#", "a#"] {
    assert!(relevant(OsStr::new(name)), "{name}");
  }
  for name in [".main.rs.swp", "main.rs~", "4913", "#main.rs#", ".#main.rs"] {
    assert!(!relevant(OsStr::new(name)), "{name}");
  }
}

/// What the shell's tests read, in crates/crabshell/src/tests/ipc.rs.
const CALL: &str = "{\"args\":[\"a\",\"b\"],\"function\":\"toggle\",\"target\":\"launcher\"}\n";
const LIST: &str = "{\"list\":true}\n";

fn call(target: &str, function: &str, args: &[&str]) -> Ask {
  Ask::Call {
    target: target.to_owned(),
    function: function.to_owned(),
    args: args.iter().map(|arg| (*arg).to_owned()).collect(),
  }
}

#[test]
fn ipc_lists_or_calls_and_needs_a_function_to_call() {
  assert_eq!(parse(&args("ipc")), Ok(Task::Ipc(Ask::List)));
  assert_eq!(
    parse(&args("ipc launcher toggle a b")),
    Ok(Task::Ipc(call("launcher", "toggle", &["a", "b"])))
  );
  assert_eq!(
    parse(&args("ipc bar hide")),
    Ok(Task::Ipc(call("bar", "hide", &[])))
  );
  assert_eq!(
    parse(&args("ipc launcher")),
    Err(String::from("can't make sense of `ipc launcher`"))
  );
}

#[test]
fn ipc_speaks_the_shells_lines() {
  assert_eq!(ipc::request(&call("launcher", "toggle", &["a", "b"])), CALL);
  assert_eq!(ipc::request(&Ask::List), LIST);
  // What the shell's `reply` writes.
  assert_eq!(
    ipc::read_reply(b"{\"ok\":\"on\\n\"}\n"),
    Ok(String::from("on\n"))
  );
  assert_eq!(
    ipc::read_reply(b"{\"error\":\"no\"}\n"),
    Err(String::from("no"))
  );
  assert_eq!(
    ipc::read_reply(b""),
    Err(String::from("the shell hung up without answering"))
  );
  let junk = ipc::read_reply(b"junk\n").expect_err("not JSON");
  assert!(
    junk.starts_with("the shell's reply doesn't parse"),
    "{junk}"
  );
  assert!(ipc::read_reply(b"{\"ok\":1}\n").is_err());
  // Printed with one newline at the end, and nothing for nothing.
  assert_eq!(ipc::shown("on"), "on\n");
  assert_eq!(ipc::shown("a\nb\n"), "a\nb\n");
  assert_eq!(ipc::shown(""), "");
}

#[test]
fn ipc_finds_the_socket_as_the_shell_names_it() {
  let path = |dir: Option<&str>, display: Option<&str>| {
    ipc::socket_path(dir.map(AsRef::as_ref), display.map(AsRef::as_ref))
  };
  let at = |name: &str| Some(PathBuf::from(format!("/run/user/7/crabshell/{name}")));
  assert_eq!(
    path(Some("/run/user/7"), Some("wayland-1")),
    at("ipc-wayland-1.sock")
  );
  assert_eq!(path(Some("/run/user/7"), None), at("ipc-wayland-0.sock"));
  assert_eq!(
    path(Some("/run/user/7"), Some("/tmp/sway/wayland-5")),
    at("ipc-wayland-5.sock")
  );
  assert_eq!(path(None, Some("wayland-1")), None);
  assert_eq!(path(Some("run/7"), Some("wayland-1")), None, "relative");
}

#[test]
fn ipc_gives_up_on_a_shell_that_never_answers() {
  let dir = std::env::temp_dir().join(format!("crabshell-cli-ipc-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).expect("a test directory");
  let socket = dir.join("ipc.sock");
  // Bound and never accepting: the connection waits in its backlog.
  let listener = std::os::unix::net::UnixListener::bind(&socket).expect("listening");
  let started = Instant::now();
  let given_up = ipc::ask(&socket, String::from(LIST), Duration::from_millis(200));
  assert!(started.elapsed() < Duration::from_secs(2));
  assert_eq!(
    given_up,
    Err(format!(
      "the shell at {} didn't answer within 0.2 s",
      socket.display()
    ))
  );
  drop(listener);
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ipc_keeps_a_reply_the_shell_sent_before_resetting_the_connection() {
  use std::io::Write;
  let dir = std::env::temp_dir().join(format!("crabshell-cli-reset-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).expect("a test directory");
  let socket = dir.join("ipc.sock");
  let listener = std::os::unix::net::UnixListener::bind(&socket).expect("listening");
  // A shell refusing a 17th connection: it answers without reading the
  // request, then closes, which resets the connection.
  let shell = std::thread::spawn(move || {
    let (mut stream, _) = listener.accept().expect("a client");
    std::thread::sleep(Duration::from_millis(100));
    stream
      .write_all(b"{\"error\":\"the shell has 16 IPC connections open already\"}\n")
      .expect("the reply");
  });
  let answer = ipc::ask(&socket, String::from(LIST), Duration::from_secs(2));
  shell.join().expect("the fake shell");
  assert_eq!(
    answer,
    Err(String::from(
      "the shell has 16 IPC connections open already"
    ))
  );
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_shell_gets_a_fixed_mmap_threshold_added_to_the_users_tunables() {
  use crate::supervise::{TUNABLE, tunables};
  assert_eq!(tunables(None), TUNABLE);
  assert_eq!(tunables(Some("".into())), TUNABLE);
  assert_eq!(
    tunables(Some("glibc.malloc.arena_max=2".into())),
    format!("glibc.malloc.arena_max=2:{TUNABLE}").as_str()
  );
  let own = "glibc.malloc.mmap_threshold=65536";
  assert_eq!(tunables(Some(own.into())), own, "the user's own wins");
}
