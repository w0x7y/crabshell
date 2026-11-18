//! The `crabshell` binary, run against a config directory of its own.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Home, PrivateBus, alive, checkout, is_compiler_error, switches, text, write_program};

/// The fenced block of `language` after `heading` in the design doc.
fn doc_block(heading: &str, language: &str) -> String {
  let doc = std::fs::read_to_string(checkout().join("docs/design.md")).expect("the design doc");
  let after = &doc[doc.find(heading).expect("the heading")..];
  let fence = format!("```{language}\n");
  let start = after.find(&fence).expect("the block") + fence.len();
  let end = start + after[start..].find("```").expect("the block's end");
  after[start..end].to_owned()
}

#[test]
fn the_template_is_the_design_docs_config() {
  let manifest = std::fs::read_to_string(
    Path::new(env!("CARGO_MANIFEST_DIR")).join("template/Cargo.toml.template"),
  )
  .expect("the template's manifest");
  let main =
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("template/src/main.rs"))
      .expect("the template's bar");
  assert_eq!(manifest, doc_block("## Writing a config", "toml"));
  assert_eq!(
    main,
    doc_block("The whole v0.1 bar as a user writes it:", "rust")
  );
}

#[test]
fn init_writes_the_template_once_and_never_overwrites() {
  let home = Home::new("init");
  let done = home.run(&["init"]);
  assert!(done.status.success(), "{}", text(&done.stderr));
  assert!(text(&done.stdout).contains("created a config in"));
  // Published on crates.io, so the plain init has nothing to warn about.
  assert!(done.stderr.is_empty(), "{}", text(&done.stderr));
  let manifest = std::fs::read_to_string(home.config().join("Cargo.toml")).expect("written");
  let release = concat!(
    "crabshell = \"",
    env!("CARGO_PKG_VERSION_MAJOR"),
    ".",
    env!("CARGO_PKG_VERSION_MINOR"),
    "\""
  );
  assert!(manifest.contains(release), "{manifest}");
  let main = std::fs::read_to_string(home.config().join("src/main.rs")).expect("written");
  assert!(main.contains("crabshell::run(|cx| cx.per_output(bar));"));

  std::fs::write(home.config().join("src/main.rs"), "// mine").expect("edited");
  let again = home.run(&["init"]);
  assert_eq!(again.status.code(), Some(1));
  assert!(
    text(&again.stderr).contains("already exists"),
    "{}",
    text(&again.stderr)
  );
  let main = std::fs::read_to_string(home.config().join("src/main.rs")).expect("still there");
  assert_eq!(main, "// mine", "nothing overwritten");
}

#[test]
fn init_with_a_path_builds_against_that_checkout() {
  let home = Home::new("path");
  let checkout = checkout().canonicalize().expect("the checkout");
  let done = home.run(&["init", "--path", checkout.to_str().expect("UTF-8")]);
  assert!(done.status.success(), "{}", text(&done.stderr));
  let manifest = std::fs::read_to_string(home.config().join("Cargo.toml")).expect("written");
  let line = format!(
    "crabshell = {{ path = \"{}\" }}",
    checkout.join("crates/crabshell").display()
  );
  assert!(manifest.contains(&line), "{manifest}");

  // A directory with a crates/crabshell that has no Cargo.toml in it.
  let elsewhere = Home::new("not-a-checkout");
  let fake = elsewhere.0.join("fake");
  std::fs::create_dir_all(fake.join("crates/crabshell")).expect("a fake checkout");
  let refused = elsewhere.run(&["init", "--path", fake.to_str().expect("UTF-8")]);
  assert_eq!(refused.status.code(), Some(1));
  assert!(text(&refused.stderr).contains("isn't a Crabshell checkout"));
  assert!(!elsewhere.config().exists(), "nothing created");
}

#[test]
fn a_relative_xdg_config_home_is_ignored_for_home() {
  let home = Home::new("relative");
  let done = home.run_with(&["init"], "relative/config");
  assert!(done.status.success(), "{}", text(&done.stderr));
  assert!(home.0.join(".config/crabshell/Cargo.toml").is_file());
}

#[test]
fn check_without_a_config_says_how_to_make_one() {
  let home = Home::new("no-config");
  let done = home.run(&["check"]);
  assert_eq!(done.status.code(), Some(1));
  assert!(text(&done.stderr).contains("create one with `crabshell init`"));
}

#[test]
fn a_bad_command_line_shows_the_usage() {
  let home = Home::new("usage");
  for line in [
    &[][..],
    &["build"][..],
    &["init", "--force"][..],
    &["ipc", "launcher"][..],
  ] {
    let done = home.run(line);
    assert_eq!(done.status.code(), Some(2), "{line:?}");
    assert!(text(&done.stderr).contains("usage: crabshell"), "{line:?}");
  }
  // An argument that isn't UTF-8 gets the usage too, not a panic.
  use std::os::unix::ffi::OsStrExt;
  let odd = Command::new(env!("CARGO_BIN_EXE_crabshell"))
    .current_dir(&home.0)
    .args(["init", "--path"])
    .arg(std::ffi::OsStr::from_bytes(b"/src/\xff"))
    .output()
    .expect("runs");
  assert_eq!(odd.status.code(), Some(2));
  assert!(text(&odd.stderr).contains("usage: crabshell"));
  let help = home.run(&["help"]);
  assert!(help.status.success());
  assert!(text(&help.stdout).contains("usage: crabshell"));
}

/// A shell's IPC socket in `home`'s runtime directory, on a thread: it
/// answers each connection with `reply` and hands back the line it read.
fn fake_shell(home: &Home, reply: &'static str) -> std::sync::mpsc::Receiver<String> {
  use std::io::{BufRead, Write};

  let dir = home.0.join("run").join("crabshell");
  std::fs::create_dir_all(&dir).expect("the runtime directory");
  let listener =
    std::os::unix::net::UnixListener::bind(dir.join("ipc-wayland-0.sock")).expect("listening");
  let (sender, lines) = std::sync::mpsc::channel();
  std::thread::spawn(move || {
    for stream in listener.incoming() {
      let Ok(mut stream) = stream else { break };
      let mut line = String::new();
      let _ = std::io::BufReader::new(&stream).read_line(&mut line);
      let _ = stream.write_all(reply.as_bytes());
      if sender.send(line).is_err() {
        break;
      }
    }
  });
  lines
}

