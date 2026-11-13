//! The `crabshell` binary, run against a config directory of its own.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Home, PrivateBus, alive, checkout, switches, text};

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
  assert!(text(&done.stderr).contains("isn't on crates.io yet"));
  let manifest = std::fs::read_to_string(home.config().join("Cargo.toml")).expect("written");
  assert!(manifest.contains("crabshell = \"0.1\""), "{manifest}");
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
  for line in [&[][..], &["build"][..], &["init", "--force"][..]] {
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
/// appends `build` or `last-good`, its process id and the time in
/// milliseconds to `starts` once it said `ready`. The word in `mode` picks
/// what it does next: `crash` exits 1, `logout` exits 2 as with no
/// compositor, `deaf` stays up and ignores the supervisor, and anything
/// else stays up until told to save, when it appends its process id to
/// `saves` and exits 0.
const FAKE_SHELL: &str = r#"
use std::io::Write;

fn main() {
  let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
  let exe = std::env::current_exe().expect("a path");
  let name = if exe.ends_with("last-good") { "last-good" } else { "build" };
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .expect("after 1970")
    .as_millis();
  let control = std::env::var_os("CRABSHELL_CONTROL").map(|path| {
    let mut stream = std::os::unix::net::UnixStream::connect(path).expect("the supervisor");
    stream.write_all(b"ready\n").expect("ready");
    stream
  });
  // After connecting, so a test that sees the start can reload it.
  let mut starts = std::fs::OpenOptions::new()
    .append(true)
    .create(true)
    .open(dir.join("starts"))
    .expect("starts");
  // One write, so a reader never sees half a line.
  let line = format!("{name} {} {now}\n", std::process::id());
  starts.write_all(line.as_bytes()).expect("written");
  match std::fs::read_to_string(dir.join("mode")).unwrap_or_default().trim() {
    "crash" => std::process::exit(1),
    "logout" => std::process::exit(2),
    "deaf" => {}
    _ => {
      use std::io::BufRead;
      for line in control.iter().flat_map(|stream| std::io::BufReader::new(stream).lines()) {
        if line.as_deref().ok() == Some("save") {
          let mut saves = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(dir.join("saves"))
            .expect("saves");
          saves.write_all(format!("{}\n", std::process::id()).as_bytes()).expect("written");
          std::process::exit(0);
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
    use std::os::unix::fs::PermissionsExt;
    let path = self.0.join("cache/crabshell/last-good");
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("the cache");
    let starts = self.config().join("starts");
    let script = format!(
      "#!/bin/sh\necho \"last-good $$ $(date +%s%3N)\" >> '{}'\nexec sleep 3600\n",
      starts.display()
    );
    std::fs::write(&path, script).expect("written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("executable");
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
  assert!(notes[0].1.contains("unclosed delimiter"), "{notes:?}");
  assert!(
    notes[0]
      .1
      .contains("; the last config that worked runs instead"),
    "{notes:?}"
  );
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
  // Long enough for the first try and the one after 1 s.
  std::thread::sleep(Duration::from_millis(2500));
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
}

#[test]
fn an_edit_saves_the_old_shell_and_starts_the_new_build() {
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
  assert_eq!(home.saves(), [first], "the old shell saved");
  assert!(!alive(first));
  std::thread::sleep(Duration::from_millis(300));
  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(5));
  assert_eq!(code, Some(0), "{log}");
  assert!(log.contains("asking the shell to save and exit"), "{log}");
  assert_eq!(log.matches("the build took").count(), 2, "{log}");
  assert!(log.contains("reloaded with"), "{log}");
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
  assert_eq!(notes[0].1, "this file contains an unclosed delimiter");
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
  // A second edit while the first reload waits doesn't put off the kill.
  std::thread::sleep(Duration::from_millis(2500));
  home.edit(|code| code + "// another\n");
  let starts = home.wait_for_starts(2, Duration::from_secs(60));
  assert_eq!(starts.len(), 2, "{starts:?}");
  let waited = starts[1].2 - starts[0].2;
  assert!((5000..7500).contains(&waited), "{waited} ms");
  assert!(!alive(first));
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
  home.edit(|code| code.replace("expect(\"starts\")", "expect(\"the starts file\")"));
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
