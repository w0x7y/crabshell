//! Phase 5's exit check: `crabshell run` on the template's bar, made with
//! `crabshell init --path`, in a private headless Sway. The first run
//! builds the whole toolkit into `target/tmp/exit-check`, which later runs
//! reuse, so it is ignored by default. Run it as
//!
//! ```sh
//! env -u SWAYSOCK cargo test -p crabshell-cli --test exit_check -- --ignored --nocapture
//! ```

#[allow(dead_code, reason = "the exit check uses part of the helpers")]
mod common;

#[allow(
  dead_code,
  reason = "the exit check uses part of the helper crabshell-wayland's tests share"
)]
#[path = "../../crabshell-wayland/tests/sway/mod.rs"]
mod sway;

use std::time::{Duration, Instant};

use common::{Home, PrivateBus, Supervised, alive, checkout, is_compiler_error};
use sway::Sway;

/// Waits up to `limit` for the `count`th line of the log holding `needle`,
/// and says when it was seen, to within 5 ms.
fn wait_for_line(supervised: &Supervised, needle: &str, count: usize, limit: Duration) -> Instant {
  let deadline = Instant::now() + limit;
  loop {
    if supervised.log().matches(needle).count() >= count {
      return Instant::now();
    }
    assert!(
      Instant::now() < deadline,
      "no {count} × {needle:?} within {limit:?}:\n{}",
      supervised.log()
    );
    std::thread::sleep(Duration::from_millis(5));
  }
}

/// The number in the `count`th line holding `needle`, the first word
/// after it.
fn number_after(log: &str, needle: &str, count: usize) -> f64 {
  let line = log
    .lines()
    .filter(|line| line.contains(needle))
    .nth(count - 1)
    .expect("the line");
  let after = &line[line.find(needle).expect("the needle") + needle.len()..];
  after
    .split_whitespace()
    .next()
    .and_then(|word| word.parse().ok())
    .expect("a number")
}

/// The supervisor's one child: the running shell.
fn shell_pid(supervised: &Supervised) -> u32 {
  let pid = supervised.pid();
  let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
    .expect("the supervisor's children");
  children
    .split_whitespace()
    .next()
    .and_then(|child| child.parse().ok())
    .expect("a running shell")
}

/// The resident memory of `pid`, in KiB.
fn rss_kib(pid: u32) -> u64 {
  let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("its status");
  status
    .lines()
    .find_map(|line| line.strip_prefix("VmRSS:"))
    .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
    .expect("VmRSS")
}