#[test]
fn ipc_calls_the_shell_and_prints_what_it_says() {
  let home = Home::new("ipc");
  let lines = fake_shell(&home, "{\"ok\":\"shown\"}\n");
  let done = home.run(&["ipc", "launcher", "toggle", "a", "b"]);
  assert_eq!(done.status.code(), Some(0), "{}", text(&done.stderr));
  assert_eq!(text(&done.stdout), "shown\n");
  let line = lines
    .recv_timeout(Duration::from_secs(5))
    .expect("a request");
  // What the shell's tests read, in crates/crabshell/src/tests/ipc.rs.
  assert_eq!(
    line,
    "{\"args\":[\"a\",\"b\"],\"function\":\"toggle\",\"target\":\"launcher\"}\n"
  );
}

#[test]
fn ipc_says_why_when_the_shell_refuses_or_isnt_there() {
  let home = Home::new("ipc-error");
  let socket = home.0.join("run/crabshell/ipc-wayland-0.sock");
  let done = home.run(&["ipc"]);
  assert_eq!(done.status.code(), Some(1));
  assert_eq!(
    text(&done.stderr),
    format!("crabshell: no shell is listening at {}\n", socket.display())
  );

  let _lines = fake_shell(&home, "{\"error\":\"no IPC handler bar.show\"}\n");
  let done = home.run(&["ipc", "bar", "show"]);
  assert_eq!(done.status.code(), Some(1));
  assert_eq!(text(&done.stderr), "crabshell: no IPC handler bar.show\n");
  assert!(done.stdout.is_empty());
}

/// Builds a new config end to end, which compiles every dependency once,
/// so it takes minutes and is ignored by default.
#[test]
#[ignore = "compiles the whole template: minutes"]
fn a_new_config_checks_clean_and_a_mistake_fails_the_check() {
  let home = Home::new("check");
  let checkout = checkout().canonicalize().expect("the checkout");
  let done = home.run(&["init", "--path", checkout.to_str().expect("UTF-8")]);
  assert!(done.status.success(), "{}", text(&done.stderr));
  let checked = home.run(&["check"]);
  assert!(checked.status.success(), "{}", text(&checked.stderr));
  assert!(
    home.0.join("cache/crabshell/target").is_dir(),
    "built in the cache"
  );

  let main = home.config().join("src/main.rs");
  let code = std::fs::read_to_string(&main).expect("the bar");
  std::fs::write(&main, code.replace(".height(30)", ".height(\"tall\")")).expect("broken");
  let failed = home.run(&["check"]);
  assert!(!failed.status.success());
  assert!(
    text(&failed.stderr).contains("error"),
    "{}",
    text(&failed.stderr)
  );
}

/// A config with no dependencies that builds in a moment. Each start
/// appends `build` or `last-good`, its process id and the time it started
/// in milliseconds to `starts` once it said `ready`. The word in `mode` picks
/// what it does next: `crash` exits 1, `logout` exits 2 as with no
/// compositor, `deaf` stays up and ignores the supervisor, `late` says
/// `ready` only once a file `go` appears and then stays up, and anything
/// else stays up and follows the supervisor. Told `handoff`, it appends
/// its process id to `saves` and says `saved`; told `exit`, it appends its
/// process id and the time to `exits` and exits 0; told `save`, it appends
/// to `saves` and exits 0. `old` is a shell from before the handoff: it
/// doesn't say `can-hand-off` or know `handoff`. `reload` asks for a
/// reload on its first start. Told `alone`, it appends its process id
/// and the time to `alones`. `stray` starts a copy of itself that
/// connects and says `ready` in its place, and stays quiet itself.
const FAKE_SHELL: &str = r#"
use std::io::Write;

fn now() -> u128 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .expect("after 1970")
    .as_millis()
}

fn append(path: std::path::PathBuf, line: String) {
  let mut file = std::fs::OpenOptions::new()
    .append(true)
    .create(true)
    .open(path)
    .expect("a record");
  // One write, so a reader never sees half a line.
  file.write_all(line.as_bytes()).expect("written");
}

fn main() {
  // Before `ready`, which a busy machine may let the old shell answer
  // before this one writes its start.
  let started = now();
  let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
  let exe = std::env::current_exe().expect("a path");
  let name = if exe.ends_with("last-good") { "last-good" } else { "build" };
  let mode = std::fs::read_to_string(dir.join("mode")).unwrap_or_default();
  let first = !dir.join("starts").exists();
  if mode.trim() == "stray" && std::env::var_os("STRAY").is_none() {
    // Not the shell: a program it started, which outlives a few seconds.
    std::process::Command::new(&exe).env("STRAY", "1").spawn().expect("the copy");
    std::thread::sleep(std::time::Duration::from_secs(3600));
  }
  let mut control = std::env::var_os("CRABSHELL_CONTROL").map(|path| {
    std::os::unix::net::UnixStream::connect(path).expect("the supervisor")
  });
  let say = |control: &mut Option<std::os::unix::net::UnixStream>, line: &[u8]| {
    control.iter_mut().for_each(|stream| stream.write_all(line).expect("said"));
  };
  if mode.trim() != "old" {
    say(&mut control, b"can-hand-off\n");
  }
  if mode.trim() != "late" {
    say(&mut control, b"ready\n");
  }
  if std::env::var_os("STRAY").is_some() {
    std::thread::sleep(std::time::Duration::from_secs(3));
    std::process::exit(0);
  }
  // After connecting, so a test that sees the start can reload it.
  append(dir.join("starts"), format!("{name} {} {started}\n", std::process::id()));
  if std::env::var_os("CRABSHELL_BESIDE").is_some_and(|value| value == "1") {
    append(dir.join("besides"), format!("{}\n", std::process::id()));
  }
  match mode.trim() {
    "crash" => std::process::exit(1),
    "logout" => std::process::exit(2),
    "deaf" => {}
    "late" => {
      while !dir.join("go").exists() {
        std::thread::sleep(std::time::Duration::from_millis(50));
      }
      say(&mut control, b"ready\n");
    }
    _ => {
      use std::io::BufRead;
      if mode.trim() == "reload" && first {
        say(&mut control, b"reload\n");
      }
      let reader = control.as_ref().map(|stream| stream.try_clone().expect("a reader"));
      for line in reader.iter().flat_map(|stream| std::io::BufReader::new(stream).lines()) {
        let pid = std::process::id();
        match line.as_deref().unwrap_or_default() {
          "save" => {
            append(dir.join("saves"), format!("{pid}\n"));
            std::process::exit(0);
          }
          "handoff" if mode.trim() != "old" => {
            append(dir.join("saves"), format!("{pid}\n"));
            say(&mut control, b"saved\n");
          }
          "exit" => {
            append(dir.join("exits"), format!("{pid} {}\n", now()));
            std::process::exit(0);
          }
          "alone" => append(dir.join("alones"), format!("{pid} {}\n", now())),
          _ => {}
        }
      }
    }
  }
  std::thread::sleep(std::time::Duration::from_secs(3600));
}
"#;

