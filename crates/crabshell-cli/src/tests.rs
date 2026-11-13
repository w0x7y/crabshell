//! The command line and the template, without running anything.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::build::{Built, Report, read};
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