#[test]
#[ignore = "builds the toolkit and runs a headless Sway: minutes the first time"]
fn phase_5_exit_check() {
  let sway = Sway::start();
  let home = Home::new("exit-check");
  let config_home = home.0.join("config");
  let checkout = checkout().canonicalize().expect("the checkout");
  let made = home.run(&["init", "--path", checkout.to_str().expect("UTF-8")]);
  assert!(made.status.success(), "{}", common::text(&made.stderr));
  let main = home.config().join("src/main.rs");
  let edit = |change: &dyn Fn(String) -> String| {
    let code = std::fs::read_to_string(&main).expect("the bar");
    std::fs::write(&main, change(code)).expect("edited");
  };

  // A cache that outlives the test, so a later run builds only the config.
  let cache = checkout.join("target/tmp/exit-check");
  let _ = std::fs::remove_file(cache.join("crabshell/last-good"));
  let bus = PrivateBus::start(&home);
  let mut command = home.command(&["run"], config_home.to_str().expect("UTF-8"));
  command
    .env("XDG_CACHE_HOME", &cache)
    .env("XDG_RUNTIME_DIR", sway.runtime_dir())
    .env("WAYLAND_DISPLAY", sway.display())
    .env("SWAYSOCK", sway.socket())
    .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
    // A server that names nothing, so libpulse never looks for the
    // desktop's.
    .env(
      "PULSE_SERVER",
      format!(
        "unix:{}",
        sway.runtime_dir().join("no-pulse-server").display()
      ),
    );
  let supervised = home.spawn_supervisor(command);
  wait_for_line(
    &supervised,
    "the shell is ready",
    1,
    Duration::from_secs(1200),
  );
  wait_for_line(
    &supervised,
    "its build is kept as",
    1,
    Duration::from_secs(90),
  );
  let supervisor_rss = rss_kib(supervised.pid());

  // 1. A syntax error leaves the old shell running and sends a notification.
  let first = shell_pid(&supervised);
  let environment = std::fs::read(format!("/proc/{first}/environ")).expect("its environment");
  assert!(
    !environment
      .split(|&byte| byte == 0)
      .any(|entry| entry.starts_with(b"DISPLAY=")),
    "the shell could reach the desktop's X server"
  );
  edit(&|code| code + "fn broken() {\n");
  let notes = bus.wait_for(1, Duration::from_secs(300));
  assert_eq!(notes.len(), 1, "{notes:?}");
  assert_eq!(notes[0].0, "Your Crabshell config didn't build");
  assert!(
    is_compiler_error(&supervised.log(), &notes[0].1),
    "{notes:?}\n{}",
    supervised.log()
  );
  assert!(alive(first), "the old shell runs on");
  assert_eq!(supervised.log().matches("the shell is ready").count(), 1);

  // 2. Fixing it brings up the new shell within build time plus 200 ms.
  edit(&|code| code.replace("fn broken() {\n", ""));
  let built = wait_for_line(&supervised, "the build took", 3, Duration::from_secs(300));
  let ready = wait_for_line(&supervised, "reloaded with", 1, Duration::from_secs(30));
  let after_build = ready - built;
  let log = supervised.log();
  let rebuild = number_after(&log, "the build took", 3);
  let gap = number_after(&log, "reloaded with", 1);
  assert!(
    after_build <= Duration::from_millis(200),
    "the new shell was ready {after_build:?} after the build"
  );
  assert!(!alive(first), "the old shell is gone");

  // 3. After `kill -9` the shell is back within 2 s.
  let killed = Instant::now();
  let second = shell_pid(&supervised);
  let sent = std::process::Command::new("kill")
    .args(["-KILL", &second.to_string()])
    .status()
    .expect("kill runs");
  assert!(sent.success());
  let back = wait_for_line(
    &supervised,
    "the shell is ready",
    3,
    Duration::from_secs(10),
  ) - killed;
  assert!(back <= Duration::from_secs(2), "back after {back:?}");

  // 4. A config that panics at startup three times falls back to last-good.
  let panicking = Instant::now();
  edit(&|code| {
    code.replacen(
      "fn main() {",
      "fn main() {\n  panic!(\"the exit check's startup panic\");",
      1,
    )
  });
  let notes = bus.wait_for(2, Duration::from_secs(300));
  assert_eq!(notes.len(), 2, "{notes:?}");
  assert_eq!(notes[1].0, "Your Crabshell config keeps crashing");
  wait_for_line(
    &supervised,
    "the shell is ready",
    4,
    Duration::from_secs(30),
  );
  let fallback = panicking.elapsed();
  let log = supervised.log();
  assert!(log.contains("last-good starts"), "{log}");
  assert_eq!(log.matches("panicked at src/main.rs").count(), 3, "{log}");

  // 5. So does a config whose bars all panic at startup: the shell has
  // nothing to show and exits with 1.
  let bars_panicking = Instant::now();
  edit(&|code| {
    code
      .replace("\n  panic!(\"the exit check's startup panic\");", "")
      .replacen(
        "-> PanelWindow {",
        "-> PanelWindow {\n    panic!(\"the exit check's bar panic\");",
        1,
      )
  });
  let notes = bus.wait_for(3, Duration::from_secs(300));
  assert_eq!(notes.len(), 3, "{notes:?}");
  assert_eq!(notes[2].0, "Your Crabshell config keeps crashing");
  wait_for_line(
    &supervised,
    "the shell is ready",
    5,
    Duration::from_secs(30),
  );
  let bars_fallback = bars_panicking.elapsed();
  let log = supervised.log();
  assert_eq!(log.matches("has nothing to show").count(), 3, "{log}");
  assert_eq!(
    log
      .matches("per_output[HEADLESS-1] (called at src/main.rs:")
      .count(),
    3,
    "{log}"
  );

  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(10));
  assert_eq!(code, Some(0), "{log}");

  let results = format!(
    "Phase 5 exit check, template bar on one headless Sway output:\n\
     rebuild after a one-line fix: {rebuild:.1} s\n\
     the new shell ready after the build ended: {} ms\n\
     reload gap, old shell's exit to the new one's ready: {gap} ms\n\
     back after kill -9: {} ms\n\
     edit to last-good ready after three startup panics: {:.1} s\n\
     edit to last-good ready after three starts whose bars all panic: {:.1} s\n\
     supervisor RSS with the shell up: {supervisor_rss} KiB\n",
    after_build.as_millis(),
    back.as_millis(),
    fallback.as_secs_f64(),
    bars_fallback.as_secs_f64(),
  );
  std::fs::write(cache.join("results.txt"), &results).expect("the results");
  println!("{results}");
}