impl Home {
  /// Writes the fake shell as the config, in `mode`.
  fn fake_config(&self, mode: &str) {
    let config = self.config();
    std::fs::create_dir_all(config.join("src")).expect("a config");
    std::fs::write(
      config.join("Cargo.toml"),
      "[package]\nname = \"fake-shell\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n",
    )
    .expect("written");
    std::fs::write(config.join("src/main.rs"), FAKE_SHELL).expect("written");
    std::fs::write(config.join("mode"), mode).expect("written");
  }

  /// A `last-good` that records its start like the fake, then stays up.
  fn fake_last_good(&self) {
    let path = self.0.join("cache/crabshell/last-good");
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("the cache");
    let starts = self.config().join("starts");
    let script = format!(
      "#!/bin/sh\necho \"last-good $$ $(date +%s%3N)\" >> '{}'\nexec sleep 3600\n",
      starts.display()
    );
    write_program(&path, &script);
  }

  /// Each start: which binary, its process id and when, in ms.
  fn starts(&self) -> Vec<(String, u32, u128)> {
    let text = std::fs::read_to_string(self.config().join("starts")).unwrap_or_default();
    text
      .lines()
      .map(|line| {
        let words: Vec<&str> = line.split(' ').collect();
        (
          words[0].to_owned(),
          words[1].parse().expect("a pid"),
          words[2].parse().expect("a time"),
        )
      })
      .collect()
  }

  /// Waits up to `limit` for `count` starts.
  fn wait_for_starts(&self, count: usize, limit: Duration) -> Vec<(String, u32, u128)> {
    let deadline = Instant::now() + limit;
    loop {
      let starts = self.starts();
      if starts.len() >= count || Instant::now() > deadline {
        return starts;
      }
      std::thread::sleep(Duration::from_millis(50));
    }
  }
}

#[test]
fn run_without_a_config_says_how_to_make_one() {
  let home = Home::new("run-no-config");
  let done = home.run(&["run"]);
  assert_eq!(done.status.code(), Some(1));
  assert!(text(&done.stderr).contains("create one with `crabshell init`"));
}

#[test]
fn run_stops_when_the_shell_loses_its_compositor() {
  let home = Home::new("run-logout");
  home.fake_config("logout");
  let (code, log) = home.supervise().wait(Duration::from_secs(60));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("exit status: 2); stopping"), "{log}");
  assert_eq!(home.starts().len(), 1, "never restarted");
  assert!(
    std::fs::read_dir(home.0.join("run/crabshell"))
      .expect("the runtime dir")
      .next()
      .is_none(),
    "the socket is removed"
  );
}

#[test]
fn a_crashing_shell_restarts_after_1_s_then_2_s() {
  let home = Home::new("run-crash");
  home.fake_config("crash");
  let supervised = home.supervise();
  let starts = home.wait_for_starts(3, Duration::from_secs(60));
  assert_eq!(starts.len(), 3, "{starts:?}");
  assert!(
    starts.iter().all(|(name, ..)| name == "build"),
    "{starts:?}"
  );
  let first = starts[1].2 - starts[0].2;
  let second = starts[2].2 - starts[1].2;
  assert!((1000..2000).contains(&first), "{first} ms");
  assert!((2000..3000).contains(&second), "{second} ms");
  // Told to stop while it waits to restart, it stops at once.
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(2));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("starting it again in 2 s"), "{log}");
}

#[test]
fn three_quick_crashes_fall_back_to_last_good() {
  let home = Home::new("run-fallback");
  home.fake_config("crash");
  home.fake_last_good();
  let bus = PrivateBus::start(&home);
  let supervised = home.supervise_on(Some(&bus));
  let starts = home.wait_for_starts(4, Duration::from_secs(60));
  let names: Vec<&str> = starts.iter().map(|(name, ..)| name.as_str()).collect();
  assert_eq!(names, ["build", "build", "build", "last-good"]);
  let fallback = starts[3].2 - starts[2].2;
  assert!((1000..2000).contains(&fallback), "{fallback} ms");
  let last_good = starts[3].1;
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("keeps crashing, so last-good starts"), "{log}");
  assert!(!alive(last_good), "the shell is ended with the supervisor");
  let notes = bus.notifications();
  assert_eq!(notes.len(), 1, "{notes:?}");
  assert_eq!(notes[0].0, "Your Crabshell config keeps crashing");
}

#[test]
fn a_config_that_doesnt_build_runs_last_good_or_fails() {
  let home = Home::new("run-broken");
  home.fake_config("stay");
  std::fs::write(home.config().join("src/main.rs"), "fn main() {").expect("broken");
  let (code, log) = home.supervise().wait(Duration::from_secs(60));
  assert_eq!(code, Some(1), "{log}");
  assert!(
    log.contains("crabshell: the config didn't build: "),
    "{log}"
  );

  home.fake_last_good();
  let bus = PrivateBus::start(&home);
  let supervised = home.supervise_on(Some(&bus));
  let starts = home.wait_for_starts(1, Duration::from_secs(60));
  assert_eq!(starts.len(), 1);
  assert_eq!(starts[0].0, "last-good");
  let notes = bus.wait_for(1, Duration::from_secs(5));
  supervised.signal("-INT");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("; starting last-good"), "{log}");
  assert_eq!(notes.len(), 1, "{notes:?}");
  assert_eq!(notes[0].0, "Your Crabshell config didn't build");
  let problem = notes[0]
    .1
    .strip_suffix("; the last config that worked runs instead")
    .expect("it says last-good runs");
  assert!(is_compiler_error(&log, problem), "{notes:?}\n{log}");
}

#[test]
fn a_supervisor_with_a_shell_up_sleeps_and_stops_on_sigterm() {
  let home = Home::new("run-idle");
  home.fake_config("stay");
  let supervised = home.supervise();
  let starts = home.wait_for_starts(1, Duration::from_secs(60));
  assert_eq!(starts.len(), 1);
  // Let it read `ready` and settle.
  std::thread::sleep(Duration::from_secs(1));
  let before = switches(supervised.pid());
  std::thread::sleep(Duration::from_secs(3));
  assert_eq!(switches(supervised.pid()), before, "it woke while idle");
  let shell = starts[0].1;
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("the shell is ready"), "{log}");
  assert!(!alive(shell), "the shell is ended with the supervisor");
}

#[test]
fn run_without_a_session_refuses_before_building() {
  let home = Home::new("run-no-session");
  home.fake_config("stay");
  std::fs::write(home.config().join("src/main.rs"), "fn main() {").expect("broken");
  let config_home = home.0.join("config");
  let done = home
    .command(&["run"], config_home.to_str().expect("UTF-8"))
    .env_remove("XDG_RUNTIME_DIR")
    .output()
    .expect("runs");
  assert_eq!(done.status.code(), Some(1));
  let log = text(&done.stderr);
  assert!(log.contains("XDG_RUNTIME_DIR isn't set"), "{log}");
  assert!(!log.contains("Compiling"), "it didn't build: {log}");
}

#[test]
fn a_program_that_cant_start_counts_as_a_crash() {
  let home = Home::new("run-cant-start");
  home.fake_config("stay");
  std::fs::write(home.config().join("src/main.rs"), "fn main() {").expect("broken");
  home.fake_last_good();
  use std::os::unix::fs::PermissionsExt;
  let last_good = home.0.join("cache/crabshell/last-good");
  std::fs::set_permissions(&last_good, std::fs::Permissions::from_mode(0o644))
    .expect("not executable");
  let supervised = home.supervise();
  // The first try and the one after 1 s, however long cargo takes.
  wait_in_log(
    &supervised,
    "starting it again in 2 s",
    Duration::from_secs(60),
  );
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("didn't start"), "{log}");
  assert!(log.contains("starting it again in 1 s"), "{log}");
  assert!(log.contains("starting it again in 2 s"), "{log}");
  assert!(home.starts().is_empty());
}

/// Waits a minute for the shell to count as healthy.
#[test]
#[ignore = "waits 60 s"]
fn a_shell_that_runs_60_s_is_kept_as_last_good() {
  let home = Home::new("run-healthy");
  home.fake_config("stay");
  let supervised = home.supervise();
  let last_good = home.0.join("cache/crabshell/last-good");
  let deadline = Instant::now() + Duration::from_secs(90);
  while !last_good.exists() && Instant::now() < deadline {
    std::thread::sleep(Duration::from_millis(200));
  }
  let program = home.0.join("cache/crabshell/target/debug/fake-shell");
  assert_eq!(
    std::fs::read(&last_good).expect("kept"),
    std::fs::read(&program).expect("built"),
  );
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("its build is kept as"), "{log}");
}

/// Stays up 62 s without `ready`, then says it: only 60 s after that is
/// the build kept.
#[test]
#[ignore = "waits 2 minutes"]
fn a_shell_is_kept_as_last_good_60_s_after_ready_not_after_its_start() {
  let home = Home::new("run-late-ready");
  home.fake_config("late");
  let supervised = home.supervise();
  home.wait_for_starts(1, Duration::from_secs(60));
  let last_good = home.0.join("cache/crabshell/last-good");
  std::thread::sleep(Duration::from_secs(62));
  assert!(!last_good.exists(), "kept before it said ready");
  std::fs::write(home.config().join("go"), "").expect("written");
  let ready = Instant::now();
  let deadline = ready + Duration::from_secs(90);
  while !last_good.exists() && Instant::now() < deadline {
    std::thread::sleep(Duration::from_millis(200));
  }
  let kept = ready.elapsed();
  assert!(last_good.exists(), "never kept");
  assert!(kept >= Duration::from_secs(59), "kept {kept:?} after ready");
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("the shell is ready"), "{log}");
}

/// Crashes for over a minute, so a timer left from an earlier start would
/// have fired by then.
#[test]
#[ignore = "waits 65 s"]
fn a_shell_that_keeps_crashing_is_never_kept_as_last_good() {
  let home = Home::new("run-never-good");
  home.fake_config("crash");
  let supervised = home.supervise();
  std::thread::sleep(Duration::from_secs(65));
  assert!(home.starts().len() >= 6, "{:?}", home.starts());
  assert!(!home.0.join("cache/crabshell/last-good").exists());
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
}

impl Home {
  /// Edits the config's `src/main.rs`.
  fn edit(&self, change: impl FnOnce(String) -> String) {
    let main = self.config().join("src/main.rs");
    let code = std::fs::read_to_string(&main).expect("the fake");
    std::fs::write(&main, change(code)).expect("edited");
  }

  /// The process ids the fake wrote when told to save.
  fn saves(&self) -> Vec<u32> {
    let text = std::fs::read_to_string(self.config().join("saves")).unwrap_or_default();
    text
      .lines()
      .map(|line| line.parse().expect("a pid"))
      .collect()
  }

  /// The process ids of the fakes started beside an old shell.
  fn besides(&self) -> Vec<u32> {
    let text = std::fs::read_to_string(self.config().join("besides")).unwrap_or_default();
    text
      .lines()
      .map(|line| line.parse().expect("a pid"))
      .collect()
  }

  /// Each `alone` the fakes heard: the process id and the time in ms.
  fn alones(&self) -> Vec<(u32, u128)> {
    let text = std::fs::read_to_string(self.config().join("alones")).unwrap_or_default();
    let line = |line: &str| {
      let (pid, at) = line.split_once(' ').expect("a pid and a time");
      (pid.parse().expect("a pid"), at.parse().expect("a time"))
    };
    text.lines().map(line).collect()
  }

  /// Waits up to `limit` for the fake told to exit to write its process
  /// id and the time in ms.
  fn wait_for_exit(&self, limit: Duration) -> (u32, u128) {
    let deadline = Instant::now() + limit;
    loop {
      let text = std::fs::read_to_string(self.config().join("exits")).unwrap_or_default();
      if let Some(line) = text.lines().next() {
        let (pid, at) = line.split_once(' ').expect("a pid and a time");
        return (pid.parse().expect("a pid"), at.parse().expect("a time"));
      }
      assert!(Instant::now() < deadline, "no exit within {limit:?}");
      std::thread::sleep(Duration::from_millis(20));
    }
  }
}

#[test]
fn an_edit_starts_the_new_build_beside_the_old_shell_until_it_is_ready() {
  let home = Home::new("reload");
  home.fake_config("stay");
  let supervised = home.supervise();
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  // Two saves close together build once.
  home.edit(|code| code + "// an edit\n");
  std::thread::sleep(Duration::from_millis(100));
  home.edit(|code| code + "// another\n");
  let starts = home.wait_for_starts(2, Duration::from_secs(60));
  assert_eq!(starts.len(), 2, "{starts:?}");
  assert_eq!(starts[1].0, "build");
  let (exited, at) = home.wait_for_exit(Duration::from_secs(5));
  assert_eq!((home.saves(), exited), (vec![first], first));
  assert!(
    at >= starts[1].2,
    "the old shell ran until the new one started"
  );
  let deadline = Instant::now() + Duration::from_secs(5);
  while alive(first) || home.alones().len() < 2 {
    assert!(Instant::now() < deadline, "{:?}", home.alones());
    std::thread::sleep(Duration::from_millis(20));
  }
  let alones = home.alones();
  assert_eq!(alones.len(), 2, "{alones:?}");
  assert_eq!(alones[0].0, first, "alone at once, with no old shell");
  assert_eq!(alones[1].0, starts[1].1);
  assert!(alones[1].1 >= at, "alone once the old shell exited");
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(
    log.contains("asking the shell to save for a reload"),
    "{log}"
  );
  assert_eq!(log.matches("the build took").count(), 2, "{log}");
  assert!(log.contains("reloaded with no gap"), "{log}");
  assert!(
    log.contains("the old shell ended (exit status: 0)"),
    "{log}"
  );
}

#[test]
fn a_shell_from_before_the_handoff_saves_and_exits_first() {
  let home = Home::new("reload-old");
  home.fake_config("old");
  let supervised = home.supervise();
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  home.edit(|code| code + "// an edit\n");
  let starts = home.wait_for_starts(2, Duration::from_secs(60));
  assert_eq!(starts.len(), 2, "{starts:?}");
  assert_eq!(home.saves(), [first], "the old shell saved");
  assert!(!alive(first));
  std::thread::sleep(Duration::from_millis(300));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("asking the shell to save and exit"), "{log}");
  assert!(log.contains("reloaded with"), "{log}");
  assert!(!log.contains("reloaded with no gap"), "{log}");
}

#[test]
fn a_reload_the_shell_asks_for_starts_the_same_build_again() {
  let home = Home::new("asked-reload");
  home.fake_config("reload");
  let supervised = home.supervise();
  let starts = home.wait_for_starts(2, Duration::from_secs(60));
  assert_eq!(starts.len(), 2, "{starts:?}");
  let (exited, _) = home.wait_for_exit(Duration::from_secs(5));
  assert_eq!((home.saves(), exited), (vec![starts[0].1], starts[0].1));
  std::thread::sleep(Duration::from_millis(300));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("the shell asked for a reload"), "{log}");
  assert!(
    log.contains("nothing in the build changed; starting it again"),
    "{log}"
  );
  assert!(log.contains("reloaded with no gap"), "{log}");
  assert_eq!(home.starts().len(), 2, "asked once");
}

#[test]
fn a_broken_edit_keeps_the_shell_and_says_why() {
  let home = Home::new("broken-edit");
  home.fake_config("stay");
  let bus = PrivateBus::start(&home);
  let supervised = home.supervise_on(Some(&bus));
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  home.edit(|code| code + "fn broken() {\n");
  let notes = bus.wait_for(1, Duration::from_secs(60));
  assert_eq!(notes.len(), 1, "{notes:?}");
  assert_eq!(notes[0].0, "Your Crabshell config didn't build");
  let problem = notes[0].1.clone();
  assert!(alive(first), "the old shell runs on");
  assert_eq!(home.starts().len(), 1);
  // Sending it left nothing that wakes the supervisor.
  std::thread::sleep(Duration::from_millis(500));
  let before = switches(supervised.pid());
  std::thread::sleep(Duration::from_secs(2));
  assert_eq!(switches(supervised.pid()), before, "it woke while idle");

  home.edit(|code| code.replace("fn broken() {\n", ""));
  let starts = home.wait_for_starts(2, Duration::from_secs(60));
  assert_eq!(starts.len(), 2, "fixing it reloads");
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("the running shell stays"), "{log}");
  assert!(is_compiler_error(&log, &problem), "{problem:?}\n{log}");
}

#[test]
fn edits_that_change_nothing_dont_reload() {
  let home = Home::new("no-change");
  home.fake_config("stay");
  let supervised = home.supervise();
  home.wait_for_starts(1, Duration::from_secs(60));
  // Not the config's sources: no build at all.
  std::fs::write(home.config().join("notes.txt"), "mine").expect("written");
  std::fs::write(home.config().join("src/.main.rs.swp"), "vim").expect("written");
  std::thread::sleep(Duration::from_secs(1));
  // A new directory builds, and so does a file in it once that build is
  // done, but the program stays the same.
  std::fs::create_dir(home.config().join("src/more")).expect("a directory");
  std::thread::sleep(Duration::from_millis(1500));
  std::fs::write(home.config().join("src/more/unused.rs"), "// mine").expect("written");
  std::thread::sleep(Duration::from_millis(1500));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert_eq!(
    log.matches("the config changed; building it").count(),
    2,
    "{log}"
  );
  assert_eq!(
    log.matches("nothing in the build changed").count(),
    2,
    "{log}"
  );
  assert_eq!(home.starts().len(), 1);
}

#[test]
fn a_shell_that_ignores_save_is_killed_after_5_s() {
  let home = Home::new("deaf");
  home.fake_config("deaf");
  let supervised = home.supervise();
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  home.edit(|code| code + "// an edit\n");
  // Asked after `asked_after` and by `asked_by`, however long the build
  // took.
  let deadline = Instant::now() + Duration::from_secs(60);
  let mut asked_after = Instant::now();
  loop {
    let looked = Instant::now();
    if supervised.log().contains("asking the shell to save") {
      break;
    }
    asked_after = looked;
    assert!(Instant::now() < deadline, "{}", supervised.log());
    std::thread::sleep(Duration::from_millis(10));
  }
  let asked_by = Instant::now();
  // A second edit while the reload waits doesn't put off the kill.
  std::thread::sleep(Duration::from_millis(2500));
  home.edit(|code| code + "// another\n");
  // Killed after `last_alive` and by `dead_by`.
  let mut last_alive = Instant::now();
  loop {
    let looked = Instant::now();
    if !alive(first) {
      break;
    }
    last_alive = looked;
    assert!(Instant::now() < deadline, "{}", supervised.log());
    std::thread::sleep(Duration::from_millis(10));
  }
  let dead_by = Instant::now();
  // The longest and shortest the wait could have been, so a late look
  // can't fail it.
  let longest = dead_by - asked_after;
  let shortest = last_alive.saturating_duration_since(asked_by);
  assert!(longest >= Duration::from_secs(5), "{longest:?}");
  assert!(shortest < Duration::from_millis(7500), "{shortest:?}");
  let starts = home.wait_for_starts(2, Duration::from_secs(60));
  assert_eq!(starts.len(), 2, "{starts:?}");
  assert!(home.saves().is_empty());
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("didn't end within 5 s; killing it"), "{log}");
  assert_eq!(log.matches("asking the shell to save").count(), 1, "{log}");
  assert_eq!(log.matches("the build took").count(), 3, "{log}");
}

#[test]
fn an_edit_replaces_a_crashing_shell_at_once() {
  let home = Home::new("fix-crash");
  home.fake_config("crash");
  let supervised = home.supervise();
  let starts = home.wait_for_starts(3, Duration::from_secs(60));
  assert_eq!(starts.len(), 3, "{starts:?}");
  // The next restart is due 4 s after the third start. The fix ignores
  // the mode, so its build stays up.
  home.edit(|code| code.replace("\"crash\" =>", "\"crashes\" =>"));
  let starts = home.wait_for_starts(4, Duration::from_secs(60));
  assert_eq!(starts.len(), 4, "{starts:?}");
  let gap = starts[3].2 - starts[2].2;
  // The restart that was due doesn't start another.
  std::thread::sleep(Duration::from_millis(4500 - gap.min(4500) as u64 + 500));
  assert_eq!(home.starts().len(), 4, "{:?}", home.starts());
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
}

/// Reloads just before the old shell turns 60 s old, so its healthy
/// timer fires after the new build replaced it on disk.
#[test]
#[ignore = "waits 60 s"]
fn a_reload_never_keeps_the_new_build_before_it_runs() {
  let home = Home::new("reload-at-60");
  home.fake_config("deaf");
  let supervised = home.supervise();
  home.wait_for_starts(1, Duration::from_secs(60));
  let program = home.0.join("cache/crabshell/target/debug/fake-shell");
  let old = std::fs::read(&program).expect("built");
  std::thread::sleep(Duration::from_secs(57));
  // A change to the program's bytes, which a comment wouldn't make.
  home.edit(|code| code.replace("expect(\"a record\")", "expect(\"the record\")"));
  let starts = home.wait_for_starts(2, Duration::from_secs(60));
  assert_eq!(starts.len(), 2, "{starts:?}");
  let kept = std::fs::read(home.0.join("cache/crabshell/last-good")).expect("kept");
  assert!(kept == old, "last-good is the program that ran 60 s");
  assert!(kept != std::fs::read(&program).expect("rebuilt"));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
}

#[test]
fn stopping_during_a_reload_ends_both() {
  let home = Home::new("stop-reload");
  home.fake_config("deaf");
  let supervised = home.supervise();
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  home.edit(|code| code + "// an edit\n");
  // The build is done and the reload waits on a shell that won't save.
  std::thread::sleep(Duration::from_millis(2000));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(2));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("asking the shell to save"), "{log}");
  assert!(!alive(first));
  assert_eq!(home.starts().len(), 1, "the new build never started");
}

#[test]
fn a_write_through_a_symlink_rebuilds() {
  let home = Home::new("symlink");
  home.fake_config("stay");
  let elsewhere = home.0.join("elsewhere");
  std::fs::create_dir(&elsewhere).expect("a directory");
  std::fs::write(elsewhere.join("linked.rs"), "// mine").expect("written");
  std::os::unix::fs::symlink(
    elsewhere.join("linked.rs"),
    home.config().join("src/linked.rs"),
  )
  .expect("a link");
  let supervised = home.supervise();
  home.wait_for_starts(1, Duration::from_secs(60));
  std::fs::write(elsewhere.join("linked.rs"), "// changed").expect("written");
  std::thread::sleep(Duration::from_millis(1500));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert_eq!(
    log.matches("the config changed; building it").count(),
    1,
    "{log}"
  );
}

#[test]
fn an_edit_during_the_first_build_rebuilds() {
  let home = Home::new("edit-while-building");
  home.fake_config("stay");
  // A build script that holds the first build up for 3 s.
  std::fs::write(
    home.config().join("build.rs"),
    "fn main() {\n  println!(\"cargo::rerun-if-changed=build.rs\");\n  \
     std::thread::sleep(std::time::Duration::from_secs(3));\n}\n",
  )
  .expect("written");
  let supervised = home.supervise();
  std::thread::sleep(Duration::from_millis(1500));
  home.edit(|code| code + "// an edit\n");
  let starts = home.wait_for_starts(1, Duration::from_secs(60));
  assert_eq!(starts.len(), 1);
  std::thread::sleep(Duration::from_millis(1500));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(
    log.contains("the config changed; building it"),
    "the edit was missed: {log}"
  );
}

#[test]
fn a_new_build_that_crashes_before_ready_leaves_the_old_shell_running() {
  let home = Home::new("crash-before-ready");
  home.fake_config("stay");
  let bus = PrivateBus::start(&home);
  let supervised = home.supervise_on(Some(&bus));
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  let crash = "fn main() {\n  std::process::exit(3);";
  home.edit(|code| code.replacen("fn main() {", crash, 1));
  let notes = bus.wait_for(1, Duration::from_secs(60));
  assert_eq!(notes.len(), 1, "{notes:?}");
  assert_eq!(notes[0].0, "Your Crabshell config didn't start");
  assert!(notes[0].1.contains("exit status: 3"), "{notes:?}");
  assert!(alive(first), "the old shell runs on");
  assert_eq!(home.saves(), [first]);
  // The fix reloads the old shell, which was still the running one.
  home.edit(|code| code.replacen(crash, "fn main() {", 1));
  let starts = home.wait_for_starts(2, Duration::from_secs(60));
  assert_eq!(starts.len(), 2, "{starts:?}");
  assert_eq!(home.wait_for_exit(Duration::from_secs(5)).0, first);
  std::thread::sleep(Duration::from_millis(300));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("so the old shell stays"), "{log}");
  assert_eq!(log.matches("reloaded with no gap").count(), 1, "{log}");
}

#[test]
fn stopping_while_the_new_shell_starts_ends_both() {
  let home = Home::new("stop-swap");
  home.fake_config("stay");
  let supervised = home.supervise();
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  // The new build never says ready, so the old one stays beside it.
  let hang = "fn main() {\n  std::thread::sleep(std::time::Duration::from_secs(3600));";
  home.edit(|code| code.replacen("fn main() {", hang, 1));
  let deadline = Instant::now() + Duration::from_secs(60);
  while home.saves().is_empty() {
    assert!(Instant::now() < deadline, "no save");
    std::thread::sleep(Duration::from_millis(50));
  }
  std::thread::sleep(Duration::from_millis(300));
  let pid = supervised.pid();
  let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
    .expect("the supervisor's children");
  let children: Vec<u32> = children
    .split_whitespace()
    .map(|child| child.parse().expect("a pid"))
    .collect();
  assert_eq!(children.len(), 2, "both shells run: {children:?}");
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(!alive(first), "{log}");
  assert!(children.iter().all(|&child| !alive(child)), "{log}");
}

/// Waits up to `limit` for `needle` in the supervisor's log.
fn wait_in_log(supervised: &common::Supervised, needle: &str, limit: Duration) {
  let deadline = Instant::now() + limit;
  while !supervised.log().contains(needle) {
    assert!(
      Instant::now() < deadline,
      "no {needle:?}: {}",
      supervised.log()
    );
    std::thread::sleep(Duration::from_millis(50));
  }
}

/// A `main` that sleeps before it connects, so the build never says ready.
const HANG: &str = "fn main() {\n  std::thread::sleep(std::time::Duration::from_secs(3600));";

#[test]
fn a_new_build_never_ready_is_killed_after_10_s_and_the_old_shell_stays() {
  let home = Home::new("never-ready");
  home.fake_config("stay");
  let bus = PrivateBus::start(&home);
  let supervised = home.supervise_on(Some(&bus));
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  home.edit(|code| code.replacen("fn main() {", HANG, 1));
  wait_in_log(
    &supervised,
    "starting the new build beside it",
    Duration::from_secs(60),
  );
  let started = Instant::now();
  let notes = bus.wait_for(1, Duration::from_secs(30));
  let waited = started.elapsed();
  assert!(
    (Duration::from_secs(9)..Duration::from_secs(13)).contains(&waited),
    "{waited:?}"
  );
  assert_eq!(notes[0].0, "Your Crabshell config didn't start");
  assert!(notes[0].1.contains("wasn't ready within 10 s"), "{notes:?}");
  assert!(alive(first), "the old shell runs on");
  // A fix reloads it as usual.
  home.edit(|code| code.replacen(HANG, "fn main() {", 1));
  let starts = home.wait_for_starts(2, Duration::from_secs(60));
  assert_eq!(starts.len(), 2, "{starts:?}");
  assert_eq!(home.wait_for_exit(Duration::from_secs(5)).0, first);
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("killing it"), "{log}");
}

#[test]
fn an_edit_while_the_new_shell_starts_replaces_it_beside_the_old_one() {
  let home = Home::new("replace-starting");
  home.fake_config("stay");
  let supervised = home.supervise();
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  home.edit(|code| code.replacen("fn main() {", HANG, 1));
  wait_in_log(
    &supervised,
    "starting the new build beside it",
    Duration::from_secs(60),
  );
  home.edit(|code| code.replacen(HANG, "fn main() {", 1));
  let starts = home.wait_for_starts(2, Duration::from_secs(60));
  assert_eq!(starts.len(), 2, "{starts:?}");
  assert_eq!(home.wait_for_exit(Duration::from_secs(5)).0, first);
  std::thread::sleep(Duration::from_millis(300));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(
    log.contains("ending the new shell, which isn't ready yet"),
    "{log}"
  );
  assert!(log.contains("ended for a newer build"), "{log}");
  assert!(!log.contains("so the old shell stays"), "{log}");
  // The ended shell read what it saved, so it saved again for the next.
  assert_eq!(home.saves(), [first, first]);
  assert_eq!(home.besides(), [starts[1].1], "started beside the old one");
}

/// A program the shell started connects and says `ready` in its place:
/// the kernel says who connected, so it doesn't count.
#[test]
fn a_connection_from_another_process_isnt_the_shells() {
  let home = Home::new("stray");
  home.fake_config("stray");
  let supervised = home.supervise();
  wait_in_log(&supervised, "the build took", Duration::from_secs(60));
  std::thread::sleep(Duration::from_millis(1500));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(!log.contains("the shell is ready"), "{log}");
}

#[test]
fn a_new_build_that_dies_right_after_connecting_leaves_the_old_shell() {
  let home = Home::new("dies-connected");
  home.fake_config("stay");
  let supervised = home.supervise();
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  let late = "  if mode.trim() != \"late\" {";
  let dies = "  std::process::exit(4);\n  if mode.trim() != \"late\" {";
  home.edit(|code| code.replacen(late, dies, 1));
  wait_in_log(
    &supervised,
    "so the old shell stays",
    Duration::from_secs(60),
  );
  assert!(alive(first), "the old shell runs on");
  // The old shell's connection is still its own: a fix reloads it.
  home.edit(|code| code.replacen(dies, late, 1));
  let starts = home.wait_for_starts(2, Duration::from_secs(60));
  assert_eq!(starts.len(), 2, "{starts:?}");
  assert_eq!(home.wait_for_exit(Duration::from_secs(5)).0, first);
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("exit status: 4"), "{log}");
}

/// The supervisor's children: the shells running.
fn shells(supervised: &common::Supervised) -> Vec<u32> {
  let pid = supervised.pid();
  let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
    .expect("the supervisor's children");
  children
    .split_whitespace()
    .map(|child| child.parse().expect("a pid"))
    .collect()
}

/// The old shell crashes while the new one starts slowly: the new one is
/// the only shell then, and isn't killed for being late.
#[test]
fn a_new_shell_left_alone_by_a_crash_is_never_late() {
  let home = Home::new("old-crashes");
  home.fake_config("stay");
  let supervised = home.supervise();
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  home.edit(|code| code.replacen("fn main() {", HANG, 1));
  wait_in_log(
    &supervised,
    "starting the new build beside it",
    Duration::from_secs(60),
  );
  std::thread::sleep(Duration::from_millis(300));
  let new: Vec<u32> = shells(&supervised)
    .into_iter()
    .filter(|&pid| pid != first)
    .collect();
  assert_eq!(new.len(), 1, "{new:?}");
  let killed = std::process::Command::new("kill")
    .args(["-KILL", &first.to_string()])
    .status()
    .expect("kill runs");
  assert!(killed.success());
  std::thread::sleep(Duration::from_secs(12));
  assert!(alive(new[0]), "the new shell runs on");
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("the old shell ended"), "{log}");
  assert!(!log.contains("wasn't ready"), "{log}");
}

/// The new build can't start, so the old shell stays; nothing ends it 10 s
/// later for a new shell that never came.
#[test]
fn a_new_build_that_cant_start_leaves_the_old_shell_for_good() {
  let home = Home::new("cant-start");
  home.fake_config("stay");
  let bus = PrivateBus::start(&home);
  let supervised = home.supervise_on(Some(&bus));
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  // A linker that writes a program whose interpreter doesn't exist, so
  // starting it fails.
  let linker = home.0.join("bad-linker");
  write_program(
    &linker,
    "#!/bin/sh\nwhile [ $# -gt 0 ]; do [ \"$1\" = -o ] && out=$2; shift; done\n\
     printf '#!/nonexistent/crabshell-test\\n' > \"$out\"\nchmod +x \"$out\"\n",
  );
  std::fs::create_dir_all(home.config().join(".cargo")).expect("a directory");
  std::fs::write(
    home.config().join(".cargo/config.toml"),
    format!(
      "[build]\nrustflags = [\"-C\", \"linker={}\"]\n",
      linker.display()
    ),
  )
  .expect("written");
  home.edit(|code| code + "// an edit\n");
  let notes = bus.wait_for(1, Duration::from_secs(120));
  assert_eq!(notes[0].0, "Your Crabshell config didn't start");
  assert!(notes[0].1.contains("didn't start"), "{notes:?}");
  std::thread::sleep(Duration::from_secs(12));
  assert!(alive(first), "the old shell runs on");
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(!log.contains("wasn't ready"), "{log}");
  assert!(!log.contains("killing it"), "{log}");
}

/// A `main` that becomes a process ignoring SIGTERM, as a shell whose loop
/// is stuck would.
const DEAF_HANG: &str = "fn main() {\n  use std::os::unix::process::CommandExt;\n  \
  let _ = std::process::Command::new(\"sh\").args([\"-c\", \"trap '' TERM; exec sleep 3600\"]).exec();";

/// The old shell crashes while the new one, which ignores SIGTERM, is
/// being replaced: the new one is still killed, and the newest build
/// starts at once, not as a crash's restart.
#[test]
fn a_shell_being_replaced_is_killed_and_the_newest_starts_when_the_old_one_crashes() {
  let home = Home::new("replaced-alone");
  home.fake_config("stay");
  let supervised = home.supervise();
  let first = home.wait_for_starts(1, Duration::from_secs(60))[0].1;
  home.edit(|code| code.replacen("fn main() {", DEAF_HANG, 1));
  wait_in_log(
    &supervised,
    "starting the new build beside it",
    Duration::from_secs(60),
  );
  home.edit(|code| code.replacen(DEAF_HANG, "fn main() {", 1));
  wait_in_log(
    &supervised,
    "ending the new shell, which isn't ready yet",
    Duration::from_secs(60),
  );
  let killed = std::process::Command::new("kill")
    .args(["-KILL", &first.to_string()])
    .status()
    .expect("kill runs");
  assert!(killed.success());
  let starts = home.wait_for_starts(2, Duration::from_secs(15));
  assert_eq!(starts.len(), 2, "{starts:?}\n{}", supervised.log());
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("which starts now"), "{log}");
  assert!(!log.contains("starting it again in"), "not a crash: {log}");
}

/// Told to stop, the supervisor ends a shell that ignores SIGTERM 5 s
/// later, rather than waiting on it.
#[test]
fn a_shell_that_ignores_sigterm_is_killed_when_run_stops() {
  let home = Home::new("deaf-stop");
  home.fake_config("stay");
  home.edit(|code| code.replacen("fn main() {", DEAF_HANG, 1));
  let supervised = home.supervise();
  wait_in_log(&supervised, "the build took", Duration::from_secs(60));
  std::thread::sleep(Duration::from_millis(500));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(8));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("killing it"), "{log}");
}
