//! Phase 2's exit criteria on headless Sway, one Sway per test: the
//! counter example matches its goldens at scales 1, 1.5 and 2, before and
//! after a click; an idle bar wakes nothing, by its own counters and from
//! outside; unplugging an output drops its bar and frees its nodes. And
//! Phase 3's: the v0.1 bar example shows the private Sway's workspaces,
//! switches them on a click, logs each switch's time to the screen, wakes
//! only on the minute with audio connected, and its volume button agrees
//! with a private pipewire-pulse through scrolls, clicks and changes made
//! elsewhere.
//!
//! Anything that asks for `cx.sway()` or `cx.audio()` runs as its own
//! process, given the private Sway's socket and a `PULSE_SERVER` that is
//! private or names nothing: in this process, libpulse would look for the
//! desktop's audio server.
//!
//! They need `sway`, `swaymsg` and `grim`, and the audio ones `pipewire`
//! and `pactl`, so they are ignored by default. Run them without the
//! session's `SWAYSOCK`, which `Sway::start()` refuses:
//!
//! ```text
//! env -u SWAYSOCK cargo test -p crabshell --test headless -- --ignored --test-threads=1
//! ```
//!
//! Goldens are written again, on purpose only, with `CRABSHELL_BLESS=1`.

#[allow(
  dead_code,
  reason = "these tests use part of the helper crabshell-wayland's share"
)]
#[path = "../../crabshell-wayland/tests/sway/mod.rs"]
mod sway;

#[allow(
  dead_code,
  reason = "the example's bar is tested; its main isn't run here"
)]
#[path = "../examples/counter.rs"]
mod counter;

#[allow(
  dead_code,
  reason = "the example's bar is tested; its main isn't run here"
)]
#[path = "../examples/bar.rs"]
mod v0_1;

#[allow(
  dead_code,
  reason = "the example's launcher is tested; its main isn't run here"
)]
#[path = "../examples/launcher.rs"]
mod launcher;

#[allow(
  dead_code,
  reason = "the example's bar is tested; its main isn't run here"
)]
#[path = "../examples/effects.rs"]
mod effects;

#[allow(
  dead_code,
  reason = "the example's windows are tested; its main isn't run here"
)]
#[path = "../examples/motion.rs"]
mod motion;

#[allow(
  dead_code,
  reason = "the example's panel is tested; its main isn't run here"
)]
#[path = "../examples/control.rs"]
mod control;

#[allow(
  dead_code,
  reason = "the example's dashboard is tested; its main isn't run here"
)]
#[path = "../examples/dashboard.rs"]
mod dashboard;

#[allow(
  dead_code,
  reason = "the example's panel is tested; its main isn't run here"
)]
#[path = "../examples/notes.rs"]
mod notes;

#[allow(
  dead_code,
  reason = "the example's lock screen is tested; its main isn't run here"
)]
#[path = "../examples/lock.rs"]
mod lock;

#[allow(
  dead_code,
  reason = "the example's bar is tested; its main isn't run here"
)]
#[path = "../examples/system.rs"]
mod system;

#[allow(dead_code, reason = "the services' own tests use the rest")]
#[path = "../../crabshell-services/tests/pipewire/mod.rs"]
mod pipewire;

#[allow(dead_code, reason = "these tests use part of the fake")]
#[path = "../../crabshell-wayland/tests/fake/mod.rs"]
mod fake;

use std::cell::Cell;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader};
use std::ops::Range;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use cosmic_text::fontdb::Source;
use crabshell::Shell;
use crabshell::prelude::*;
use crabshell_reactive::Runtime;
use crabshell_ui::{Content, NodeView, Rect};
use crabshell_wayland::{EventLoop, Platform, WindowId};
use pipewire::PrivateServer;
use sway::{AppWindow, Image, InputMethod, Sway, VirtualKeyboard, VirtualPointer, keys};

/// How long any wait may take before the test fails.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A shell running the counter example's bar on every output.
fn start(sway: &Sway) -> Shell {
  Shell::new(sway.connect(), |cx| cx.per_output(counter::bar)).expect("the root closure runs")
}

/// Dispatches until `done` holds, failing the test after ten seconds.
fn run_until(shell: &mut Shell, what: &str, mut done: impl FnMut(&mut Platform) -> bool) {
  let deadline = Instant::now() + TIMEOUT;
  while !done(shell.platform()) {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    shell
      .dispatch(Some(Duration::from_millis(20)))
      .expect("the connection works");
  }
}

/// Dispatches until no frame has been committed for half a second.
fn settle(shell: &mut Shell) {
  let deadline = Instant::now() + TIMEOUT;
  let mut commits = shell.platform().stats().commits;
  let mut quiet_since = Instant::now();
  while quiet_since.elapsed() < Duration::from_millis(500) {
    assert!(Instant::now() < deadline, "the bars never stopped drawing");
    shell
      .dispatch(Some(Duration::from_millis(20)))
      .expect("the connection works");
    let now = shell.platform().stats().commits;
    if now != commits {
      commits = now;
      quiet_since = Instant::now();
    }
  }
}

/// The bar on `output`, if it has committed a frame.
fn shown_on(platform: &Platform, output: &str) -> Option<WindowId> {
  platform
    .windows()
    .filter(|(_, window)| window.output().name() == output)
    .map(|(id, _)| id)
    .find(|id| {
      platform
        .window_info(*id)
        .is_some_and(|info| info.commits > 0)
    })
}

/// The `n`th child of the bar's row.
fn in_row<T>(
  platform: &Platform,
  id: WindowId,
  n: usize,
  read: impl FnOnce(NodeView<'_>) -> T,
) -> T {
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the bar is held");
  let view = window.view();
  let row = view.root().children().next().expect("the bar has a row");
  read(row.children().nth(n).expect("the row holds that child"))
}

/// The counter button: the bar's row's first child.
fn button<T>(platform: &Platform, id: WindowId, read: impl FnOnce(NodeView<'_>) -> T) -> T {
  in_row(platform, id, 0, read)
}

/// A button's label, as laid out.
fn text_of(button: NodeView<'_>) -> String {
  let text = button
    .children()
    .next()
    .expect("the button holds its label");
  let Content::Text(text) = text.content() else {
    panic!("the label is text");
  };
  text.runs().map(|run| run.text).collect()
}

/// The counter button's label.
fn label(platform: &Platform, id: WindowId) -> String {
  button(platform, id, text_of)
}

/// The middle of `rect`, in whole logical pixels.
fn middle(
  Rect {
    x,
    y,
    width,
    height,
  }: Rect,
) -> (u32, u32) {
  ((x + width / 2.0) as u32, (y + height / 2.0) as u32)
}

/// The middle of the counter button, in logical pixels.
fn button_center(platform: &Platform, id: WindowId) -> (u32, u32) {
  button(platform, id, |button| middle(button.rect()))
}

/// Fails unless the label's font is Noto Sans, which `sans-serif` was when
/// the goldens were drawn: other fonts draw other pixels.
fn check_fonts(shell: &mut Shell, id: WindowId) {
  let font = button(shell.platform(), id, |button| {
    let text = button
      .children()
      .next()
      .expect("the button holds its label");
    let Content::Text(text) = text.content() else {
      panic!("the label is text");
    };
    text
      .runs()
      .flat_map(|run| run.glyphs.into_owned())
      .next()
      .map(|glyph| glyph.font_id)
  })
  .expect("the label has glyphs");
  let family = shell
    .platform()
    .text_system()
    .font_system()
    .db()
    .face(font)
    .and_then(|face| face.families.first())
    .map(|(name, _)| name.clone())
    .unwrap_or_default();
  assert_eq!(
    family, "Noto Sans",
    "the goldens were drawn with Noto Sans for `sans-serif`, but fontconfig picks \
         {family:?} here (see `fc-match sans-serif`); they can't match"
  );
}

/// Checks the top `rows` of `shot` against `tests/golden/<name>.png`,
/// within 2/255 per channel, or writes the golden when `CRABSHELL_BLESS`
/// is set. On a mismatch the screenshot is saved under the target
/// directory.
fn check_golden(name: &str, shot: &Image, rows: u32) {
  assert!(
    shot.height >= rows,
    "{name}: the screenshot is shorter than the bar"
  );
  let top = Image {
    rgba: shot.rgba[..shot.width as usize * rows as usize * 4].to_vec(),
    width: shot.width,
    height: rows,
  };
  let path = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests/golden")
    .join(format!("{name}.png"));
  if std::env::var_os("CRABSHELL_BLESS").is_some() {
    std::fs::create_dir_all(path.parent().expect("a directory")).expect("create tests/golden");
    top.save(&path);
    return;
  }
  let bytes = std::fs::read(&path).unwrap_or_else(|_| {
    panic!(
      "no golden {}; run with CRABSHELL_BLESS=1 to write it",
      path.display()
    )
  });
  let failure = golden_difference(&Image::decode(&bytes), &top);
  if let Some(failure) = failure {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("headless");
    std::fs::create_dir_all(&dir).expect("create the failure directory");
    let saved = dir.join(format!("{name}.png"));
    top.save(&saved);
    panic!(
      "{name} doesn't match its golden: {failure}; the screenshot is in {}",
      saved.display()
    );
  }
}

/// How `shot` differs from `golden`, or `None` when every pixel is within
/// 2/255.
fn golden_difference(golden: &Image, shot: &Image) -> Option<String> {
  if (golden.width, golden.height) != (shot.width, shot.height) {
    return Some(format!(
      "size {}x{}, golden {}x{}",
      shot.width, shot.height, golden.width, golden.height
    ));
  }
  let differing = shot
    .rgba
    .as_chunks::<4>()
    .0
    .iter()
    .zip(golden.rgba.as_chunks::<4>().0)
    .filter(|(shot, golden)| {
      shot
        .iter()
        .zip(golden.iter())
        .any(|(a, b)| a.abs_diff(*b) > 2)
    })
    .count();
  (differing > 0).then(|| format!("{differing} pixels differ by more than 2/255"))
}

#[test]
#[ignore = "needs headless Sway"]
fn the_counter_bar_matches_its_goldens_before_and_after_a_click() {
  for (scale, factor, suffix) in [("1", 1.0, "1x"), ("1.5", 1.5, "1.5x"), ("2", 2.0, "2x")] {
    let sway = Sway::start();
    // 1920 by 1080 is a whole number of logical pixels at every scale.
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let mut shell = start(&sway);
    run_until(&mut shell, "the bar's first frame", |platform| {
      shown_on(platform, "HEADLESS-1").is_some()
    });
    settle(&mut shell);
    let id = shown_on(shell.platform(), "HEADLESS-1").expect("the bar is shown");
    check_fonts(&mut shell, id);
    assert_eq!(label(shell.platform(), id), "clicked 0 times");
    let rows = (30.0 * factor) as u32;
    check_golden(
      &format!("counter@{suffix}"),
      &sway.screenshot("HEADLESS-1"),
      rows,
    );

    // A virtual pointer gives the seat a pointer; the platform takes it
    // while the loop runs.
    let mut pointer = VirtualPointer::new(&sway);
    settle(&mut shell);
    let (x, y) = button_center(shell.platform(), id);
    let layout = ((1920.0 / factor) as u32, (1080.0 / factor) as u32);
    pointer.move_to(x, y, layout);
    pointer.click();
    run_until(&mut shell, "the click to count", |platform| {
      label(platform, id) == "clicked once"
    });
    settle(&mut shell);
    // The pointer is still over the button, so it shows its hover style.
    check_golden(
      &format!("counter-clicked@{suffix}"),
      &sway.screenshot("HEADLESS-1"),
      rows,
    );
  }
}

/// An example as its own program, the way `run` runs it, with its log
/// lines read as they come.
struct ExampleBar {
  child: Child,
  lines: mpsc::Receiver<String>,
}

impl ExampleBar {
  /// Builds the example `name` (in release if `release`) and starts it on
  /// `sway`, with `extra` in its environment.
  fn start(sway: &Sway, name: &str, release: bool, extra: &[(&str, &str)]) -> ExampleBar {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = Command::new(cargo);
    build
      .args(["build", "-q", "-p", "crabshell", "--example", name])
      .current_dir(&workspace);
    if release {
      build.arg("--release");
    }
    assert!(
      build.status().is_ok_and(|status| status.success()),
      "the {name} example builds"
    );
    let target = std::env::var_os("CARGO_TARGET_DIR")
      .map(PathBuf::from)
      .unwrap_or_else(|| workspace.join("target"));
    let program = target
      .join(if release { "release" } else { "debug" })
      .join("examples")
      .join(name);
    ExampleBar::spawn(sway, Command::new(program), extra)
  }

  /// Starts `command` as a shell on `sway`, with `extra` in its otherwise
  /// cleared environment, and its log read as it comes. Its `SWAYSOCK`
  /// is the private Sway's, never this process's. Its `PULSE_SERVER` is
  /// a socket that doesn't exist, unless `extra` names a private server:
  /// unset, libpulse could find the desktop's through HOME's client.conf.
  /// Its `DBUS_SYSTEM_BUS_ADDRESS` is one that doesn't exist either,
  /// unless `extra` gives one.
  /// Its `PATH` is this process's, unless `extra` gives one. Its
  /// `XDG_CONFIG_HOME` holds only a link to the user's fontconfig, unless
  /// `extra` gives one, so the goldens' fonts stay and the user's GTK
  /// icon theme stays out.
  fn spawn(sway: &Sway, mut command: Command, extra: &[(&str, &str)]) -> ExampleBar {
    command.env_clear();
    let nowhere = sway.runtime_dir().join("no-pulse-server");
    command.env("PULSE_SERVER", format!("unix:{}", nowhere.display()));
    // Never the desktop's system bus, where the lock example would ask
    // logind for an inhibitor.
    let no_bus = sway.runtime_dir().join("no-system-bus");
    command.env(
      "DBUS_SYSTEM_BUS_ADDRESS",
      format!("unix:path={}", no_bus.display()),
    );
    let config = sway.runtime_dir().join("xdg-config");
    if let Some(fontconfig) = user_config().map(|dir| dir.join("fontconfig"))
      && std::fs::create_dir_all(&config).is_ok()
    {
      let _ = std::os::unix::fs::symlink(fontconfig, config.join("fontconfig"));
    }
    command.env("XDG_CONFIG_HOME", &config);
    command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
    for (name, value) in extra {
      command.env(name, value);
    }
    let name = Path::new(command.get_program())
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_default();
    let mut child = command
      .env("HOME", std::env::var_os("HOME").unwrap_or_default())
      .env("XDG_RUNTIME_DIR", sway.runtime_dir())
      .env("WAYLAND_DISPLAY", sway.display())
      .env("SWAYSOCK", sway.socket())
      .env("CRABSHELL_LOG", "info")
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::piped())
      .spawn()
      .unwrap_or_else(|error| panic!("start {command:?}: {error}"));
    let stderr = child.stderr.take().expect("stderr is piped");
    let (sender, lines) = mpsc::channel();
    std::thread::spawn(move || {
      for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        eprintln!("{name}: {line}");
        if sender.send(line).is_err() {
          break;
        }
      }
    });
    ExampleBar { child, lines }
  }

  /// The first log line containing `needle`.
  fn wait_for(&self, needle: &str) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
      let left = deadline.saturating_duration_since(Instant::now());
      match self.lines.recv_timeout(left) {
        Ok(line) if line.contains(needle) => return line,
        Ok(_) => {}
        Err(_) => panic!("the shell never logged {needle:?}"),
      }
    }
  }

  /// The first frame's time after `run()`, from its log line, in ms.
  fn first_frame_ms(&self) -> f64 {
    ms_after(&self.wait_for(FIRST_FRAME), FIRST_FRAME)
  }

  /// The log lines that came and weren't read yet.
  fn drain(&self) -> Vec<String> {
    self.lines.try_iter().collect()
  }

  /// Its exit status, waiting up to ten seconds; `None` if it is still
  /// running then.
  fn exit_status(&mut self) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
      if let Ok(Some(status)) = self.child.try_wait() {
        return Some(status);
      }
      std::thread::sleep(Duration::from_millis(50));
    }
    None
  }

  /// Context switches of all its threads so far: each is a wakeup.
  fn context_switches(&self) -> u64 {
    self.switches_by_thread().values().sum()
  }

  /// Each live thread's context switches so far, by thread id. A thread
  /// that exits takes its count with it, so wakeups are counted per
  /// thread, never from a total that can drop.
  fn switches_by_thread(&self) -> HashMap<String, u64> {
    let tasks = format!("/proc/{}/task", self.child.id());
    std::fs::read_dir(&tasks)
      .expect("the example is running")
      .filter_map(Result::ok)
      .filter_map(|task| {
        let status = std::fs::read_to_string(task.path().join("status")).ok()?;
        let switches = status
          .lines()
          .filter(|line| line.contains("ctxt_switches"))
          .filter_map(|line| line.split_whitespace().last()?.parse::<u64>().ok())
          .sum();
        Some((task.file_name().to_string_lossy().into_owned(), switches))
      })
      .collect()
  }

  /// Waits until its context switches stop changing for a second.
  fn wait_until_quiet(&self) {
    let deadline = Instant::now() + TIMEOUT;
    let mut last = self.context_switches();
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < Duration::from_secs(1) {
      assert!(Instant::now() < deadline, "the example never went quiet");
      std::thread::sleep(Duration::from_millis(100));
      let now = self.context_switches();
      if now != last {
        last = now;
        quiet_since = Instant::now();
      }
    }
  }

  /// Watches its context switches for `time`, every 50 ms. Returns each
  /// change seen: when, in seconds past the wall clock's minute, and how
  /// many switches it was.
  fn wakeups_for(&self, time: Duration) -> Vec<(f64, u64)> {
    let end = Instant::now() + time;
    let mut last = self.switches_by_thread();
    let mut seen = Vec::new();
    while Instant::now() < end {
      std::thread::sleep(Duration::from_millis(50));
      let now = self.switches_by_thread();
      // A new thread's switches all count; an exited one's drop out.
      let new: u64 = now
        .iter()
        .map(|(thread, switches)| switches.saturating_sub(last.get(thread).copied().unwrap_or(0)))
        .sum();
      if new > 0 {
        let wall = SystemTime::now()
          .duration_since(SystemTime::UNIX_EPOCH)
          .expect("the clock is past 1970");
        seen.push((wall.as_secs_f64() % 60.0, new));
      }
      last = now;
    }
    seen
  }

  /// Waits until `server` lists it as a client.
  fn wait_until_connected(&self, server: &PrivateServer) {
    let deadline = Instant::now() + TIMEOUT;
    let me = format!("application.process.id = \"{}\"", self.child.id());
    loop {
      let clients = server.pactl(&["list", "clients"]);
      if clients.contains(&me) {
        return;
      }
      assert!(
        Instant::now() < deadline,
        "it never connected to the private audio server: {clients}"
      );
      std::thread::sleep(Duration::from_millis(50));
    }
  }

  /// Waits for the v0.1 bar run by [`run_v0_1_bar_logging_its_volume`] to log
  /// its volume button saying `label`, and returns the button's middle.
  fn volume_says(&self, label: &str) -> (u32, u32) {
    let line = self.wait_for(&format!(" says {label}"));
    line
      .split(VOLUME)
      .nth(1)
      .and_then(|rest| rest.split(" says").next()?.split_once(','))
      .and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))
      .unwrap_or_else(|| panic!("no volume button's middle in {line:?}"))
  }

  /// Its resident memory in KiB, from `ps -o rss=`.
  fn rss_kib(&self) -> u64 {
    let output = Command::new("ps")
      .args(["-o", "rss=", "-p", &self.child.id().to_string()])
      .output()
      .expect("ps runs");
    String::from_utf8_lossy(&output.stdout)
      .trim()
      .parse()
      .expect("ps prints the RSS")
  }
}

impl Drop for ExampleBar {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

/// A program whose worker thread wakes 50 times and exits, as a decode
/// thread does, counts those wakeups and nothing back when it goes.
#[test]
fn wakeups_are_counted_per_thread_so_an_exiting_one_takes_none_back() {
  const TEST: &str = "wakeups_are_counted_per_thread_so_an_exiting_one_takes_none_back";
  run_as_child();
  let child = Command::new(std::env::current_exe().expect("the test binary"))
    .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
    .env(CHILD, "a thread exits")
    .stdout(Stdio::null())
    .spawn()
    .expect("the test binary runs again");
  let (_sender, lines) = mpsc::channel();
  let program = ExampleBar { child, lines };
  let woke: u64 = program
    .wakeups_for(Duration::from_millis(1500))
    .iter()
    .map(|(_, switches)| switches)
    .sum();
  assert!(woke >= 20, "{woke} wakeups seen from the worker");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_shell_never_looks_for_the_desktops_audio_server() {
  let sway = Sway::start();
  let mut sleep = Command::new("sleep");
  sleep.arg("10");
  let shell = ExampleBar::spawn(&sway, sleep, &[]);
  let environ =
    std::fs::read(format!("/proc/{}/environ", shell.child.id())).expect("the shell's environment");
  let server = environ
    .split(|byte| *byte == 0)
    .find_map(|entry| entry.strip_prefix(b"PULSE_SERVER="))
    .map(|server| String::from_utf8_lossy(server).into_owned())
    .expect("PULSE_SERVER is set, so libpulse never looks elsewhere");
  let socket = server.strip_prefix("unix:").expect("a unix socket");
  assert!(
    Path::new(socket).starts_with(sway.runtime_dir()) && !Path::new(socket).exists(),
    "{server} is a socket in the private Sway's directory that doesn't exist"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn an_idle_counter_bar_wakes_nothing() {
  let sway = Sway::start();
  // In this process: with nothing to do, one dispatch waits out its whole
  // timeout, and the platform's counters stand still.
  let mut shell = start(&sway);
  run_until(&mut shell, "the bar's first frame", |platform| {
    shown_on(platform, "HEADLESS-1").is_some()
  });
  settle(&mut shell);
  let before = shell.platform().stats();
  let waited = Instant::now();
  shell
    .dispatch(Some(Duration::from_secs(5)))
    .expect("the connection works");
  let waited = waited.elapsed();
  assert!(
    waited >= Duration::from_millis(4950),
    "something woke the loop after {waited:?}"
  );
  let after = shell.platform().stats();
  assert_eq!(
    after.wayland_wakeups, before.wayland_wakeups,
    "Wayland events"
  );
  assert_eq!(after.commits, before.commits, "frames");
  // Nor did the switch log start the Sway service to read it.
  assert!(crabshell_services::Sway::started(shell.cx()).is_none());
  drop(shell);

  // From outside, for a minute: the example program, sleeping in the
  // loop `run` runs, gets no context switches at all. A timer as slow as
  // one a minute would show.
  let bar = ExampleBar::start(&sway, "counter", false, &[]);
  bar.wait_for("first frame committed");
  bar.wait_until_quiet();
  let before = bar.context_switches();
  std::thread::sleep(Duration::from_secs(60));
  assert_eq!(
    bar.context_switches(),
    before,
    "context switches in 60 idle seconds"
  );
}

/// The font file the bundled-font tests read, as a config would bundle it.
const BUNDLED_FONT: &str = "/usr/share/fonts/dejavu/DejaVuSans.ttf";

#[test]
#[ignore = "needs headless Sway"]
fn a_bundled_font_lays_out_the_bar_and_measures_with_it() {
  // Leaked, as `include_bytes!` would keep them.
  let Ok(bytes) = std::fs::read(BUNDLED_FONT) else {
    eprintln!("skipped: no {BUNDLED_FONT}");
    return;
  };
  let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
  let font = Font::new("DejaVu Sans", 13.0);
  let sway = Sway::start();
  let measured = Rc::new(Cell::new(None));
  let mut shell = Shell::new(sway.connect(), |cx| {
    cx.add_font(bytes);
    measured.set(Some(cx.measure_text("clicked 0 times", font)));
    cx.per_output(move |cx, output| counter::bar(cx, output).style(Style::new().font(font)));
  })
  .expect("the root closure runs");
  run_until(&mut shell, "the bar", |platform| {
    shown_on(platform, "HEADLESS-1").is_some()
  });
  let id = shown_on(shell.platform(), "HEADLESS-1").expect("the bar is shown");
  // Every face is the bundled one's: the platform lays out with the fonts
  // the root context added to, and fontconfig loaded no file.
  let fonts = shell.platform().text_system().font_system();
  let bundled = |source: &Source| matches!(source, Source::Binary(_));
  let files = fonts.db().faces().filter(|face| !bundled(&face.source));
  assert_eq!(files.count(), 0, "faces were loaded from files");
  assert!(!fonts.db().is_empty());
  drop(fonts);
  assert_eq!(label(shell.platform(), id), "clicked 0 times");
  let width = button(shell.platform(), id, |button| {
    button.children().next().expect("the label").rect().width
  });
  let measured = measured.get().expect("root measured");
  assert_eq!(measured.width.ceil(), width);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_click_on_a_link_in_a_button_runs_on_link_at_each_scale() {
  for (scale, factor) in [("1", 1.0), ("1.5", 1.5)] {
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let opened = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let clicks = Rc::new(Cell::new(0));
    let (log, count) = (Rc::clone(&opened), Rc::clone(&clicks));
    let mut shell = Shell::new(sway.connect(), move |cx| {
      let (log, count) = (Rc::clone(&log), Rc::clone(&count));
      cx.per_output(move |_cx, output| {
        let (log, count) = (Rc::clone(&log), Rc::clone(&count));
        let body = parse_markup("read <a href=\"https://example.org\">the notes</a>");
        let text = Text::rich(body).on_link(move |_cx, url| log.borrow_mut().push(url.to_owned()));
        PanelWindow::new(output)
          .anchors(Edge::Top | Edge::Left | Edge::Right)
          .height(30)
          .child(
            Row::new()
              .padding(4, 8)
              .align(Align::Center)
              .child(Button::new(text).on_click(move |_| count.set(count.get() + 1))),
          )
      });
    })
    .expect("the root closure runs");
    run_until(&mut shell, "the bar's first frame", |platform| {
      shown_on(platform, "HEADLESS-1").is_some()
    });
    settle(&mut shell);
    let id = shown_on(shell.platform(), "HEADLESS-1").expect("the bar is shown");
    // The middle of the link's glyphs, where layout put them.
    let (x, y) = button(shell.platform(), id, |button| {
      let label = button.children().next().expect("the button holds its text");
      let Content::Text(text) = label.content() else {
        panic!("the label is text");
      };
      let run = text.runs().next().expect("one line");
      let link: Vec<_> = run.glyphs.iter().filter(|g| g.metadata == 2).collect();
      let (first, last) = (link[0], link[link.len() - 1]);
      let origin = text.origin();
      (
        (origin.x + (first.x + last.x + last.w) / 2.0) as u32,
        (origin.y + run.line_top + run.line_height / 2.0) as u32,
      )
    });
    let mut pointer = VirtualPointer::new(&sway);
    settle(&mut shell);
    let layout = ((1920.0 / factor) as u32, (1080.0 / factor) as u32);
    pointer.move_to(x, y, layout);
    pointer.click();
    run_until(&mut shell, "the link to open", |_| {
      !opened.borrow().is_empty()
    });
    assert_eq!(*opened.borrow(), ["https://example.org"], "at {scale}");
    assert_eq!(clicks.get(), 0, "the button got the click at {scale}");
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn unplugging_an_output_drops_its_bar_and_frees_its_nodes() {
  let sway = Sway::start();
  let runtime = Runtime::current();
  let mut shell = start(&sway);
  run_until(&mut shell, "the first bar", |platform| {
    shown_on(platform, "HEADLESS-1").is_some()
  });
  settle(&mut shell);
  runtime.flush();
  let nodes = runtime.node_count();

  sway.msg(&["create_output"]);
  run_until(&mut shell, "a bar on the new output", |platform| {
    shown_on(platform, "HEADLESS-2").is_some()
  });
  settle(&mut shell);
  assert!(runtime.node_count() > nodes, "the second bar has nodes");

  // Sway closes the bar's surface before it removes the output, so this
  // takes per_output's closed-window path; its unit tests guard the
  // removal path.
  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  run_until(&mut shell, "the second bar to go", |platform| {
    platform.windows().count() == 1 && platform.outputs().all(|output| output.name != "HEADLESS-2")
  });
  runtime.flush();
  // All but HEADLESS-2's persisted click count, which the shell keeps for
  // when it comes back.
  assert_eq!(
    runtime.node_count(),
    nodes + 1,
    "per_output's owner for HEADLESS-2 was disposed"
  );
}

#[test]
#[ignore = "needs headless Sway; measures only with CRABSHELL_MEASURE set"]
fn measure_the_counter_bar() {
  run_as_child();
  if std::env::var_os("CRABSHELL_MEASURE").is_none() {
    eprintln!("not measuring: set CRABSHELL_MEASURE=1 to, which takes two minutes");
    return;
  }
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let counter = ExampleBar::start(&sway, "counter", true, &[]);
  let mut report = format!("counter example, {}\n", measure(&counter));
  drop(counter);
  // Then the same bar in a font bundled with `add_font`, run as this test
  // binary, so as a release build only under `cargo test --release`.
  let bundled = ExampleBar::spawn(
    &sway,
    this_as_shell("measure_the_counter_bar"),
    &[(CHILD, "bundled counter")],
  );
  let build = if cfg!(debug_assertions) {
    "a DEBUG test binary"
  } else {
    "the release test binary"
  };
  report += &format!(
    "counter bar in DejaVu Sans bundled with add_font, run from {build}, {}\n",
    measure_without_idle(&bundled)
  );
  drop(bundled);
  // Then the counter bar following PipeWire's graph over the native
  // protocol, against one following the default sink through libpulse,
  // on one private server, run as this test binary as above.
  let server = PrivateServer::start("measure-graph");
  let socket = server.pipewire_socket();
  let runtime = socket.parent().expect("the runtime directory");
  let runtime = runtime.display().to_string();
  for (child, connected, what) in [
    (
      "pipewire counter",
      "the graph has",
      "following PipeWire's graph natively",
    ),
    (
      "audio counter",
      "the default sink is",
      "following the default sink through libpulse",
    ),
  ] {
    let env = [
      (CHILD, child),
      ("PIPEWIRE_RUNTIME_DIR", runtime.as_str()),
      ("PULSE_SERVER", &server.address()),
    ];
    let bar = ExampleBar::spawn(&sway, this_as_shell("measure_the_counter_bar"), &env);
    // Its service may connect before the first frame or after it.
    let (mut first_frame_ms, mut seen) = (None, false);
    while first_frame_ms.is_none() || !seen {
      let line = bar.wait_for("");
      seen |= line.contains(connected);
      if line.contains(FIRST_FRAME) {
        first_frame_ms = Some(ms_after(&line, FIRST_FRAME));
      }
    }
    std::thread::sleep(Duration::from_secs(60));
    let rss = bar.rss_kib();
    report += &format!(
      "counter bar {what}, run from {build}, one 1920x1080 headless output at scale 1: \
       first frame {:.1} ms after run(), RSS {rss} KiB ({:.1} MiB) after 60 s\n",
      first_frame_ms.expect("seen above"),
      rss as f64 / 1024.0
    );
  }
  drop(server);
  // Then the v0.1 bar, on workspace 1, then switching between 1 and 2. Its
  // tray runs the watcher on a private session bus.
  let server = PrivateServer::start("measure");
  let bus = PrivateBus::start();
  let bar = ExampleBar::start(
    &sway,
    "bar",
    true,
    &[
      ("PULSE_SERVER", &server.address()),
      ("DBUS_SESSION_BUS_ADDRESS", &bus.address),
    ],
  );
  bar.wait_until_connected(&server);
  let idle = measure(&bar);
  let times = switch_workspaces(&sway, &bar, 10);
  let slowest = times.iter().copied().fold(0.0, f64::max);
  report += &format!(
    "bar example with audio on a private pipewire-pulse and the tray on a private bus, \
         {idle}; workspace switch to \
         screen: median {:.1} ms, slowest {slowest:.1} ms over {} switches, each {times:.1?} ms\n",
    median(&times),
    times.len()
  );
  drop(bar);
  // Then Phase 17's notes example on the same bus. Its clock shows
  // seconds, so it never rests a whole second; stopped, it shows what else
  // wakes it.
  let notes = ExampleBar::start(
    &sway,
    "notes",
    true,
    &[
      ("DBUS_SESSION_BUS_ADDRESS", &bus.address),
      ("NOTES_NOW", NOTES_NOW),
    ],
  );
  report += &format!(
    "notes example with its clock stopped and notifications on a private bus, {}\n",
    measure(&notes)
  );
  drop(notes);
  // Then Phase 9's desktop example, with every service it uses running
  // on private buses with no daemons.
  let desktop = Desktop::new("measure-desktop");
  let started = desktop.start(&sway);
  report += &format!(
    "desktop example with every Phase 9 service started, {}\n",
    measure(&started)
  );
  drop(started);
  // Then Phase 10's overview, opened over IPC at once, listing its
  // applications and another program's window, with the wallpaper's
  // colors and `uname`. Its first frame is the opened overview's.
  let overview = Overview::new();
  let _window = AppWindow::new(&sway, "org.example.notes", "notes");
  let opened = overview.start(&sway);
  ipc_call(&sway, "overview", "open");
  report += &format!("overview example opened over IPC, {}\n", measure(&opened));
  drop(opened);
  // Then Phase 11's lock screen, locked over IPC at once. Nobody types, so
  // PAM never runs. Its first frame is the lock surface's.
  let lock = ExampleBar::start(&sway, "lock", true, &[]);
  ipc_call(&sway, "lock", "on");
  report += &format!("lock example locked over IPC, {}\n", measure(&lock));
  drop(lock);
  // Then Phase 15's control center, on a private audio server and session
  // bus, with today's date and its midnight timer, as a user runs it.
  let desktop = Desktop::new("measure-control");
  let control = desktop.run(&sway, "control");
  report += &format!(
    "control example with audio on a private pipewire-pulse and notifications on a private bus, {}\n",
    measure(&control)
  );
  drop(control);
  // Then Phase 16's dashboard over a 1920x1080 wallpaper, on private buses
  // with no UPower and no player. It samples the CPU each second while it
  // shows, so it is hidden over IPC to be idle.
  let mut desktop = Desktop::new("measure-dashboard");
  let files = DashboardFiles::new();
  desktop
    .env
    .push(("WALLPAPER", files.wallpaper_1080p().display().to_string()));
  let dashboard = desktop.run(&sway, "dashboard");
  // Shown, it never rests, so its wakeups are counted from the first frame.
  let first_frame_ms = dashboard.first_frame_ms();
  let shown: u64 = dashboard
    .wakeups_for(Duration::from_secs(60))
    .iter()
    .map(|(_, switches)| switches)
    .sum();
  report += &format!(
    "dashboard example over a 1920x1080 wallpaper, release build, one 1920x1080 headless \
     output at scale 1: first frame {first_frame_ms:.1} ms after run(); shown and sampling \
     the CPU, RSS {:.1} MiB after 60 s and {shown} context switches in them",
    dashboard.rss_kib() as f64 / 1024.0
  );
  ipc_call(&sway, "dashboard", "close");
  dashboard.wait_until_quiet();
  let wakeups = dashboard.wakeups_for(Duration::from_secs(60));
  let idle: u64 = wakeups.iter().map(|(_, switches)| switches).sum();
  report += &format!(
    "; then hidden over IPC: RSS {:.1} MiB, {idle} context switches in 60 idle s\n",
    dashboard.rss_kib() as f64 / 1024.0
  );
  drop(dashboard);
  // Then Phase 18's system bar, with workspaces from a fake niri and the
  // weather from a local server. It samples its stats every 2 s, so its
  // wakeups are counted from the first frame.
  let niri = FakeNiri::start(&sway);
  let server = LocalServer::start(vec![("/weather", WEATHER.as_bytes().to_vec())]);
  let folder = wallpaper_folder(&sway);
  let env = system_env(&niri, &server, &folder, "");
  let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
  let system = ExampleBar::start(&sway, "system", true, &env);
  let first_frame_ms = system.first_frame_ms();
  let woke: u64 = system
    .wakeups_for(Duration::from_secs(60))
    .iter()
    .map(|(_, switches)| switches)
    .sum();
  report += &format!(
    "system example with a fake niri and the weather from a local server, release build, one \
     1920x1080 headless output at scale 1: first frame {first_frame_ms:.1} ms after run(); \
     sampling its stats every 2 s, RSS {:.1} MiB after 60 s and {woke} context switches in them\n",
    system.rss_kib() as f64 / 1024.0
  );
  eprint!("{report}");
  let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("headless");
  std::fs::create_dir_all(&dir).expect("create the output directory");
  std::fs::write(dir.join("measurements.txt"), report).expect("write the measurements");
}

/// A release example's numbers on one 1920x1080 output: its first frame,
/// its RSS 60 s after it, and its wakeups while idle until then.
fn measure(bar: &ExampleBar) -> String {
  let first_frame_ms = bar.first_frame_ms();
  let started = Instant::now();
  bar.wait_until_quiet();
  let quiet = bar.context_switches();
  let wakeups = bar.wakeups_for(Duration::from_secs(60).saturating_sub(started.elapsed()));
  let idle: u64 = wakeups.iter().map(|(_, switches)| switches).sum();
  let seen: Vec<f64> = wakeups.iter().map(|(past, _)| *past).collect();
  let rss = bar.rss_kib();
  format!(
    "release build, one 1920x1080 headless output at scale 1: \
         first frame {first_frame_ms:.1} ms after run(), RSS {rss} KiB ({:.1} MiB) after 60 s, \
         {quiet} context switches by the time it was quiet, {idle} more while idle, \
         seen at {seen:.2?} s past the minute",
    rss as f64 / 1024.0
  )
}

/// [`measure`] without the idle wakeups, for a shell run as this test
/// binary: libtest wakes it at 60 s to say the test is still running.
fn measure_without_idle(bar: &ExampleBar) -> String {
  let first_frame_ms = bar.first_frame_ms();
  std::thread::sleep(Duration::from_secs(60));
  let rss = bar.rss_kib();
  format!(
    "one 1920x1080 headless output at scale 1: \
         first frame {first_frame_ms:.1} ms after run(), RSS {rss} KiB ({:.1} MiB) after 60 s, \
         idle not counted, since the test harness wakes it",
    rss as f64 / 1024.0
  )
}

/// What the shell logs once a workspace switch is on screen, before its
/// time.
const SWITCHED: &str = "workspace switch on screen in ";

/// Switches the private Sway between workspaces 2 and 1 `n` times, a
/// quarter second apart so the bar is idle each time, and returns each
/// switch's time to the screen from `bar`'s log, in ms. Each switch must
/// be logged exactly once.
fn switch_workspaces(sway: &Sway, bar: &ExampleBar, n: usize) -> Vec<f64> {
  let mut times = Vec::new();
  for i in 0..=n {
    std::thread::sleep(Duration::from_millis(250));
    let extra = bar.drain();
    assert!(
      !extra.iter().any(|line| line.contains(SWITCHED)),
      "a switch was logged twice: {extra:?}"
    );
    if i < n {
      sway.msg(&["workspace", if i % 2 == 0 { "2" } else { "1" }]);
      times.push(ms_after(&bar.wait_for(SWITCHED), SWITCHED));
    }
  }
  times
}

/// The middle of `times`, or the upper of the two middle ones.
fn median(times: &[f64]) -> f64 {
  let mut sorted = times.to_vec();
  sorted.sort_by(f64::total_cmp);
  sorted[sorted.len() / 2]
}

/// What `run` logs once the first frame is committed, before its time.
const FIRST_FRAME: &str = "first frame committed ";

/// The number of ms in `line` right after `prefix`, as in `... 4.2 ms`.
fn ms_after(line: &str, prefix: &str) -> f64 {
  line
    .split(prefix)
    .nth(1)
    .and_then(|rest| rest.split(" ms").next())
    .and_then(|ms| ms.parse().ok())
    .unwrap_or_else(|| panic!("no time after {prefix:?} in {line:?}"))
}

/// The bar's background and the focused workspace's, from examples/bar.rs.
const BAR_BACKGROUND: [u8; 3] = [0x1e, 0x1e, 0x2e];
const FOCUSED: [u8; 3] = [0x45, 0x47, 0x5a];

/// The pixel at `x`, `y` of `shot`, without its alpha.
fn rgb(shot: &Image, x: u32, y: u32) -> [u8; 3] {
  let at = (y as usize * shot.width as usize + x as usize) * 4;
  [shot.rgba[at], shot.rgba[at + 1], shot.rgba[at + 2]]
}

/// The columns of the focused workspace's button in the bar along the top
/// of `shot`: the longest run of its background on the rows between the
/// button's top edge and its text. A text edge can blend to that color
/// too, but never 16 pixels in a row.
fn focused_button(shot: &Image) -> Option<Range<u32>> {
  let close = |pixel: [u8; 3]| pixel.iter().zip(FOCUSED).all(|(a, b)| a.abs_diff(b) <= 2);
  let mut longest = 0..0;
  for y in 4..9 {
    let mut start = 0;
    for x in 0..=shot.width {
      if x < shot.width && close(rgb(shot, x, y)) {
        continue;
      }
      if x - start > longest.len() as u32 {
        longest = start..x;
      }
      start = x + 1;
    }
  }
  (longest.len() >= 16).then_some(longest)
}

/// Whether the columns `button` of the bar in `shot` hold a label on the
/// bar's background: an unfocused workspace's button.
fn labelled(shot: &Image, button: &Range<u32>) -> bool {
  let area = button.clone().flat_map(|x| (4..26).map(move |y| (x, y)));
  let pixels: Vec<[u8; 3]> = area.map(|(x, y)| rgb(shot, x, y)).collect();
  pixels.contains(&BAR_BACKGROUND) && pixels.iter().any(|pixel| *pixel != BAR_BACKGROUND)
}

/// Screenshots `output` until `ready` holds for it, failing after ten
/// seconds.
fn wait_for_shot(sway: &Sway, output: &str, what: &str, ready: impl Fn(&Image) -> bool) {
  let deadline = Instant::now() + TIMEOUT;
  while !ready(&sway.screenshot(output)) {
    assert!(
      Instant::now() < deadline,
      "timed out waiting for {what} on {output}"
    );
    std::thread::sleep(Duration::from_millis(50));
  }
}

/// The private Sway's focused workspace, as `swaymsg -t get_workspaces`
/// says.
fn focused_in(sway: &Sway) -> Option<String> {
  let json = sway.msg(&["-t", "get_workspaces", "-r"]);
  let list: Vec<Workspace> = serde_json::from_str(&json).expect("swaymsg prints the list");
  list.into_iter().find(|ws| ws.focused).map(|ws| ws.name)
}

/// The private Sway with two 1920 by 1080 outputs side by side, focused
/// workspace 1 on HEADLESS-1 and workspace 2 on HEADLESS-2, and the bar
/// example showing them. Sway removes an empty workspace once it isn't
/// shown, so one output would never show two. Returns where workspace 1's
/// button is.
fn start_bar(release: bool) -> (Sway, ExampleBar, Range<u32>) {
  let sway = Sway::start();
  sway.msg(&[
    "output",
    "HEADLESS-1",
    "mode",
    "1920x1080",
    "position",
    "0",
    "0",
  ]);
  sway.msg(&["create_output"]);
  sway.msg(&[
    "output",
    "HEADLESS-2",
    "mode",
    "1920x1080",
    "position",
    "1920",
    "0",
  ]);
  let bar = ExampleBar::start(&sway, "bar", release, &[]);
  let deadline = Instant::now() + TIMEOUT;
  let one = loop {
    if let Some(one) = focused_button(&sway.screenshot("HEADLESS-1")) {
      break one;
    }
    assert!(
      Instant::now() < deadline,
      "workspace 1 never showed focused"
    );
    std::thread::sleep(Duration::from_millis(50));
  };
  wait_for_shot(&sway, "HEADLESS-2", "workspace 2, not focused", |shot| {
    focused_button(shot).is_none() && labelled(shot, &one)
  });
  assert_eq!(focused_in(&sway).as_deref(), Some("1"));
  (sway, bar, one)
}

#[test]
#[ignore = "needs headless Sway"]
fn the_bar_shows_each_outputs_workspaces_as_sway_switches() {
  let (sway, _bar, one) = start_bar(false);
  sway.msg(&["workspace", "2"]);
  // Each output's first button starts at the same place.
  wait_for_shot(&sway, "HEADLESS-2", "workspace 2, focused", |shot| {
    focused_button(shot).is_some_and(|two| two.start == one.start)
  });
  wait_for_shot(&sway, "HEADLESS-1", "workspace 1, not focused", |shot| {
    focused_button(shot).is_none() && labelled(shot, &one)
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn clicking_a_workspace_focuses_it_in_the_private_sway() {
  let (sway, _bar, one) = start_bar(false);
  sway.msg(&["workspace", "2"]);
  wait_for_shot(&sway, "HEADLESS-1", "workspace 1, not focused", |shot| {
    focused_button(shot).is_none()
  });
  let mut pointer = VirtualPointer::new(&sway);
  let x = (one.start + one.end) / 2;
  // The shell takes the new pointer while its loop runs, in its own
  // process; a click before that misses, so it clicks until Sway agrees.
  let deadline = Instant::now() + TIMEOUT;
  while focused_in(&sway).as_deref() != Some("1") {
    assert!(
      Instant::now() < deadline,
      "the click never focused workspace 1"
    );
    pointer.move_to(x, 15, (3840, 1080));
    pointer.click();
    std::thread::sleep(Duration::from_millis(200));
  }
  wait_for_shot(&sway, "HEADLESS-1", "workspace 1, focused again", |shot| {
    focused_button(shot) == Some(one.clone())
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn each_workspace_switch_logs_its_time_to_the_screen() {
  // Release, as a config's template builds its dependencies optimized.
  // Each switch repaints both outputs' bars.
  let (sway, bar, _) = start_bar(true);
  let mut times = switch_workspaces(&sway, &bar, 10);
  // A bar that never finishes its frame, on an output that is off, doesn't
  // keep the other output's commit from showing the switch.
  sway.msg(&["output", "HEADLESS-2", "power", "off"]);
  times.extend(switch_workspaces(&sway, &bar, 4));
  let median = median(&times);
  let slowest = times.iter().copied().fold(0.0, f64::max);
  eprintln!("workspace switch to screen: median {median:.1} ms, each {times:.1?} ms");
  let frame = 1000.0 / 60.0;
  assert!(
    slowest <= frame,
    "a switch took {slowest:.1} ms, more than a frame at 60 Hz: {times:?}"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn an_idle_bar_wakes_only_on_the_minute() {
  let sway = Sway::start();
  // Connected, so libpulse's thread and socket are watched too.
  let server = PrivateServer::start("idle");
  let bar = ExampleBar::start(&sway, "bar", false, &[("PULSE_SERVER", &server.address())]);
  bar.wait_for(FIRST_FRAME);
  bar.wait_until_connected(&server);
  bar.wait_until_quiet();
  // 65 s holds a minute boundary or two, and the minute clock's tick on
  // each, early by up to 50 ms or late by the sampling.
  let wakeups = bar.wakeups_for(Duration::from_secs(65));
  eprintln!("idle wakeups, as (seconds past the minute, context switches): {wakeups:.2?}");
  assert!(!wakeups.is_empty(), "the clock never ticked in 65 s");
  for (past, _) in &wakeups {
    assert!(
      *past < 1.0 || *past > 59.9,
      "a wakeup {past:.2} s past the minute, away from the clock's tick: {wakeups:.2?}"
    );
  }
}

/// Log lines captured from this thread, while a guard from
/// [`install`](Logs::install) lives.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl io::Write for Logs {
  fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
    self
      .0
      .lock()
      .expect("log buffer lock")
      .extend_from_slice(bytes);
    Ok(bytes.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl Logs {
  fn install(&self) -> tracing::subscriber::DefaultGuard {
    let writer = self.clone();
    tracing::subscriber::set_default(
      tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || writer.clone())
        .finish(),
    )
  }

  fn text(&self) -> String {
    String::from_utf8_lossy(&self.0.lock().expect("log buffer lock")).into_owned()
  }
}

/// How many windows `platform` holds for `output`.
fn on(platform: &Platform, output: &str) -> usize {
  platform
    .windows()
    .filter(|(_, window)| window.output().name() == output)
    .count()
}

#[test]
#[ignore = "needs headless Sway"]
fn a_panic_after_a_nested_per_output_doesnt_end_the_shell() {
  let sway = Sway::start();
  let logs = Logs::default();
  let _guard = logs.install();
  // f asks for more bars from its own context, then fails: that context
  // is gone by the time the nested request runs.
  let mut shell = Shell::new(sway.connect(), |cx| {
    cx.per_output(|cx: &Cx, output: &Output| -> PanelWindow {
      cx.per_output(counter::bar);
      panic!("the bar for {} fails", output.name());
    })
  })
  .expect("the root closure runs");
  let result = panic::catch_unwind(AssertUnwindSafe(|| {
    for _ in 0..50 {
      shell
        .dispatch(Some(Duration::from_millis(20)))
        .expect("the connection works");
    }
  }));
  assert!(
    result.is_ok(),
    "a panic escaped Shell::dispatch, which would end the shell"
  );
  let logs = logs.text();
  assert!(
    logs.contains("per_output[HEADLESS-1] (called at crates/crabshell/tests/headless.rs:"),
    "the panic is logged with the path of the per_output call: {logs}"
  );
  assert_eq!(shell.platform().windows().count(), 0);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_panicking_timer_is_logged_with_its_place_and_the_shell_runs_on() {
  let sway = Sway::start();
  let logs = Logs::default();
  let _guard = logs.install();
  let fine = Rc::new(std::cell::Cell::new(0_u32));
  let counted = Rc::clone(&fine);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let counted = Rc::clone(&counted);
    cx.per_output(move |cx: &Cx, output: &Output| {
      cx.interval(Duration::from_millis(50), || panic!("the timer fails"));
      let counted = Rc::clone(&counted);
      cx.interval(Duration::from_millis(50), move || {
        counted.set(counted.get() + 1)
      });
      counter::bar(cx, output)
    })
  })
  .expect("the root closure runs");
  let result = panic::catch_unwind(AssertUnwindSafe(|| {
    run_until(&mut shell, "the other timer's fifth fire", |_| {
      fine.get() >= 5
    });
  }));
  assert!(result.is_ok(), "a panic escaped Shell::dispatch");
  let logs = logs.text();
  assert!(
    logs.contains("timer cx.interval at crates/crabshell/tests/headless.rs:")
      && logs.contains("in per_output[HEADLESS-1] (called at ")
      && logs.contains("panicked and stops: the timer fails"),
    "the panic is logged with the call site and the context: {logs}"
  );
  assert_eq!(logs.matches("the timer fails").count(), 1, "it stopped");
  assert!(
    shown_on(shell.platform(), "HEADLESS-1").is_some(),
    "the bar is still up"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn a_nested_builders_bars_go_with_its_context() {
  let sway = Sway::start();
  let logs = Logs::default();
  let _guard = logs.install();
  let mut shell = Shell::new(sway.connect(), |cx| {
    cx.per_output(|cx: &Cx, output: &Output| {
      // An extra bar on every output, asked for while building
      // HEADLESS-1's: it belongs to HEADLESS-1's context.
      if output.name() == "HEADLESS-1" {
        cx.per_output(counter::bar);
      }
      counter::bar(cx, output)
    })
  })
  .expect("the root closure runs");
  run_until(&mut shell, "two bars on HEADLESS-1", |platform| {
    on(platform, "HEADLESS-1") == 2
  });
  sway.msg(&["create_output"]);
  run_until(&mut shell, "two bars on HEADLESS-2", |platform| {
    on(platform, "HEADLESS-2") == 2
  });
  settle(&mut shell);
  sway.msg(&["output", "HEADLESS-1", "unplug"]);
  run_until(
    &mut shell,
    "HEADLESS-1 and its extra bars to go",
    |platform| {
      platform.outputs().all(|output| output.name != "HEADLESS-1")
        && on(platform, "HEADLESS-2") == 1
    },
  );
  sway.msg(&["create_output"]);
  run_until(&mut shell, "a bar on HEADLESS-3", |platform| {
    on(platform, "HEADLESS-3") == 1
  });
  settle(&mut shell);
  assert_eq!(
    on(shell.platform(), "HEADLESS-3"),
    1,
    "only the root's builder is left"
  );
  let logs = logs.text();
  assert!(!logs.contains("panicked"), "{logs}");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_plain_unplug_logs_no_warning() {
  let sway = Sway::start();
  let logs = Logs::default();
  let _guard = logs.install();
  let mut shell = start(&sway);
  run_until(&mut shell, "a bar", |platform| {
    on(platform, "HEADLESS-1") == 1
  });
  sway.msg(&["create_output"]);
  run_until(&mut shell, "a second bar", |platform| {
    on(platform, "HEADLESS-2") == 1
  });
  settle(&mut shell);
  let mark = logs.text().len();
  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  run_until(&mut shell, "HEADLESS-2 to go", |platform| {
    platform.outputs().all(|output| output.name != "HEADLESS-2")
  });
  settle(&mut shell);
  let during = logs.text()[mark..].to_owned();
  assert!(
    !during.contains("WARN") && !during.contains("ERROR"),
    "a plain unplug logged a warning or an error:\n{during}"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn dropping_a_shell_frees_its_nodes() {
  let sway = Sway::start();
  let runtime = Runtime::current();
  runtime.flush();
  let before = runtime.node_count();
  {
    let mut shell = start(&sway);
    run_until(&mut shell, "a bar", |platform| {
      on(platform, "HEADLESS-1") == 1
    });
    settle(&mut shell);
    assert!(runtime.node_count() > before);
  }
  runtime.flush();
  assert_eq!(
    runtime.node_count(),
    before,
    "nodes left after the Shell was dropped"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn the_root_context_hands_out_running_services() {
  let sway = Sway::start();
  let mut clock = None;
  let mut shell = Shell::new(sway.connect(), |cx| {
    clock = Some(cx.clock(Precision::Second));
  })
  .expect("the root closure runs");
  let clock = clock.expect("the root closure ran");
  // Without the services, cx.clock() hands out a clock that stands still.
  let first = clock.get_untracked();
  run_until(&mut shell, "a second clock to tick", |_| {
    clock.get_untracked() != first
  });
}

/// What `wl-paste` prints, the shell running meanwhile, since it waits
/// for the shell to write.
fn wl_paste(sway: &Sway, shell: &mut Shell) -> String {
  let mut child = Some(
    sway
      .client("wl-paste")
      .arg("--no-newline")
      .stdout(Stdio::piped())
      .spawn()
      .expect("wl-paste runs"),
  );
  let mut output = None;
  run_until(shell, "wl-paste to finish", |_| {
    if let Some(Ok(Some(_))) = child.as_mut().map(Child::try_wait) {
      let done = child.take().expect("still held");
      output = Some(done.wait_with_output().expect("its output"));
    }
    output.is_some()
  });
  String::from_utf8_lossy(&output.expect("finished").stdout).into_owned()
}

#[test]
#[ignore = "needs headless Sway and wl-clipboard"]
fn the_clipboard_follows_other_programs_keeps_a_history_and_sets_what_they_paste() {
  let sway = Sway::start();
  let mut held = None;
  let mut shell = Shell::new(sway.connect(), |cx| {
    held = Some(cx.clipboard());
    cx.per_output(counter::bar);
  })
  .expect("the root closure runs");
  let clipboard = held.expect("the root closure ran");
  let (text, history) = (clipboard.text(), clipboard.history());
  run_until(&mut shell, "the bar", |platform| {
    shown_on(platform, "HEADLESS-1").is_some()
  });
  let texts = move || {
    let entries = history.get_untracked();
    let text = |entry: &ClipboardEntry| match &entry.content {
      ClipboardContent::Text(text) => text.clone(),
      ClipboardContent::Image(_) => String::from("<image>"),
    };
    entries.iter().map(text).collect::<Vec<_>>()
  };

  // The bar never has the keyboard, and still hears the copy.
  let mut copy = sway
    .client("wl-copy")
    .args(["--foreground", "from elsewhere"])
    .spawn()
    .expect("wl-copy runs");
  run_until(&mut shell, "the copy", |_| {
    text.get_untracked().as_deref() == Some("from elsewhere")
  });
  clipboard.set_text("from the config");
  run_until(&mut shell, "the config's own copy", |_| {
    texts() == ["from the config", "from elsewhere"]
  });
  assert_eq!(wl_paste(&sway, &mut shell), "from the config");
  let _ = copy.kill();
  let _ = copy.wait();

  let older = history.get_untracked()[1].id;
  clipboard.copy(older);
  run_until(&mut shell, "the entry back on top", |_| {
    texts() == ["from elsewhere", "from the config"]
  });
  assert_eq!(wl_paste(&sway, &mut shell), "from elsewhere");
  clipboard.remove(older);
  assert_eq!(texts(), ["from the config"]);
  settle(&mut shell);

  // Following the clipboard wakes nothing while it doesn't change.
  let before = shell.platform().stats();
  let waited = Instant::now();
  shell
    .dispatch(Some(Duration::from_secs(3)))
    .expect("the connection works");
  let waited = waited.elapsed();
  assert!(
    waited >= Duration::from_millis(2950),
    "woke after {waited:?}"
  );
  assert_eq!(
    shell.platform().stats().wayland_wakeups,
    before.wayland_wakeups
  );
  clipboard.clear_history();
  assert!(texts().is_empty());
}

/// Set when this test binary runs again as a shell; the value says which.
const CHILD: &str = "CRABSHELL_HEADLESS_CHILD";

/// This test binary, to run again as a shell: the test called `test`,
/// which runs one when [`CHILD`] is set.
fn this_as_shell(test: &str) -> Command {
  let mut command = Command::new(std::env::current_exe().expect("the test binary"));
  command.args(["--exact", test, "--ignored", "--nocapture"]);
  command
}

/// In a child run as a shell: runs the shell `CHILD` names, which never
/// returns.
fn run_as_child() {
  match std::env::var(CHILD).as_deref() {
    Ok("panics") => crabshell::run(|_| panic!("a broken config")),
    // Every output's bar panics, or only HEADLESS-1's.
    Ok("bars panic") => crabshell::run(|cx| {
      cx.per_output(|_: &Cx, output: &Output| -> PanelWindow {
        panic!("no bar for {}", output.name())
      });
    }),
    Ok("one bar panics") => crabshell::run(|cx| {
      cx.per_output(|cx: &Cx, output: &Output| {
        assert!(output.name() != "HEADLESS-1", "no bar for HEADLESS-1");
        counter::bar(cx, output)
      });
    }),
    Ok("a thread exits") => {
      let worker = std::thread::spawn(|| {
        for _ in 0..50 {
          std::thread::sleep(Duration::from_millis(10));
        }
      });
      let _ = worker.join();
      std::thread::sleep(Duration::from_secs(2));
      std::process::exit(0);
    }
    Ok("counter") => crabshell::run(|cx| cx.per_output(counter::bar)),
    Ok("v0.1") => run_v0_1_bar_logging_its_volume(),
    Ok("exits") => crabshell::run(|cx| {
      let program = cx.process(|| ["sh", "-c", "echo $$; exec sleep 30"]);
      let _program = program.on_line(|pid| eprintln!("the program is {pid}"));
      let shell = cx.clone();
      cx.ipc("shell", "exit", move |_| {
        // A launcher that starts a program and ends at once.
        let started = std::env::var("EXIT_STARTS").expect("where to say it started");
        shell.exec_detached(["sh", "-c", &format!("echo yes > {started}")]);
        shell.exit(3)
      });
      cx.per_output(counter::bar);
    }),
    Ok("programs") => crabshell::run(|cx| {
      let program = cx.process(|| ["sh", "-c", "echo $$; exec sleep 30"]);
      let _program = program.on_line(|pid| eprintln!("the program is {pid}"));
      cx.ipc("shell", "ping", |_| Ok(String::from("pong")));
      cx.per_output(counter::bar);
    }),
    // `other` refuses everyone without asking, so no account is reached.
    Ok("lock example") => crabshell::run(|cx| lock::setup(cx, "other")),
    // Locks and unlocks over IPC, and exits once the signal says unlocked.
    Ok("lock and exit") => crabshell::run(|cx| {
      let want = cx.signal(false);
      cx.ipc("lock", "on", move |_| {
        want.set(true);
        Ok(String::new())
      });
      cx.ipc("lock", "off", move |_| {
        want.set(false);
        Ok(String::new())
      });
      let shown = cx.session_lock(lock_screen(move || want.get()));
      let was = std::cell::Cell::new(false);
      let shell = cx.clone();
      cx.effect(move || {
        if shown.get() {
          was.set(true);
          eprintln!("the session is locked: true");
        } else if was.get() {
          shell.exit(0);
        }
      });
    }),
    Ok("lock") => crabshell::run(|cx| {
      let locked = cx.persist("locked", false);
      cx.ipc("lock", "on", move |_| {
        locked.set(true);
        Ok(String::new())
      });
      let shown = cx.session_lock(lock_screen(move || locked.get()));
      cx.effect(move || eprintln!("the session is locked: {}", shown.get()));
      cx.per_output(counter::bar);
    }),
    // The notes example, whose links panic instead of opening.
    Ok("notes") => crabshell::run(|cx| notes::shell(cx, |_, _| panic!("no browser"))),
    // The counter bar with a service connected, for its RSS.
    Ok("pipewire counter") => crabshell::run(|cx| {
      let nodes = cx.pipewire().nodes();
      cx.effect(move || {
        let count = nodes.with(Vec::len);
        if count > 0 {
          eprintln!("the graph has {count} nodes");
        }
      });
      cx.per_output(counter::bar);
    }),
    Ok("audio counter") => crabshell::run(|cx| {
      let sink = cx.audio().default_sink();
      cx.effect(move || {
        if let Some(sink) = sink.get() {
          eprintln!("the default sink is {}", sink.name);
        }
      });
      cx.per_output(counter::bar);
    }),
    // The counter bar in a font bundled from bytes, as `include_bytes!`
    // gives them: fontconfig never starts.
    Ok("bundled counter") => crabshell::run(|cx| {
      let bytes = std::fs::read(BUNDLED_FONT).expect("DejaVu Sans is installed");
      cx.add_font(Box::leak(bytes.into_boxed_slice()));
      let font = Style::new().font(Font::new("DejaVu Sans", 13.0));
      cx.per_output(move |cx, output| counter::bar(cx, output).style(font));
    }),
    // The dashboard example with each of its feeds pinned.
    Ok("dashboard") => crabshell::run(|cx| {
      let art = std::env::var("DASHBOARD_ART").unwrap_or_default();
      let feeds = dashboard::Feeds {
        battery: cx.signal(Some(pinned_battery())).read_only(),
        players: cx.signal(vec![pinned_player(&art)]).read_only(),
        cpu: pinned_cpu,
      };
      dashboard::shell(cx, feeds);
      cx.set_icon_theme(FIXTURE_ICONS);
    }),
    // The system example with its clock, stats and backlight pinned, and
    // its clipboard history logged.
    Ok("system") => crabshell::run(|cx| {
      let backlight = cx.signal(vec![pinned_backlight()]).read_only();
      run_system(cx, backlight);
    }),
    // The same with the machine's own backlights, if any.
    Ok("system idle") => crabshell::run(|cx| run_system(cx, cx.brightness().devices())),
    // `cx.icon_path` after `cx.set_icon_theme` in one closure, and in a
    // binding that reads the theme's name as the docs say.
    Ok("icon path") => crabshell::run(|cx| {
      cx.set_icon_theme(FIXTURE_ICONS);
      let found = cx.icon_path("computer-symbolic", 16).is_some();
      eprintln!("set then found: {found}");
      let theme = cx.signal(String::from(FIXTURE_ICONS));
      let probe = cx.clone();
      cx.effect(move || {
        let name = theme.get();
        let found = probe.icon_path("computer-symbolic", 16).is_some();
        eprintln!("bound to {name}: {found}");
      });
      let setter = cx.clone();
      cx.after(Duration::from_millis(100), move || {
        setter.set_icon_theme("hicolor");
        theme.set(String::from("hicolor"));
      });
      cx.per_output(counter::bar);
    }),
    // A red icon from the fixtures' theme, as a missing icon's fallback,
    // on a bar that names no theme.
    Ok("themed icon") => crabshell::run(|cx| {
      let found = cx.icon_path("computer-symbolic", 32).is_some();
      eprintln!("the theme has computer-symbolic: {found}");
      cx.per_output(|_, output| {
        PanelWindow::new(output)
          .anchors(Edge::Top | Edge::Left | Edge::Right)
          .height(32)
          .style(Style::new().background(Color::rgb(0, 0, 0)))
          .child(
            Icon::new("not-in-any-theme")
              .fallback("computer-symbolic")
              .size(32)
              .tint(Color::rgb(255, 0, 0)),
          )
      });
    }),
    Ok("ipc") => crabshell::run(|cx| {
      let count = cx.signal(0_i64);
      cx.effect(move || eprintln!("the count is {}", count.get()));
      cx.ipc("counter", "add", move |args| {
        let step: i64 = args
          .first()
          .and_then(|step| step.parse().ok())
          .ok_or_else(|| String::from("add takes a number"))?;
        count.update(|count| *count += step);
        Ok(count.get_untracked().to_string())
      });
      cx.per_output(counter::bar);
    }),
    _ => {}
  }
}

/// What [`run_v0_1_bar_logging_its_volume`] logs, before the volume button's
/// middle and label.
const VOLUME: &str = "the volume button at ";

/// The v0.1 bar on every output, on a loop of its own that logs the volume
/// button's middle and label whenever they change: what the bar shows,
/// read from its laid-out tree after each turn. Never returns.
fn run_v0_1_bar_logging_its_volume() -> ! {
  let event_loop = EventLoop::connect().expect("the private Sway");
  let mut shell =
    Shell::new(event_loop, |cx| cx.per_output(v0_1::bar)).expect("the root closure runs");
  let mut logged = String::new();
  loop {
    shell.dispatch(None).expect("the connection works");
    let platform = shell.platform();
    let Some(id) = shown_on(platform, "HEADLESS-1") else {
      continue;
    };
    // Between the tray and the clock.
    let ((x, y), label) = in_row(platform, id, 3, |button| {
      (middle(button.rect()), text_of(button))
    });
    let line = format!("{VOLUME}{x},{y} says {label}");
    if line != logged {
      eprintln!("{line}");
      logged = line;
    }
  }
}

/// Phase 3's exit check for audio, plus mute and the way back.
#[test]
#[ignore = "needs headless Sway and pipewire"]
fn the_volume_button_and_the_audio_server_agree() {
  run_as_child();
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080"]);
  let server = PrivateServer::start("volume");
  server.pactl(&["set-sink-volume", "@DEFAULT_SINK@", "50%"]);
  let volume = || server.pactl(&["get-sink-volume", "@DEFAULT_SINK@"]);
  let mute = || server.pactl(&["get-sink-mute", "@DEFAULT_SINK@"]);
  // Before the shell, so the seat has a pointer when the shell binds it,
  // and the first scroll can't miss.
  let mut pointer = VirtualPointer::new(&sway);
  let bar = ExampleBar::spawn(
    &sway,
    this_as_shell("the_volume_button_and_the_audio_server_agree"),
    &[(CHILD, "v0.1"), ("PULSE_SERVER", &server.address())],
  );
  let layout = (1920, 1080);
  let (x, y) = bar.volume_says("vol 50%");

  // One notch up.
  pointer.move_to(x, y, layout);
  pointer.scroll(-1);
  bar.volume_says("vol 55%");
  assert!(volume().contains(" 55% "), "{}", volume());

  // Two notches down.
  pointer.scroll(1);
  pointer.scroll(1);
  let (x, y) = bar.volume_says("vol 45%");
  assert!(volume().contains(" 45% "), "{}", volume());

  // A click mutes, and another unmutes.
  pointer.move_to(x, y, layout);
  pointer.click();
  let (x, y) = bar.volume_says("muted");
  assert_eq!(mute().trim(), "Mute: yes");
  pointer.move_to(x, y, layout);
  pointer.click();
  bar.volume_says("vol 45%");
  assert_eq!(mute().trim(), "Mute: no");

  // A change from elsewhere shows.
  server.pactl(&["set-sink-volume", "@DEFAULT_SINK@", "30%"]);
  bar.volume_says("vol 30%");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_root_that_panics_exits_1() {
  run_as_child();
  let sway = Sway::start();
  let mut shell = ExampleBar::spawn(
    &sway,
    this_as_shell("a_root_that_panics_exits_1"),
    &[(CHILD, "panics")],
  );
  shell.wait_for("the root closure panicked");
  let status = shell.exit_status().expect("the shell exits");
  assert_eq!(
    status.code(),
    Some(crabshell::EXIT_ROOT_PANICKED),
    "{status}"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn a_shell_whose_bars_all_panic_at_startup_exits_1() {
  run_as_child();
  let sway = Sway::start();
  sway.msg(&["create_output"]);
  let mut shell = ExampleBar::spawn(
    &sway,
    this_as_shell("a_shell_whose_bars_all_panic_at_startup_exits_1"),
    &[(CHILD, "bars panic")],
  );
  for output in ["HEADLESS-1", "HEADLESS-2"] {
    let line = shell.wait_for(&format!("per_output[{output}] (called at "));
    assert!(
      line.contains("crates/crabshell/tests/headless.rs:"),
      "the panic is logged with its path: {line}"
    );
  }
  shell.wait_for("has nothing to show");
  let status = shell.exit_status().expect("the shell exits");
  assert_eq!(
    status.code(),
    Some(crabshell::EXIT_ROOT_PANICKED),
    "{status}"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn a_bar_that_panics_on_one_of_two_outputs_leaves_the_shell_running() {
  run_as_child();
  let sway = Sway::start();
  sway.msg(&["create_output"]);
  let mut shell = ExampleBar::spawn(
    &sway,
    this_as_shell("a_bar_that_panics_on_one_of_two_outputs_leaves_the_shell_running"),
    &[(CHILD, "one bar panics")],
  );
  shell.wait_for("per_output[HEADLESS-1] (called at ");
  // The verdict comes before any frame, so the shell outlived it.
  shell.wait_for(FIRST_FRAME);
  // Later, only the panicked build is left, which is no verdict: the shell
  // still hears of the next output.
  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  shell.wait_for("output removed name=\"HEADLESS-2\"");
  sway.msg(&["create_output"]);
  shell.wait_for("output added name=\"HEADLESS-3\"");
  assert!(
    shell.child.try_wait().expect("its status").is_none(),
    "the shell ended"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn losing_the_compositor_exits_2() {
  run_as_child();
  let sway = Sway::start();
  let mut shell = ExampleBar::spawn(
    &sway,
    this_as_shell("losing_the_compositor_exits_2"),
    &[(CHILD, "counter")],
  );
  shell.wait_for("first frame committed");
  // Sway goes, as at logout.
  drop(sway);
  let status = shell.exit_status().expect("the shell exits");
  assert_eq!(
    status.code(),
    Some(crabshell::EXIT_NO_COMPOSITOR),
    "{status}"
  );
}

/// A private session bus, for the v0.1 bar's tray: a `dbus-daemon` on an
/// address of its own, which starts no services. It stops on drop.
struct PrivateBus {
  daemon: Child,
  address: String,
  config: PathBuf,
}

impl PrivateBus {
  fn start() -> PrivateBus {
    let config = private_bus_config();
    let mut daemon = Command::new("dbus-daemon")
      .arg(format!("--config-file={}", config.display()))
      .args(["--nofork", "--print-address"])
      .stdout(Stdio::piped())
      .spawn()
      .expect("dbus-daemon starts (it comes with dbus)");
    let stdout = daemon.stdout.take().expect("dbus-daemon's stdout");
    let mut address = String::new();
    BufReader::new(stdout)
      .read_line(&mut address)
      .expect("dbus-daemon prints its address");
    let address = address.trim().to_owned();
    PrivateBus {
      daemon,
      address,
      config,
    }
  }
}

impl Drop for PrivateBus {
  fn drop(&mut self) {
    let _ = self.daemon.kill();
    let _ = self.daemon.wait();
    let _ = std::fs::remove_file(&self.config);
    // Killed, dbus-daemon can't remove its socket in /tmp, so it is removed here.
    if let Some(socket) = self
      .address
      .strip_prefix("unix:path=")
      .and_then(|rest| rest.split(',').next())
    {
      let _ = std::fs::remove_file(socket);
    }
  }
}

/// A session bus's config with no service directories, written to a file
/// of its own. `dbus-daemon --session` would read the system's, and start
/// real services, such as the desktop's notification daemon, on demand.
fn private_bus_config() -> PathBuf {
  use std::sync::atomic::{AtomicU32, Ordering};
  static BUSES: AtomicU32 = AtomicU32::new(0);
  let path = std::env::temp_dir().join(format!(
    "crabshell-bus-{}-{}.conf",
    std::process::id(),
    BUSES.fetch_add(1, Ordering::Relaxed)
  ));
  std::fs::write(
    &path,
    "<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>\
     <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
     <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
  )
  .expect("the bus's config");
  path
}

/// Waits for a shell to connect to `listener`, say it can hand off and
/// that it is ready; gives the connection back.
fn accept_ready(listener: &std::os::unix::net::UnixListener) -> std::os::unix::net::UnixStream {
  use std::io::Read;

  listener
    .set_nonblocking(true)
    .expect("a non-blocking listener");
  let deadline = Instant::now() + Duration::from_secs(10);
  let mut stream = loop {
    match listener.accept() {
      Ok((stream, _)) => break stream,
      Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
        assert!(Instant::now() < deadline, "the shell never connected");
        std::thread::sleep(Duration::from_millis(20));
      }
      Err(error) => panic!("accept the shell: {error}"),
    }
  };
  stream.set_nonblocking(false).expect("a blocking stream");
  stream
    .set_read_timeout(Some(Duration::from_secs(10)))
    .expect("a read timeout");
  let mut said = [0_u8; 19];
  stream
    .read_exact(&mut said)
    .expect("the shell says something");
  assert_eq!(&said, b"can-hand-off\nready\n");
  stream
}

/// The shell's half of a reload: it says `ready` on the supervisor's
/// socket once its bar has drawn, saves its persisted values and exits
/// with 0 on `save`, and the next shell starts from them.
#[test]
#[ignore = "needs headless Sway"]
fn a_reload_saves_the_count_and_the_next_shell_starts_from_it() {
  use std::io::Write;

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let socket = sway.runtime_dir().join("control.sock");
  let listener = std::os::unix::net::UnixListener::bind(&socket).expect("the control socket");
  let control = socket.to_str().expect("a UTF-8 path");
  let state = sway.runtime_dir().join("crabshell").join("state.json");
  let saved_count = || -> u64 {
    let text = std::fs::read_to_string(&state).expect("the saved values");
    let values: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    values["clicks on HEADLESS-1"].as_u64().expect("a count")
  };
  let mut pointer = VirtualPointer::new(&sway);

  let mut first = ExampleBar::start(&sway, "counter", false, &[("CRABSHELL_CONTROL", control)]);
  let mut supervisor = accept_ready(&listener);
  // The button starts at the bar's left padding.
  pointer.move_to(30, 15, (1920, 1080));
  pointer.click();
  std::thread::sleep(Duration::from_millis(500));
  supervisor.write_all(b"save\n").expect("told to save");
  let status = first.exit_status().expect("the shell exits");
  assert_eq!(status.code(), Some(0));
  assert_eq!(saved_count(), 1);

  let mut second = ExampleBar::start(&sway, "counter", false, &[("CRABSHELL_CONTROL", control)]);
  let mut supervisor = accept_ready(&listener);
  assert!(
    !state.exists(),
    "the next shell read the file and removed it"
  );
  supervisor.write_all(b"save\n").expect("told to save");
  let status = second.exit_status().expect("the shell exits");
  assert_eq!(status.code(), Some(0));
  assert_eq!(saved_count(), 1, "it started from the saved count");
}

/// A reload keeps the notifications: the shell saves them with its
/// persisted values, the next one shows them again, and their ids go on.
#[test]
#[ignore = "needs headless Sway"]
fn a_reload_keeps_the_notifications_and_their_ids_go_on() {
  use std::io::Write;

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let socket = sway.runtime_dir().join("control.sock");
  let listener = std::os::unix::net::UnixListener::bind(&socket).expect("the control socket");
  let state = sway.runtime_dir().join("crabshell").join("state.json");
  let saved = || -> Vec<(u64, String)> {
    let text = std::fs::read_to_string(&state).expect("the saved values");
    let values: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    let history = values["crabshell::services"]["notifications"]["history"]
      .as_array()
      .expect("the notifications kept")
      .clone();
    let kept = history.iter().map(|kept| {
      let id = kept["id"].as_u64().expect("an id");
      (id, kept["summary"].as_str().expect("a summary").to_owned())
    });
    kept.collect()
  };
  let desktop = Desktop::new("reload-notifications");
  let mut env: Vec<(&str, &str)> = desktop.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
  env.push(("CRABSHELL_CONTROL", socket.to_str().expect("a UTF-8 path")));
  // The next shell takes the notifications name a moment after its bar
  // draws, so this tries again until it answers.
  let notify = |summary: &str| -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
      let output = Command::new("gdbus")
        .args(["call", "--address", &desktop.session.address])
        .args(["--dest", "org.freedesktop.Notifications"])
        .args(["--object-path", "/org/freedesktop/Notifications"])
        .args(["--method", "org.freedesktop.Notifications.Notify"])
        .args(["reload", "0", "", summary, "", "[]", "{}", "0"])
        .output()
        .expect("gdbus runs");
      if output.status.success() {
        return String::from_utf8_lossy(&output.stdout).trim().to_owned();
      }
      assert!(Instant::now() < deadline, "gdbus: {output:?}");
      std::thread::sleep(Duration::from_millis(100));
    }
  };

  let mut first = ExampleBar::start(&sway, "desktop", true, &env);
  let mut supervisor = accept_ready(&listener);
  assert_eq!(notify("kept"), "(uint32 1,)");
  supervisor.write_all(b"save\n").expect("told to save");
  assert_eq!(first.exit_status().and_then(|s| s.code()), Some(0));
  assert_eq!(saved(), [(1, String::from("kept"))]);

  let mut second = ExampleBar::start(&sway, "desktop", true, &env);
  let mut supervisor = accept_ready(&listener);
  assert_eq!(notify("next"), "(uint32 2,)", "the ids go on");
  supervisor.write_all(b"save\n").expect("told to save");
  assert_eq!(second.exit_status().and_then(|s| s.code()), Some(0));
  let both = [(1, String::from("kept")), (2, String::from("next"))];
  assert_eq!(saved(), both, "the next shell kept the first one's");
}

/// A reload as `crabshell run` does it: the old shell saves and runs on,
/// the new one starts beside it and takes the notifications over, and
/// only then says it is ready and the old one is told to exit. An app
/// sending notifications all through the swap gets every one answered,
/// and each is in the new shell's history.
#[test]
#[ignore = "needs headless Sway"]
fn notifications_sent_through_a_handoff_all_reach_the_new_shell() {
  use std::io::{Read, Write};
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let socket = sway.runtime_dir().join("control.sock");
  let listener = std::os::unix::net::UnixListener::bind(&socket).expect("the control socket");
  let state = sway.runtime_dir().join("crabshell").join("state.json");
  let desktop = Desktop::new("handoff-notifications");
  let mut env: Vec<(&str, &str)> = desktop.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
  env.push(("CRABSHELL_CONTROL", socket.to_str().expect("a UTF-8 path")));
  let saved_ids = || -> Vec<u64> {
    let text = std::fs::read_to_string(&state).expect("the saved values");
    let values: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    let history = values["crabshell::services"]["notifications"]["history"]
      .as_array()
      .expect("the notifications kept")
      .clone();
    history
      .iter()
      .map(|kept| kept["id"].as_u64().expect("an id"))
      .collect()
  };
  let said = |supervisor: &mut std::os::unix::net::UnixStream, line: &[u8]| {
    let mut heard = vec![0_u8; line.len()];
    supervisor
      .read_exact(&mut heard)
      .expect("the shell answers");
    assert_eq!(heard, line);
  };

  let mut first = ExampleBar::start(&sway, "desktop", true, &env);
  let mut old = accept_ready(&listener);
  let stop = Arc::new(AtomicBool::new(false));
  let app = std::thread::spawn({
    let (stop, address) = (Arc::clone(&stop), desktop.session.address.clone());
    move || {
      let mut ids = Vec::new();
      while !stop.load(Ordering::Relaxed) {
        let output = Command::new("gdbus")
          .args(["call", "--address", &address])
          .args(["--dest", "org.freedesktop.Notifications"])
          .args(["--object-path", "/org/freedesktop/Notifications"])
          .args(["--method", "org.freedesktop.Notifications.Notify"])
          .args(["handoff", "0", "", "a note", "", "[]", "{}", "0"])
          .output()
          .expect("gdbus runs");
        assert!(output.status.success(), "unanswered: {output:?}");
        let reply = String::from_utf8_lossy(&output.stdout);
        let id = reply
          .trim()
          .trim_start_matches("(uint32 ")
          .trim_end_matches(",)");
        ids.push(id.parse::<u64>().expect("an id"));
        std::thread::sleep(Duration::from_millis(5));
      }
      ids
    }
  });
  std::thread::sleep(Duration::from_millis(300));
  old.write_all(b"handoff\n").expect("told to hand off");
  said(&mut old, b"saved\n");
  // As the supervisor starts it: beside the old one until `alone`.
  let mut beside = env.clone();
  beside.push(("CRABSHELL_BESIDE", "1"));
  let mut second = ExampleBar::start(&sway, "desktop", true, &beside);
  let mut new = accept_ready(&listener);
  old.write_all(b"exit\n").expect("told to exit");
  assert_eq!(first.exit_status().and_then(|s| s.code()), Some(0));
  new.write_all(b"alone\n").expect("told it runs alone");
  std::thread::sleep(Duration::from_millis(300));
  stop.store(true, Ordering::Relaxed);
  let sent = app.join().expect("the app ran");
  new.write_all(b"handoff\n").expect("told to hand off");
  said(&mut new, b"saved\n");
  assert_eq!(saved_ids(), sent, "every one sent is in the new shell");
  new.write_all(b"exit\n").expect("told to exit");
  assert_eq!(second.exit_status().and_then(|s| s.code()), Some(0));
}

/// `cx.ipc` in a shell `run` started: it serves at the socket named for
/// `WAYLAND_DISPLAY`, the handler's signal reaches its effect on the
/// loop's flush, and the socket goes when the shell exits for a reload.
#[test]
#[ignore = "needs headless Sway"]
fn an_ipc_handler_answers_and_its_socket_goes_at_a_reload() {
  use std::io::{Read, Write};

  run_as_child();
  let sway = Sway::start();
  let control = sway.runtime_dir().join("control.sock");
  let listener = std::os::unix::net::UnixListener::bind(&control).expect("the control socket");
  let mut shell = ExampleBar::spawn(
    &sway,
    this_as_shell("an_ipc_handler_answers_and_its_socket_goes_at_a_reload"),
    &[
      (CHILD, "ipc"),
      ("CRABSHELL_CONTROL", control.to_str().expect("UTF-8")),
    ],
  );
  let mut supervisor = accept_ready(&listener);
  let socket = sway
    .runtime_dir()
    .join("crabshell")
    .join(format!("ipc-{}.sock", sway.display()));
  let mut client = std::os::unix::net::UnixStream::connect(&socket).expect("the IPC socket");
  client.set_read_timeout(Some(TIMEOUT)).expect("a timeout");
  client
    .write_all(b"{\"args\":[\"2\"],\"function\":\"add\",\"target\":\"counter\"}\n")
    .expect("asked");
  let mut reply = String::new();
  client.read_to_string(&mut reply).expect("the reply");
  assert_eq!(reply, "{\"ok\":\"2\"}\n");
  shell.wait_for("the count is 2");

  supervisor.write_all(b"save\n").expect("told to save");
  let status = shell.exit_status().expect("the shell exits");
  assert_eq!(status.code(), Some(0));
  assert!(!socket.exists(), "the shell removed its socket");
}

#[test]
#[ignore = "needs headless Sway"]
fn cx_floating_shows_a_window_while_its_context_lives() {
  let sway = Sway::start();
  let runtime = Runtime::current();
  let kept: std::rc::Rc<std::cell::RefCell<Option<(Cx, usize)>>> = Default::default();
  let hold = std::rc::Rc::clone(&kept);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let before = Runtime::current().node_count();
    let child = cx.child();
    child.floating(
      FloatingWindow::new()
        .title("crabshell-cx-floating")
        .child(Text::new("hello")),
    );
    *hold.borrow_mut() = Some((child, before));
  })
  .expect("the root closure runs");
  run_until(&mut shell, "the floating window", |platform| {
    platform.windows().any(|(id, _)| {
      platform
        .window_info(id)
        .is_some_and(|info| info.commits > 0)
    })
  });
  assert!(
    sway
      .msg(&["-t", "get_tree"])
      .contains("crabshell-cx-floating")
  );
  let (child, before) = kept.borrow_mut().take().expect("the root ran");
  child.owner().dispose();
  run_until(&mut shell, "the window to go", |platform| {
    platform.windows().count() == 0
  });
  runtime.flush();
  assert_eq!(
    runtime.node_count(),
    before,
    "everything the window made is freed"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn cx_idle_monitor_turns_true_when_idle_and_ends_with_its_context() {
  let sway = Sway::start();
  let runtime = Runtime::current();
  type Kept = Option<(Cx, ReadSignal<bool>, usize)>;
  let kept: std::rc::Rc<std::cell::RefCell<Kept>> = Default::default();
  let hold = std::rc::Rc::clone(&kept);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let before = Runtime::current().node_count();
    let child = cx.child();
    let idle = child.idle_monitor(Duration::from_millis(300));
    *hold.borrow_mut() = Some((child, idle, before));
  })
  .expect("the root closure runs");
  let idle = kept
    .borrow()
    .as_ref()
    .map(|(_, idle, _)| *idle)
    .expect("the root ran");
  assert!(!idle.get_untracked());
  run_until(&mut shell, "idle", |_| idle.get_untracked());
  assert_eq!(shell.platform().idle_watches(), 1);

  let (child, _, before) = kept.borrow_mut().take().expect("the root ran");
  child.owner().dispose();
  run_until(&mut shell, "the watch to end", |platform| {
    platform.idle_watches() == 0
  });
  runtime.flush();
  assert_eq!(runtime.node_count(), before, "everything it made is freed");
}

#[test]
#[ignore = "needs headless Sway"]
fn cx_global_shortcut_without_hyprland_stays_false_logs_once_and_frees_everything() {
  // Sway lacks hyprland-global-shortcuts-v1.
  let logs = Logs::default();
  let _guard = logs.install();
  let sway = Sway::start();
  let runtime = Runtime::current();
  type Kept = Option<(Cx, [ReadSignal<bool>; 2], usize)>;
  let kept: std::rc::Rc<std::cell::RefCell<Kept>> = Default::default();
  let hold = std::rc::Rc::clone(&kept);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let before = Runtime::current().node_count();
    let child = cx.child();
    let held = [
      child.global_shortcut("crabshell", "launcher", "Show the launcher"),
      child.global_shortcut("crabshell", "lock", "Lock the session"),
    ];
    *hold.borrow_mut() = Some((child, held, before));
  })
  .expect("the root closure runs");
  run_until(&mut shell, "the missing protocol logged", |_| {
    logs.text().contains("lacks hyprland-global-shortcuts-v1")
  });
  settle(&mut shell);
  let (child, held, before) = kept.borrow_mut().take().expect("the root ran");
  assert!(held.iter().all(|held| !held.get_untracked()));
  assert_eq!(shell.platform().shortcut_watches(), 0);
  let text = logs.text();
  assert_eq!(
    text.matches("lacks hyprland-global-shortcuts-v1").count(),
    1,
    "{text}"
  );

  child.owner().dispose();
  settle(&mut shell);
  runtime.flush();
  assert_eq!(runtime.node_count(), before, "everything it made is freed");
}

/// A test's file or directory, removed when the test ends, a panic
/// included, so test runs leave nothing in the temp dir.
struct Removed(std::path::PathBuf);

impl std::ops::Deref for Removed {
  type Target = std::path::PathBuf;

  fn deref(&self) -> &std::path::PathBuf {
    &self.0
  }
}

impl Drop for Removed {
  fn drop(&mut self) {
    if std::fs::remove_dir_all(&self.0).is_err() {
      let _ = std::fs::remove_file(&self.0);
    }
  }
}

/// A 4 by 4 PNG of one color, in the temp dir.
fn one_color_png(name: &str, [r, g, b]: [u8; 3]) -> Removed {
  let path = std::env::temp_dir().join(format!("crabshell-{}-{name}.png", std::process::id()));
  let file = std::fs::File::create(&path).expect("create the PNG");
  let mut encoder = png::Encoder::new(file, 4, 4);
  encoder.set_color(png::ColorType::Rgb);
  let mut writer = encoder.write_header().expect("the header");
  writer
    .write_image_data(&[r, g, b].repeat(16))
    .expect("the pixels");
  Removed(path)
}

#[test]
#[ignore = "needs headless Sway"]
fn cx_quantize_colors_follows_its_path_then_sleeps_and_ends_with_its_context() {
  let sway = Sway::start();
  let runtime = Runtime::current();
  let files = (
    one_color_png("quantize-red", [255, 0, 0]),
    one_color_png("quantize-blue", [0, 0, 255]),
  );
  let (red, blue) = (files.0.to_path_buf(), files.1.to_path_buf());
  type Kept = Option<(Cx, Signal<PathBuf>, ReadSignal<Vec<Color>>, usize)>;
  let kept: std::rc::Rc<std::cell::RefCell<Kept>> = Default::default();
  let hold = std::rc::Rc::clone(&kept);
  let first = red.clone();
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let before = Runtime::current().node_count();
    let child = cx.child();
    let path = child.signal(first);
    let colors = child.quantize_colors(move || path.get(), 2);
    *hold.borrow_mut() = Some((child, path, colors, before));
  })
  .expect("the root closure runs");
  let (path, colors) = kept
    .borrow()
    .as_ref()
    .map(|(_, path, colors, _)| (*path, *colors))
    .expect("the root ran");
  assert!(colors.get_untracked().is_empty());
  run_until(&mut shell, "red", |_| {
    colors.get_untracked() == [Color::rgb(255, 0, 0)]
  });
  path.set(blue);
  run_until(&mut shell, "blue", |_| {
    colors.get_untracked() == [Color::rgb(0, 0, 255)]
  });
  // The job and its source are gone, so the shell sleeps.
  let waited = Instant::now();
  shell
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  let waited = waited.elapsed();
  assert!(
    waited >= Duration::from_millis(1950),
    "something woke the loop after {waited:?}"
  );

  let (child, _, _, before) = kept.borrow_mut().take().expect("the root ran");
  child.owner().dispose();
  runtime.flush();
  assert_eq!(runtime.node_count(), before, "everything it made is freed");
}

#[test]
#[ignore = "needs headless Sway"]
fn cx_toplevels_lists_the_shells_floating_window_and_closes_it() {
  let sway = Sway::start();
  type Kept = Option<(Toplevels, Signal<bool>)>;
  let kept: std::rc::Rc<std::cell::Cell<Kept>> = Default::default();
  let hold = std::rc::Rc::clone(&kept);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let open = cx.signal(true);
    cx.floating(
      FloatingWindow::new()
        .title("crabshell-floating")
        .width(300)
        .height(200)
        .visible(move || open.get())
        .on_close(move |_| open.set(false))
        .child(Text::new("floating")),
    );
    hold.set(Some((cx.toplevels(), open)));
  })
  .expect("the root closure runs");
  let (toplevels, open) = kept.take().expect("the root ran");
  assert!(
    shell.cx().toplevels().list() == toplevels.list(),
    "every caller shares one list"
  );
  let ours = || {
    let list = toplevels.list().get_untracked();
    list
      .into_iter()
      .find(|toplevel| toplevel.title == "crabshell-floating")
  };
  run_until(&mut shell, "the floating window listed", |_| {
    ours().is_some_and(|toplevel| toplevel.outputs == ["HEADLESS-1"])
  });

  // A new scale makes the platform tell the same list again, which
  // isn't set, so nothing that reads it runs.
  let runs = std::rc::Rc::new(std::cell::Cell::new(0));
  let counted = std::rc::Rc::clone(&runs);
  let list = toplevels.list();
  let watcher = shell.cx().child();
  watcher.effect(move || {
    list.with(|_| ());
    counted.set(counted.get() + 1);
  });
  Runtime::current().flush();
  let before = runs.get();
  sway.msg(&["output", "HEADLESS-1", "scale", "2"]);
  for _ in 0..10 {
    shell
      .dispatch(Some(Duration::from_millis(30)))
      .expect("the connection works");
  }
  assert_eq!(runs.get(), before, "an unchanged list was set again");
  watcher.owner().dispose();

  toplevels.close(ours().expect("listed").id);
  run_until(&mut shell, "the window to close", |_| !open.get_untracked());
  run_until(&mut shell, "an empty list", |_| {
    toplevels.list().get_untracked().is_empty()
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn cx_window_manager_lists_sways_workspaces_and_switches_them() {
  let sway = Sway::start();
  let runtime = Runtime::current();
  runtime.flush();
  let before = runtime.node_count();
  let kept: std::rc::Rc<std::cell::Cell<Option<WindowManager>>> = Default::default();
  let hold = std::rc::Rc::clone(&kept);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    cx.per_output(counter::bar);
    hold.set(Some(cx.window_manager()));
  })
  .expect("the root closure runs");
  let manager = kept.take().expect("the root ran");
  assert!(
    shell.cx().window_manager().windowsets() == manager.windowsets(),
    "every caller shares one list"
  );
  let named = |name: &str| {
    let on = manager.windowsets_on("HEADLESS-1");
    on.into_iter().find(|set| set.name == name)
  };
  run_until(&mut shell, "workspace 1", |_| {
    named("1").is_some_and(|set| set.active)
  });

  // A window keeps workspace 1 once another shows.
  let _app = AppWindow::new(&sway, "org.example.App", "another program");
  sway.msg(&["workspace", "two"]);
  run_until(&mut shell, "workspace two", |_| {
    named("two").is_some_and(|set| set.active)
  });
  manager.activate(named("1").expect("listed").id);
  run_until(&mut shell, "workspace 1 again", |_| {
    named("1").is_some_and(|set| set.active) && named("two").is_none()
  });
  let projection = manager.projections().get_untracked()[0].id;
  manager.create_windowset(projection, "made");
  run_until(&mut shell, "a new workspace", |_| {
    named("made").is_some_and(|set| set.active)
  });

  // Each output lists its own; workspace 1 moves with `assign`.
  let names_on = |output: &str| -> Vec<String> {
    let on = manager.windowsets_on(output).into_iter();
    on.map(|set| set.name).collect()
  };
  sway.msg(&["create_output"]);
  run_until(&mut shell, "HEADLESS-2's workspace", |_| {
    names_on("HEADLESS-2") == ["2"] && named("2").is_none()
  });
  let second = manager.windowsets_on("HEADLESS-2")[0].projection;
  manager.assign(named("1").expect("listed").id, second.expect("in one"));
  run_until(&mut shell, "workspace 1 on HEADLESS-2", |_| {
    names_on("HEADLESS-2").contains(&String::from("1")) && named("1").is_none()
  });
  settle(&mut shell);

  // A new scale makes the platform tell the same lists again, which
  // aren't set, so nothing that reads them runs.
  let runs = std::rc::Rc::new(std::cell::Cell::new(0));
  let counted = std::rc::Rc::clone(&runs);
  let watcher = shell.cx().child();
  watcher.effect(move || {
    manager.windowsets_on("HEADLESS-1");
    counted.set(counted.get() + 1);
  });
  runtime.flush();
  let ran = runs.get();
  sway.msg(&["output", "HEADLESS-1", "scale", "2"]);
  settle(&mut shell);
  assert_eq!(runs.get(), ran, "unchanged lists were set again");
  watcher.owner().dispose();
  // Nothing changes, so nothing wakes the shell.
  let started = Instant::now();
  shell
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  let slept = started.elapsed();
  assert!(slept >= Duration::from_millis(1950), "woke after {slept:?}");

  drop(shell);
  runtime.flush();
  assert_eq!(runtime.node_count(), before, "everything it made is freed");
}

#[test]
#[ignore = "needs headless Sway"]
fn cx_outputs_lists_each_output_and_follows_its_changes() {
  use std::cell::{Cell, RefCell};
  use std::rc::Rc;

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let runtime = Runtime::current();
  runtime.flush();
  let before = runtime.node_count();
  let kept: Rc<Cell<Option<ReadSignal<Vec<OutputInfo>>>>> = Rc::default();
  // What each bar's builder found of its own output as it built.
  let found: Rc<RefCell<Vec<Option<OutputInfo>>>> = Rc::default();
  let (hold, saw) = (Rc::clone(&kept), Rc::clone(&found));
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let saw = Rc::clone(&saw);
    cx.per_output(move |cx, output| {
      let name = output.name();
      let all = cx.outputs().get_untracked();
      saw
        .borrow_mut()
        .push(all.into_iter().find(|info| info.name == name));
      counter::bar(cx, output)
    });
    hold.set(Some(cx.outputs()));
  })
  .expect("the root closure runs");
  let outputs = kept.take().expect("the root ran");
  assert!(
    shell.cx().outputs() == outputs,
    "every caller shares one list"
  );
  let named = |name: &str| {
    let all = outputs.get_untracked();
    all.into_iter().find(|info| info.name == name)
  };
  run_until(&mut shell, "HEADLESS-1's bar", |platform| {
    shown_on(platform, "HEADLESS-1").is_some()
  });
  let one = named("HEADLESS-1").expect("listed");
  assert_eq!((one.x, one.y, one.width, one.height), (0, 0, 1920, 1080));
  assert_eq!((one.scale, one.fractional_scale), (1, 1.0));
  // A config can name the types of its fields.
  let mode: Option<crabshell::Mode> = one.mode;
  assert_eq!(
    mode.map(|mode| (mode.width, mode.height)),
    Some((1920, 1080))
  );
  assert_eq!(one.transform, crabshell::Turn::default());
  assert_eq!(*found.borrow(), [Some(one)], "its bar's builder found it");

  // A binding that reads the list follows a new scale.
  let watcher = shell.cx().child();
  let scales = Rc::new(RefCell::new(Vec::new()));
  let seen = Rc::clone(&scales);
  watcher.effect(move || {
    let all = outputs.get();
    seen.borrow_mut().push(
      all
        .iter()
        .map(|info| info.fractional_scale)
        .collect::<Vec<_>>(),
    );
  });
  runtime.flush();
  sway.msg(&["output", "HEADLESS-1", "scale", "1.5"]);
  run_until(&mut shell, "HEADLESS-1 at 1.5", |_| {
    named("HEADLESS-1").is_some_and(|info| (info.fractional_scale, info.width) == (1.5, 1280))
  });
  runtime.flush();
  assert_eq!(scales.borrow().last(), Some(&vec![1.5]));

  // An output that comes is listed after the first, and one that goes
  // leaves the list.
  sway.msg(&["create_output"]);
  run_until(&mut shell, "HEADLESS-2's bar", |platform| {
    shown_on(platform, "HEADLESS-2").is_some()
  });
  let names = || -> Vec<String> {
    let all = outputs.get_untracked();
    all.into_iter().map(|info| info.name).collect()
  };
  assert_eq!(names(), ["HEADLESS-1", "HEADLESS-2"]);
  assert_eq!(found.borrow().len(), 2);
  assert!(found.borrow()[1].is_some(), "HEADLESS-2's builder found it");
  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  run_until(&mut shell, "HEADLESS-2 to go", |_| {
    names() == ["HEADLESS-1"]
  });
  watcher.owner().dispose();

  drop(shell);
  runtime.flush();
  assert_eq!(runtime.node_count(), before, "everything it made is freed");
}

/// 500 apps: the example's names, numbered.
fn many_apps() -> Vec<String> {
  (0..500)
    .map(|n| {
      let name = [
        "firefox", "files", "foot", "gimp", "kitty", "mpv", "obs", "vlc",
      ][n % 8];
      format!("{name} {}", n / 8 + 1)
    })
    .collect()
}

/// A shell showing the launcher example on every output, with its about
/// window, and the virtual keyboard it took.
fn start_launcher(sway: &Sway) -> (Shell, VirtualKeyboard) {
  // A layer surface gets the keyboard if one exists as it maps.
  let keyboard = VirtualKeyboard::new(sway);
  let shell = Shell::new(sway.connect(), |cx| {
    let about = cx.signal(false);
    cx.floating(launcher::about_window(about));
    cx.per_output(move |cx, output| launcher::launcher(cx, output, many_apps(), about));
  })
  .expect("the root closure runs");
  (shell, keyboard)
}

/// The launcher's window: the one panel.
fn launcher_window(platform: &Platform) -> Option<WindowId> {
  platform
    .windows()
    .find(|(_, window)| window.floating_config().is_none() && !window.is_popup())
    .map(|(id, _)| id)
}

/// The names of the launcher's rows that are built, in order.
fn launcher_rows(platform: &Platform) -> Vec<String> {
  let Some(id) = launcher_window(platform) else {
    return Vec::new();
  };
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the launcher is held");
  fn texts(node: NodeView<'_>, out: &mut Vec<String>) {
    if let Content::Text(text) = node.content() {
      out.push(text.content().to_owned());
    }
    for child in node.children() {
      texts(child, out);
    }
  }
  let view = window.view();
  // The box's column, then the list after the search row.
  let list = view
    .root()
    .children()
    .next()
    .and_then(|column| column.children().nth(1))
    .expect("the launcher has a list");
  let mut out = Vec::new();
  texts(list, &mut out);
  out
}

/// The part of `shot` the launcher covers, centered on a `width` by
/// `height` output.
fn launcher_part(shot: &Image, factor: f32) -> Image {
  let (width, height) = (
    (420.0 * factor).round() as u32,
    (360.0 * factor).round() as u32,
  );
  let (x, y) = ((shot.width - width) / 2, (shot.height - height) / 2);
  let rgba = (y..y + height)
    .flat_map(|row| {
      let start = ((row * shot.width + x) * 4) as usize;
      shot.rgba[start..start + width as usize * 4].to_vec()
    })
    .collect();
  Image {
    rgba,
    width,
    height,
  }
}

/// Phase 7's exit check: the launcher matches its goldens at each scale,
/// typing filters 500 rows within a frame a key, the wheel scrolls, Down
/// picks, the about window opens and closes from the compositor, and the
/// open launcher wakes nothing while idle.
#[test]
#[ignore = "needs headless Sway"]
fn the_launcher_types_scrolls_opens_windows_and_matches_its_goldens() {
  for (scale, factor, suffix) in [("1", 1.0, "1x"), ("1.5", 1.5, "1.5x"), ("2", 2.0, "2x")] {
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let (mut shell, _keyboard) = start_launcher(&sway);
    run_until(&mut shell, "the launcher", |platform| {
      launcher_window(platform)
        .and_then(|id| platform.window_info(id))
        .is_some_and(|info| info.commits > 0)
    });
    settle(&mut shell);
    check_golden(
      &format!("launcher@{suffix}"),
      &launcher_part(&sway.screenshot("HEADLESS-1"), factor),
      (360.0 * factor).round() as u32,
    );
  }

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let runtime = Runtime::current();
  let (mut shell, mut keyboard) = start_launcher(&sway);
  let mut pointer = VirtualPointer::new(&sway);
  run_until(&mut shell, "the launcher", |platform| {
    launcher_window(platform)
      .and_then(|id| platform.window_info(id))
      .is_some_and(|info| info.commits > 0)
  });
  settle(&mut shell);
  assert_eq!(
    launcher_rows(shell.platform()).first().map(String::as_str),
    Some("firefox 1")
  );

  // Typing filters within one frame a key.
  let commits = shell.platform().stats().commits;
  for key in [keys::F, keys::I, keys::R] {
    keyboard.tap(key);
  }
  run_until(&mut shell, "the filtered list", |platform| {
    let rows = launcher_rows(platform);
    !rows.is_empty() && rows.iter().all(|row| row.starts_with("firefox"))
  });
  settle(&mut shell);
  assert!(
    shell.platform().stats().commits <= commits + 3,
    "{} frames for three keys",
    shell.platform().stats().commits - commits
  );

  // The wheel scrolls the list: its first rows go.
  pointer.move_to(960, 600, (1920, 1080));
  pointer.scroll(5);
  run_until(&mut shell, "the list to scroll", |platform| {
    launcher_rows(platform).first().map(String::as_str) != Some("firefox 1")
  });

  // Down picks the next row and keeps it in view.
  keyboard.tap(keys::DOWN);
  settle(&mut shell);

  // The about button opens the floating window; the compositor closes it.
  runtime.flush();
  let nodes = runtime.node_count();
  let (x, y) = (960 + 210 - 12 - 30, 540 - 180 + 12 + 14);
  let (x, y) = (x.to_string(), y.to_string());
  sway.msg(&["seat", "-", "cursor", "set", &x, &y]);
  sway.msg(&["seat", "-", "cursor", "press", "button1"]);
  sway.msg(&["seat", "-", "cursor", "release", "button1"]);
  run_until(&mut shell, "the about window", |_| {
    sway.msg(&["-t", "get_tree"]).contains("about crabshell")
  });
  sway.msg(&["[title=\"about crabshell\"]", "kill"]);
  run_until(&mut shell, "the about window to close", |_| {
    !sway.msg(&["-t", "get_tree"]).contains("about crabshell")
  });
  settle(&mut shell);
  runtime.flush();
  assert_eq!(
    runtime.node_count(),
    nodes,
    "closing the window freed nothing it shouldn't"
  );

  // Idle with the field focused: one dispatch sleeps the whole time.
  let before = shell.platform().stats().iterations;
  let started = Instant::now();
  shell
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  assert!(
    started.elapsed() >= Duration::from_millis(1900),
    "{:?}",
    started.elapsed()
  );
  assert_eq!(shell.platform().stats().iterations, before + 1);
}

/// How far the effects bar's indicator has slid, from the width of the
/// spacer before it.
fn indicator_offset(platform: &Platform, id: WindowId) -> f32 {
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the bar is held");
  let view = window.view();
  // The row, the tabs' column, the indicator's row, then its spacer.
  view
    .root()
    .children()
    .next()
    .and_then(|row| row.children().next())
    .and_then(|column| column.children().nth(1))
    .and_then(|indicator| indicator.children().next())
    .map_or(f32::NAN, |spacer| spacer.rect().width)
}

/// Phase 8's exit check: the effects bar matches its goldens at each
/// scale; hovering a tab and picking another draws frames only while the
/// fade and the slide run, then the bar sleeps; and its blur, which Sway
/// can't give, is logged once.
#[test]
#[ignore = "needs headless Sway"]
fn the_effects_bar_draws_only_while_it_moves_and_matches_its_goldens() {
  for (scale, factor, suffix) in [("1", 1.0, "1x"), ("1.5", 1.5, "1.5x"), ("2", 2.0, "2x")] {
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let mut shell = Shell::new(sway.connect(), |cx| cx.per_output(effects::effects))
      .expect("the root closure runs");
    run_until(&mut shell, "the bar's first frame", |platform| {
      shown_on(platform, "HEADLESS-1").is_some()
    });
    settle(&mut shell);
    check_golden(
      &format!("effects@{suffix}"),
      &sway.screenshot("HEADLESS-1"),
      (48.0 * factor) as u32,
    );
  }

  let logs = Logs::default();
  let _logging = logs.install();
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut shell = Shell::new(sway.connect(), |cx| cx.per_output(effects::effects))
    .expect("the root closure runs");
  run_until(&mut shell, "the bar's first frame", |platform| {
    shown_on(platform, "HEADLESS-1").is_some()
  });
  let id = shown_on(shell.platform(), "HEADLESS-1").expect("the bar is shown");
  let mut pointer = VirtualPointer::new(&sway);
  settle(&mut shell);
  assert_eq!(indicator_offset(shell.platform(), id), 0.0);

  // The third tab: 12 in, past two 40-pixel tabs and their 4-pixel gaps.
  let before = shell.platform().stats();
  pointer.move_to(12 + 2 * 44 + 20, 20, (1920, 1080));
  pointer.click();
  run_until(
    &mut shell,
    "the indicator under the third tab",
    |platform| indicator_offset(platform, id) == 88.0,
  );
  settle(&mut shell);
  let after = shell.platform().stats();
  let frames = after.commits - before.commits;
  // 250 ms at 60 Hz is about 15 frames; frame callbacks may merge some.
  assert!((6..=60).contains(&frames), "{frames} frames for the slide");
  let iterations = after.iterations - before.iterations;
  assert!(
    iterations < 400,
    "{iterations} loop iterations for one slide"
  );
  eprintln!(
    "the slide and fade took {frames} frames, drawn in {:.2} ms each",
    (after.drawing - before.drawing).as_secs_f64() * 1000.0 / frames as f64
  );

  // Ended: one dispatch sleeps the whole time.
  let before = shell.platform().stats().iterations;
  let started = Instant::now();
  shell
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  assert!(started.elapsed() >= Duration::from_millis(1900));
  assert_eq!(shell.platform().stats().iterations, before + 1);
  assert_eq!(
    logs.text().matches("can't blur behind windows").count(),
    1,
    "{}",
    logs.text()
  );
}

/// The private buses and audio server the desktop example runs on, and
/// the environment that points it at them, never at the desktop's.
struct Desktop {
  server: PrivateServer,
  _system: PrivateBus,
  session: PrivateBus,
  env: Vec<(&'static str, String)>,
}

impl Desktop {
  fn new(name: &str) -> Desktop {
    let server = PrivateServer::start(name);
    let (system, session) = (PrivateBus::start(), PrivateBus::start());
    let env = vec![
      ("PULSE_SERVER", server.address()),
      ("DBUS_SYSTEM_BUS_ADDRESS", system.address.clone()),
      ("DBUS_SESSION_BUS_ADDRESS", session.address.clone()),
    ];
    Desktop {
      server,
      _system: system,
      session,
      env,
    }
  }

  fn start(&self, sway: &Sway) -> ExampleBar {
    self.run(sway, "desktop")
  }

  /// Starts the example `name` on these buses and this audio server.
  fn run(&self, sway: &Sway, name: &str) -> ExampleBar {
    let env: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    ExampleBar::start(sway, name, true, &env)
  }

  /// Sends a notification, as an app would, and returns gdbus's answer.
  fn notify(&self, summary: &str, timeout_ms: i32) -> String {
    self.notify_with(summary, "from the exit check", timeout_ms)
  }

  /// [`notify`](Self::notify) with `body`.
  fn notify_with(&self, summary: &str, body: &str, timeout_ms: i32) -> String {
    notify_on(&self.session.address, summary, body, timeout_ms)
  }
}

/// The user's XDG config directory, where fontconfig reads its settings.
fn user_config() -> Option<PathBuf> {
  let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
  let home = || std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"));
  xdg.filter(|dir| dir.is_absolute()).or_else(home)
}

/// Sends a notification on the bus at `address`, as an app would, and
/// returns gdbus's answer.
fn notify_on(address: &str, summary: &str, body: &str, timeout_ms: i32) -> String {
  let output = Command::new("gdbus")
    .args(["call", "--address", address])
    .args(["--dest", "org.freedesktop.Notifications"])
    .args(["--object-path", "/org/freedesktop/Notifications"])
    .args(["--method", "org.freedesktop.Notifications.Notify"])
    .args(["exit-check", "0", "", summary, body, "[]", "{}"])
    .arg(timeout_ms.to_string())
    .env_clear()
    .env("PATH", std::env::var_os("PATH").unwrap_or_default())
    .output()
    .expect("gdbus runs");
  assert!(output.status.success(), "gdbus: {output:?}");
  String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Waits until two screenshots in a row, 300 ms apart, are the same.
fn steady_shot(sway: &Sway, output: &str) -> Image {
  let deadline = Instant::now() + TIMEOUT;
  let mut last = sway.screenshot(output);
  loop {
    std::thread::sleep(Duration::from_millis(300));
    let shot = sway.screenshot(output);
    if shot.rgba == last.rgba {
      return shot;
    }
    assert!(Instant::now() < deadline, "{output} never stopped changing");
    last = shot;
  }
}

/// The desktop example's toast color, from examples/desktop.rs.
const TOAST_BACKGROUND: [u8; 3] = [0x31, 0x32, 0x44];

/// Phase 9's exit check: the desktop example, with every Phase 9 service
/// started on private buses with no daemons and a private audio server,
/// matches its goldens at each scale; a notification an app sends shows
/// as a toast and goes when it expires; and then the shell sleeps.
#[test]
#[ignore = "needs headless Sway"]
fn the_desktop_bar_shows_a_toast_and_sleeps_with_every_service_started() {
  let desktop = Desktop::new("desktop");
  for (scale, factor, suffix) in [("1", 1.0, "1x"), ("1.5", 1.5, "1.5x"), ("2", 2.0, "2x")] {
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let bar = desktop.start(&sway);
    bar.wait_for(FIRST_FRAME);
    let shot = steady_shot(&sway, "HEADLESS-1");
    check_golden(&format!("desktop@{suffix}"), &shot, (30.0 * factor) as u32);
  }

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let bar = desktop.start(&sway);
  bar.wait_for(FIRST_FRAME);
  steady_shot(&sway, "HEADLESS-1");
  // Inside the first toast: 8 in from the right edge, 36 down.
  let inside = (1920 - 8 - 340 + 6, 36 + 6);
  assert_ne!(
    rgb(&sway.screenshot("HEADLESS-1"), inside.0, inside.1),
    TOAST_BACKGROUND
  );
  let answer = desktop.notify("Hello", 1500);
  assert!(answer.contains("uint32 1"), "{answer}");
  wait_for_shot(&sway, "HEADLESS-1", "the toast", |shot| {
    rgb(shot, inside.0, inside.1) == TOAST_BACKGROUND
  });
  wait_for_shot(&sway, "HEADLESS-1", "the toast to expire", |shot| {
    rgb(shot, inside.0, inside.1) != TOAST_BACKGROUND
  });

  // Asleep: no wakeups for 10 s with every service started.
  steady_shot(&sway, "HEADLESS-1");
  let wakeups = bar.wakeups_for(Duration::from_secs(10));
  assert!(wakeups.is_empty(), "woke while idle: {wakeups:?}");
}

/// What the overview example reads, in a temporary directory: three
/// applications that write their name to `launched` when started, one
/// hidden, icons for each, a wallpaper of two colors, and private XDG
/// directories so the user's own never show.
struct Overview {
  dir: PathBuf,
}

impl Overview {
  fn new() -> Overview {
    let dir = std::env::temp_dir().join(format!("crabshell-overview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let apps = dir.join("data/applications");
    std::fs::create_dir_all(&apps).expect("the applications directory");
    std::fs::create_dir_all(dir.join("none")).expect("an empty data directory");
    let launch = dir.join("launch.sh");
    std::fs::write(
      &launch,
      format!(
        "#!/bin/sh\necho \"$XDG_ACTIVATION_TOKEN $DESKTOP_STARTUP_ID\" > {}\necho \"$1\" > {}\n",
        dir.join("token").display(),
        dir.join("launched").display()
      ),
    )
    .expect("the launch script");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&launch, std::fs::Permissions::from_mode(0o755))
      .expect("an executable script");
    for (id, name, comment, color) in [
      ("files", "Files", "Browse your files", [0x89, 0xb4, 0xfa]),
      ("notes", "Notes", "Write things down", [0xa6, 0xe3, 0xa1]),
      ("terminal", "Terminal", "", [0xf3, 0x8b, 0xa8]),
    ] {
      let icon = small_png(&dir.join(format!("{id}.png")), &[color; 4]);
      std::fs::write(
        apps.join(format!("{id}.desktop")),
        format!(
          "[Desktop Entry]\nType=Application\nName={name}\nComment={comment}\nIcon={}\nExec={} {id}\n",
          icon.display(),
          launch.display()
        ),
      )
      .expect("a desktop entry");
    }
    std::fs::write(
      apps.join("hidden.desktop"),
      "[Desktop Entry]\nType=Application\nName=Hidden\nNoDisplay=true\nExec=true\n",
    )
    .expect("a hidden entry");
    small_png(
      &dir.join("wallpaper.png"),
      &[[0x1e, 0x66, 0xf5], [0xfe, 0x64, 0x0b]],
    );
    Overview { dir }
  }

  fn env(&self) -> Vec<(&'static str, String)> {
    let at = |name: &str| self.dir.join(name).display().to_string();
    vec![
      ("XDG_DATA_HOME", at("data")),
      ("XDG_DATA_DIRS", at("none")),
      ("XDG_STATE_HOME", at("state")),
      ("WALLPAPER", at("wallpaper.png")),
    ]
  }

  fn start(&self, sway: &Sway) -> ExampleBar {
    let env = self.env();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    ExampleBar::start(sway, "overview", true, &env)
  }

  fn launched(&self) -> Option<String> {
    std::fs::read_to_string(self.dir.join("launched")).ok()
  }

  /// The activation token the last launched application was given, in
  /// both variables.
  fn token(&self) -> Option<String> {
    std::fs::read_to_string(self.dir.join("token")).ok()
  }
}

impl Drop for Overview {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

/// A PNG 4 pixels wide with a row for each of `colors`, at `path`.
fn small_png(path: &Path, colors: &[[u8; 3]]) -> PathBuf {
  let file = std::fs::File::create(path).expect("create the PNG");
  let height = u32::try_from(colors.len()).expect("a few rows");
  let mut encoder = png::Encoder::new(file, 4, height);
  encoder.set_color(png::ColorType::Rgb);
  let mut writer = encoder.write_header().expect("the header");
  let pixels: Vec<u8> = colors.iter().flat_map(|color| color.repeat(4)).collect();
  writer.write_image_data(&pixels).expect("the pixels");
  path.to_owned()
}

/// Calls the shell's IPC handler `target`.`function` on `sway`, as
/// `crabshell ipc` does, and returns the reply line.
fn ipc_call(sway: &Sway, target: &str, function: &str) -> String {
  ipc_call_with(sway, target, function, &[])
}

/// [`ipc_call`] with `args`.
fn ipc_call_with(sway: &Sway, target: &str, function: &str, args: &[&str]) -> String {
  use std::io::Write;
  let socket = sway
    .runtime_dir()
    .join("crabshell")
    .join(format!("ipc-{}.sock", sway.display()));
  let deadline = Instant::now() + TIMEOUT;
  let mut stream = loop {
    match std::os::unix::net::UnixStream::connect(&socket) {
      Ok(stream) => break stream,
      Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
      Err(error) => panic!("no IPC socket at {}: {error}", socket.display()),
    }
  };
  stream
    .set_read_timeout(Some(TIMEOUT))
    .expect("a read timeout");
  let args = serde_json::to_string(args).expect("the arguments as JSON");
  writeln!(
    stream,
    r#"{{"target":"{target}","function":"{function}","args":{args}}}"#
  )
  .expect("the request");
  let mut reply = String::new();
  BufReader::new(stream)
    .read_line(&mut reply)
    .expect("the reply");
  reply
}

/// Phase 10's exit check: the overview example opens and closes from the
/// command line, lists the applications from desktop entries, recent
/// ones first, and another program's window, shows the wallpaper's colors
/// and `uname -s`, launches an application on a click, and sleeps while
/// it is open.
#[test]
#[ignore = "needs headless Sway"]
fn the_overview_opens_from_the_command_line_launches_an_app_and_sleeps() {
  let overview = Overview::new();
  // The overview's top 40 + 420 logical pixels.
  for (scale, factor, suffix) in [("1", 1.0, "1x"), ("1.5", 1.5, "1.5x"), ("2", 2.0, "2x")] {
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    sway.msg(&["default_border", "none"]);
    let _window = AppWindow::new(&sway, "org.example.notes", "notes");
    let shell = overview.start(&sway);
    assert_eq!(ipc_call(&sway, "overview", "open"), "{\"ok\":\"\"}\n");
    shell.wait_for(FIRST_FRAME);
    let shot = steady_shot(&sway, "HEADLESS-1");
    check_golden(
      &format!("overview@{suffix}"),
      &shot,
      (460.0 * factor) as u32,
    );
  }

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  sway.msg(&["default_border", "none"]);
  let _window = AppWindow::new(&sway, "org.example.notes", "notes");
  // A headless seat has no pointer until a program gives it one.
  let mut pointer = VirtualPointer::new(&sway);
  let shell = overview.start(&sway);
  let closed = steady_shot(&sway, "HEADLESS-1");
  ipc_call(&sway, "overview", "toggle");
  shell.wait_for(FIRST_FRAME);
  let open = steady_shot(&sway, "HEADLESS-1");
  assert_ne!(open.rgba, closed.rgba, "toggle opened it");

  // Asleep while open, with every Phase 10 service it uses started.
  let wakeups = shell.wakeups_for(Duration::from_secs(10));
  assert!(wakeups.is_empty(), "woke while idle: {wakeups:?}");

  // The third application, Terminal: 18 + 12 + 18 + 10 + 16 + 2 down the
  // card, then two rows of 28 and their gaps.
  pointer.move_to(1920 / 2 - 300, 40 + 76 + 2 * 30 + 14, (1920, 1080));
  pointer.click();
  let deadline = Instant::now() + TIMEOUT;
  while overview.launched().is_none() {
    assert!(Instant::now() < deadline, "nothing was launched");
    std::thread::sleep(Duration::from_millis(50));
  }
  assert_eq!(overview.launched().as_deref(), Some("terminal\n"));
  // Sway handed out an activation token for the click, in both variables.
  let token = overview.token().expect("the token's file");
  let (xdg, startup) = token.trim_end().split_once(' ').expect("two words");
  assert!(!xdg.is_empty() && xdg == startup, "{token:?}");
  wait_for_shot(&sway, "HEADLESS-1", "the overview to close", |shot| {
    shot.rgba == closed.rgba
  });
  let recent = std::fs::read_to_string(overview.dir.join("state/crabshell/overview.json"))
    .expect("the recent applications");
  assert!(recent.contains("terminal"), "{recent}");

  // Opened again, it lists Terminal first.
  ipc_call(&sway, "overview", "toggle");
  let reopened = steady_shot(&sway, "HEADLESS-1");
  assert_ne!(reopened.rgba, open.rgba, "the order changed");
  drop(shell);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_screencopy_view_shows_the_floating_window_the_shell_opened() {
  const GREEN: Color = Color::rgb(0x20, 0xc0, 0x40);
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  let mut shell = Shell::new(sway.connect(), |cx| {
    cx.floating(
      FloatingWindow::new()
        .title("captured")
        .width(300)
        .height(200)
        .style(Style::new().background(GREEN)),
    );
    let toplevels = cx.toplevels();
    cx.per_output(move |_, output| {
      let source = move || {
        let list = toplevels.list().get();
        let window = list.iter().find(|toplevel| toplevel.title == "captured");
        // No window has `u64::MAX`, so the view waits.
        CaptureSource::Toplevel(window.map_or(u64::MAX, |toplevel| toplevel.id))
      };
      PanelWindow::new(output)
        .anchors(Edge::Top | Edge::Left)
        .width(160)
        .height(90)
        .child(
          ScreencopyView::new(source)
            .live(true)
            .style(Style::new().width(160.0).height(90.0)),
        )
    });
  })
  .expect("the root closure runs");
  let green = |platform: &Platform| {
    let Some(id) = shown_on(platform, "HEADLESS-1") else {
      return false;
    };
    let (_, window) = platform
      .windows()
      .find(|(held, _)| *held == id)
      .expect("the bar is held");
    let view = window.view();
    let Some(node) = view.root().children().next() else {
      return false;
    };
    let Content::Capture(capture) = node.content() else {
      return false;
    };
    let expected = [GREEN.r, GREEN.g, GREEN.b, 255];
    let pixels = capture.frame.rgba.as_chunks::<4>().0;
    pixels
      .iter()
      .all(|pixel| pixel.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 2))
  };
  run_until(&mut shell, "the window's frame in the view", |platform| {
    green(platform)
  });
  assert_eq!(shell.platform().capture_sessions(), 1);
}

/// A shell told to end by SIGTERM or SIGINT, as `crabshell run` on Ctrl-C
/// or a session ending tells it, stops the programs it started and
/// removes its IPC socket, then ends by that signal, as it did before.
#[test]
#[ignore = "needs headless Sway"]
fn a_signal_ends_the_shell_with_its_programs_and_socket() {
  use std::os::unix::process::ExitStatusExt;

  run_as_child();
  for signal in ["TERM", "INT"] {
    let sway = Sway::start();
    let mut shell = ExampleBar::spawn(
      &sway,
      this_as_shell("a_signal_ends_the_shell_with_its_programs_and_socket"),
      &[(CHILD, "programs")],
    );
    let line = shell.wait_for("the program is ");
    let pid: u32 = line
      .rsplit(' ')
      .next()
      .and_then(|pid| pid.trim().parse().ok())
      .expect("a pid");
    let socket = sway
      .runtime_dir()
      .join("crabshell")
      .join(format!("ipc-{}.sock", sway.display()));
    assert!(socket.exists(), "the IPC socket is there");
    let sent = Command::new("kill")
      .args([format!("-{signal}"), shell.child.id().to_string()])
      .status()
      .expect("kill runs");
    assert!(sent.success());
    let status = shell.exit_status().expect("the shell exits");
    let number = if signal == "TERM" { 15 } else { 2 };
    assert_eq!(status.signal(), Some(number), "{status}");
    assert!(!socket.exists(), "the shell removed its socket");
    let deadline = Instant::now() + TIMEOUT;
    while Path::new(&format!("/proc/{pid}")).exists() {
      assert!(Instant::now() < deadline, "its program {pid} still runs");
      std::thread::sleep(Duration::from_millis(50));
    }
  }
}

/// `cx.exit(code)` ends the shell with that status, its programs stopped
/// and its IPC socket removed, as a greeter does once its session starts.
/// glibc's `exit` also drops the services, so the program check holds even
/// without the explicit stop, which stays because Rust doesn't promise it.
/// A program started just before, still waiting for its activation token,
/// starts without it.
#[test]
#[ignore = "needs headless Sway"]
fn cx_exit_ends_the_shell_with_its_status_programs_and_socket() {
  use std::io::Write;

  run_as_child();
  let sway = Sway::start();
  let started = sway.runtime_dir().join("started");
  let mut shell = ExampleBar::spawn(
    &sway,
    this_as_shell("cx_exit_ends_the_shell_with_its_status_programs_and_socket"),
    &[
      (CHILD, "exits"),
      ("EXIT_STARTS", &started.display().to_string()),
    ],
  );
  let line = shell.wait_for("the program is ");
  let pid: u32 = line
    .rsplit(' ')
    .next()
    .and_then(|pid| pid.trim().parse().ok())
    .expect("a pid");
  let socket = sway
    .runtime_dir()
    .join("crabshell")
    .join(format!("ipc-{}.sock", sway.display()));
  let mut client = std::os::unix::net::UnixStream::connect(&socket).expect("the IPC socket");
  writeln!(
    client,
    r#"{{"target":"shell","function":"exit","args":[]}}"#
  )
  .expect("asked");
  let status = shell.exit_status().expect("the shell exits");
  assert_eq!(status.code(), Some(3), "{status}");
  assert!(!socket.exists(), "the shell removed its socket");
  let deadline = Instant::now() + TIMEOUT;
  while !started.exists() {
    assert!(
      Instant::now() < deadline,
      "the program started at exit never ran"
    );
    std::thread::sleep(Duration::from_millis(50));
  }
  let deadline = Instant::now() + TIMEOUT;
  while Path::new(&format!("/proc/{pid}")).exists() {
    assert!(Instant::now() < deadline, "its program {pid} still runs");
    std::thread::sleep(Duration::from_millis(50));
  }
}

/// A stand-in for greetd on the other end of `GREETD_SOCK`.
struct FakeGreetd(std::os::unix::net::UnixStream);

impl FakeGreetd {
  /// The greeter's next request, as JSON text.
  fn request(&mut self) -> String {
    use std::io::Read;
    let mut length = [0; 4];
    self.0.read_exact(&mut length).expect("a request's length");
    let mut body = vec![0; u32::from_ne_bytes(length) as usize];
    self.0.read_exact(&mut body).expect("a request");
    String::from_utf8(body).expect("UTF-8")
  }

  fn reply(&mut self, json: &str) {
    use std::io::Write;
    let length = u32::try_from(json.len()).expect("small").to_ne_bytes();
    self.0.write_all(&length).expect("a reply's length");
    self.0.write_all(json.as_bytes()).expect("a reply");
  }
}

/// Phase 11's greeter check: the greeter example, typed into, makes a
/// session for its user, asks again after a refused password, starts the
/// session once authenticated and exits 0, as greetd needs.
#[test]
#[ignore = "needs headless Sway"]
fn the_greeter_logs_in_after_a_refused_password_and_exits() {
  const SECRET: &str =
    r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password:"}"#;
  const ASKS: &str = "greetd: Asking";
  let sway = Sway::start();
  let socket = sway.runtime_dir().join("greetd.sock");
  let listener = std::os::unix::net::UnixListener::bind(&socket).expect("the fake greetd");
  let mut keyboard = VirtualKeyboard::new(&sway);
  let greeter = ExampleBar::start(
    &sway,
    "greeter",
    false,
    &[("GREETD_SOCK", socket.to_str().expect("UTF-8"))],
  );
  greeter.wait_for(FIRST_FRAME);
  for key in [keys::C, keys::A, keys::L, keys::ENTER] {
    keyboard.tap(key);
  }
  let (stream, _) = listener.accept().expect("the greeter connects");
  stream.set_read_timeout(Some(TIMEOUT)).expect("a timeout");
  let mut greetd = FakeGreetd(stream);
  assert_eq!(
    greetd.request(),
    r#"{"type":"create_session","username":"cal"}"#
  );
  greetd.reply(SECRET);
  greeter.wait_for(ASKS);

  // A wrong password: the attempt ends and greetd asks again.
  for key in [keys::I, keys::C, keys::E, keys::ENTER] {
    keyboard.tap(key);
  }
  assert_eq!(
    greetd.request(),
    r#"{"type":"post_auth_message_response","response":"ice"}"#
  );
  greetd.reply(r#"{"type":"error","error_type":"auth_error","description":"Wrong password"}"#);
  assert_eq!(greetd.request(), r#"{"type":"cancel_session"}"#);
  greetd.reply(r#"{"type":"success"}"#);
  assert_eq!(
    greetd.request(),
    r#"{"type":"create_session","username":"cal"}"#
  );
  greetd.reply(SECRET);
  greeter.wait_for(ASKS);

  for key in [keys::F, keys::I, keys::R, keys::E, keys::ENTER] {
    keyboard.tap(key);
  }
  assert_eq!(
    greetd.request(),
    r#"{"type":"post_auth_message_response","response":"fire"}"#
  );
  greetd.reply(r#"{"type":"success"}"#);
  assert_eq!(
    greetd.request(),
    r#"{"cmd":["sway"],"env":[],"type":"start_session"}"#
  );
  greetd.reply(r#"{"type":"success"}"#);
  let mut greeter = greeter;
  let status = greeter.exit_status().expect("the greeter exits");
  assert_eq!(status.code(), Some(0), "{status}");
}

/// The test lock screens' background.
const MAGENTA: Color = Color::rgb(0xff, 0x00, 0xff);

/// A magenta lock screen showing each output's name, locked while `on`.
fn lock_screen(on: impl Fn() -> bool + 'static) -> SessionLock {
  SessionLock::new()
    .locked(on)
    .style(Style::new().background(MAGENTA))
    .surface(|_, output| Text::new(output.name()))
}

/// The lock surfaces the platform holds, by output.
fn lock_surfaces(platform: &Platform) -> Vec<String> {
  platform
    .windows()
    .filter(|(_, window)| window.view().root().path().starts_with("SessionLock["))
    .map(|(_, window)| window.output().name())
    .collect()
}

#[test]
#[ignore = "needs headless Sway"]
fn cx_session_lock_follows_the_compositor_and_ends_with_its_context() {
  let sway = Sway::start();
  let runtime = Runtime::current();
  type Kept = Option<(Cx, Signal<bool>, ReadSignal<bool>, usize)>;
  let kept: std::rc::Rc<std::cell::RefCell<Kept>> = Default::default();
  let hold = std::rc::Rc::clone(&kept);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let before = Runtime::current().node_count();
    let child = cx.child();
    let on = child.signal(false);
    let locked = child.session_lock(lock_screen(move || on.get()));
    *hold.borrow_mut() = Some((child, on, locked, before));
  })
  .expect("the root closure runs");
  let (on, locked) = kept
    .borrow()
    .as_ref()
    .map(|(_, on, locked, _)| (*on, *locked))
    .expect("the root ran");
  settle(&mut shell);
  assert!(!locked.get_untracked());
  assert!(lock_surfaces(shell.platform()).is_empty());

  on.set(true);
  run_until(&mut shell, "the compositor to lock", |_| {
    locked.get_untracked()
  });
  assert_eq!(lock_surfaces(shell.platform()), ["HEADLESS-1"]);
  on.set(false);
  run_until(&mut shell, "the unlock", |platform| {
    !locked.get_untracked() && lock_surfaces(platform).is_empty()
  });

  let (child, _, _, before) = kept.borrow_mut().take().expect("the root ran");
  child.owner().dispose();
  run_until(&mut shell, "the lock to go", |platform| {
    platform.session_locks() == 0
  });
  runtime.flush();
  assert_eq!(runtime.node_count(), before, "everything it made is freed");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_surface_builder_that_panics_is_logged_and_its_output_is_still_covered() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  sway.msg(&["create_output"]);
  let logs = Logs::default();
  let _guard = logs.install();
  type Kept = Option<(Signal<bool>, ReadSignal<bool>)>;
  let kept: std::rc::Rc<std::cell::Cell<Kept>> = Default::default();
  let hold = std::rc::Rc::clone(&kept);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let on = cx.signal(true);
    cx.per_output(counter::bar);
    let locked = cx.session_lock(
      SessionLock::new()
        .locked(move || on.get())
        .style(Style::new().background(MAGENTA))
        .surface(|_, output| {
          if output.name() == "HEADLESS-1" {
            panic!("no lock screen here");
          }
          Text::new(output.name())
        }),
    );
    hold.set(Some((on, locked)));
  })
  .expect("the root closure runs");
  let (on, locked) = kept.get().expect("the root ran");
  run_until(&mut shell, "the compositor to lock", |_| {
    locked.get_untracked()
  });
  let text = logs.text();
  assert!(
    text.contains(
      "SessionLock[HEADLESS-1] panicked while it was built, so it shows only the lock's style: no lock screen here"
    ),
    "{text}"
  );
  assert_eq!(
    lock_surfaces(shell.platform()),
    ["HEADLESS-1", "HEADLESS-2"]
  );
  // The style still covers HEADLESS-1.
  let deadline = Instant::now() + TIMEOUT;
  while !sway
    .screenshot("HEADLESS-1")
    .rgba
    .as_chunks::<4>()
    .0
    .iter()
    .all(|pixel| pixel[..3] == [0xff, 0x00, 0xff])
  {
    assert!(
      Instant::now() < deadline,
      "HEADLESS-1 never showed the style"
    );
    shell
      .dispatch(Some(Duration::from_millis(50)))
      .expect("the connection works");
  }

  // The shell goes on: it unlocks, and its bars are there.
  on.set(false);
  run_until(&mut shell, "the unlock", |platform| {
    !locked.get_untracked() && lock_surfaces(platform).is_empty()
  });
  let platform = shell.platform();
  assert!(shown_on(platform, "HEADLESS-1").is_some() && shown_on(platform, "HEADLESS-2").is_some());
}

/// The shell's half of a reload while locked: the session stays locked as
/// the first shell exits, and the next, starting from the persisted
/// `locked`, locks it again.
#[test]
#[ignore = "needs headless Sway"]
fn a_shell_reloaded_while_locked_locks_again_from_its_persisted_value() {
  use std::io::Write;

  run_as_child();
  let sway = Sway::start();
  let socket = sway.runtime_dir().join("control.sock");
  let listener = std::os::unix::net::UnixListener::bind(&socket).expect("the control socket");
  let control = socket.to_str().expect("a UTF-8 path");
  let test = "a_shell_reloaded_while_locked_locks_again_from_its_persisted_value";
  let env = [(CHILD, "lock"), ("CRABSHELL_CONTROL", control)];

  let mut first = ExampleBar::spawn(&sway, this_as_shell(test), &env);
  let mut supervisor = accept_ready(&listener);
  assert_eq!(ipc_call(&sway, "lock", "on"), "{\"ok\":\"\"}\n");
  first.wait_for("the session is locked: true");
  supervisor.write_all(b"save\n").expect("told to save");
  assert_eq!(
    first.exit_status().and_then(|status| status.code()),
    Some(0)
  );
  // The session stays locked between the two shells.
  wait_for_shot(&sway, "HEADLESS-1", "Sway's red", |shot| {
    mostly(shot, [0xff, 0x00, 0x00])
  });

  let second = ExampleBar::spawn(&sway, this_as_shell(test), &env);
  let _supervisor = accept_ready(&listener);
  let line = second.wait_for("the session is locked: true");
  eprintln!("the next shell: {line}");
}

/// Phase 11's exit check: the lock example, locked over IPC, matches its
/// goldens at scales 1, 1.5 and 2, shows typing as dots, says a refused
/// password was wrong, and sleeps while locked. Its PAM service is
/// `other`, which refuses everyone without asking, so no real account is
/// reached.
#[test]
#[ignore = "needs headless Sway"]
fn the_lock_screen_locks_over_ipc_refuses_a_wrong_password_and_sleeps() {
  const TEST: &str = "the_lock_screen_locks_over_ipc_refuses_a_wrong_password_and_sleeps";
  run_as_child();
  for (scale, suffix) in [("1", "1x"), ("1.5", "1.5x"), ("2", "2x")] {
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let shell = ExampleBar::spawn(&sway, this_as_shell(TEST), &[(CHILD, "lock example")]);
    assert_eq!(ipc_call(&sway, "lock", "on"), "{\"ok\":\"\"}\n");
    shell.wait_for(FIRST_FRAME);
    let shot = steady_shot(&sway, "HEADLESS-1");
    check_golden(&format!("lock@{suffix}"), &shot, shot.height);
  }

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  // The seat needs a keyboard for the lock surface to take.
  let mut keyboard = VirtualKeyboard::new(&sway);
  let shell = ExampleBar::spawn(&sway, this_as_shell(TEST), &[(CHILD, "lock example")]);
  assert_eq!(ipc_call(&sway, "lock", "on"), "{\"ok\":\"\"}\n");
  shell.wait_for(FIRST_FRAME);
  let locked = steady_shot(&sway, "HEADLESS-1");
  for key in [keys::I, keys::C, keys::E] {
    keyboard.tap(key);
  }
  wait_for_shot(&sway, "HEADLESS-1", "the dots", |shot| {
    shot.rgba != locked.rgba
  });
  let dots = steady_shot(&sway, "HEADLESS-1");
  check_golden("lock-typed@1x", &dots, dots.height);
  // PAM's answer empties the field and shows the notice in one frame.
  keyboard.tap(keys::ENTER);
  wait_for_shot(&sway, "HEADLESS-1", "the refusal", |shot| {
    shot.rgba != dots.rgba
  });
  let refused = steady_shot(&sway, "HEADLESS-1");
  check_golden("lock-refused@1x", &refused, refused.height);

  let wakeups = shell.wakeups_for(Duration::from_secs(10));
  assert!(wakeups.is_empty(), "woke while locked: {wakeups:?}");
  assert_eq!(
    sway.screenshot("HEADLESS-1").rgba,
    refused.rgba,
    "still locked"
  );
}

/// Whether more than half of `shot` is `rgb`.
fn mostly(shot: &Image, rgb: [u8; 3]) -> bool {
  let pixels = shot.rgba.as_chunks::<4>().0;
  pixels.iter().filter(|pixel| pixel[..3] == rgb).count() > pixels.len() / 2
}

/// A shell that exits once its lock's signal says unlocked leaves the
/// session unlocked: the signal waits for the compositor to have the
/// unlock.
#[test]
#[ignore = "needs headless Sway"]
fn a_shell_that_exits_once_unlocked_leaves_the_session_unlocked() {
  run_as_child();
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  let test = "a_shell_that_exits_once_unlocked_leaves_the_session_unlocked";
  let mut shell = ExampleBar::spawn(&sway, this_as_shell(test), &[(CHILD, "lock and exit")]);
  assert_eq!(ipc_call(&sway, "lock", "on"), "{\"ok\":\"\"}\n");
  shell.wait_for("the session is locked: true");
  assert_eq!(ipc_call(&sway, "lock", "off"), "{\"ok\":\"\"}\n");
  assert_eq!(
    shell.exit_status().and_then(|status| status.code()),
    Some(0)
  );
  // Locked, Sway shows the lock's magenta, and its red once the locker is
  // gone; unlocked, neither.
  wait_for_shot(&sway, "HEADLESS-1", "the unlocked desktop", |shot| {
    !mostly(shot, [0xff, 0x00, 0x00]) && !mostly(shot, [0xff, 0x00, 0xff])
  });
}

/// The lock example keeps `locked` under `$XDG_RUNTIME_DIR`, so a shell
/// restarted after a crash while locked locks again and can unlock.
#[test]
#[ignore = "needs headless Sway"]
fn the_lock_example_locks_again_after_a_crash() {
  run_as_child();
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let test = "the_lock_example_locks_again_after_a_crash";
  let mut first = ExampleBar::spawn(&sway, this_as_shell(test), &[(CHILD, "lock example")]);
  assert_eq!(ipc_call(&sway, "lock", "on"), "{\"ok\":\"\"}\n");
  first.wait_for(FIRST_FRAME);
  let locked = steady_shot(&sway, "HEADLESS-1");
  first.child.kill().expect("killed, as a crash");
  let _ = first.child.wait();
  wait_for_shot(&sway, "HEADLESS-1", "Sway's red", |shot| {
    mostly(shot, [0xff, 0x00, 0x00])
  });
  let second = ExampleBar::spawn(&sway, this_as_shell(test), &[(CHILD, "lock example")]);
  second.wait_for(FIRST_FRAME);
  wait_for_shot(&sway, "HEADLESS-1", "the lock screen again", |shot| {
    shot.rgba == locked.rgba
  });
}

/// The lock example's background, which fills a locked screen.
const LOCK_SCREEN: [u8; 3] = [0x11, 0x11, 0x1b];
/// What the shell logs when the compositor says the session is locked.
const LOCKED: &str = "the compositor locked the session";

/// Phase 18's exit check for logind: the lock example holds a delay
/// inhibitor from a fake logind, and when logind says the machine is
/// about to sleep, it lets go only after the compositor said the session
/// is locked, and its lock screen shows.
#[test]
#[ignore = "needs headless Sway"]
fn the_lock_example_locks_before_the_machine_sleeps() {
  const TEST: &str = "the_lock_example_locks_before_the_machine_sleeps";
  run_as_child();
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  let bus = PrivateBus::start();
  let logind = FakeLogind::start(&bus);
  let shell = ExampleBar::spawn(
    &sway,
    this_as_shell(TEST),
    &[
      (CHILD, "lock example"),
      ("DBUS_SYSTEM_BUS_ADDRESS", &bus.address),
    ],
  );
  let inhibitor = logind
    .inhibitors
    .recv_timeout(Duration::from_secs(10))
    .expect("the lock example asks for an inhibitor");
  let let_go = hung_up(inhibitor);
  std::thread::sleep(Duration::from_millis(500));
  assert!(let_go.try_recv().is_err(), "let go before any sleep");
  let early = shell.drain();
  assert!(!early.iter().any(|line| line.contains(LOCKED)), "{early:?}");

  logind.prepare_for_sleep(&bus, true);
  let let_go_at = let_go
    .recv_timeout(Duration::from_secs(10))
    .expect("the inhibitor let go");
  let line = shell.wait_for(LOCKED);
  let locked_at: crabshell::jiff::Timestamp = line
    .split_whitespace()
    .next()
    .and_then(|time| time.parse().ok())
    .unwrap_or_else(|| panic!("no time in {line:?}"));
  assert!(
    locked_at <= let_go_at,
    "let go at {let_go_at}, before the lock at {locked_at}"
  );
  wait_for_shot(&sway, "HEADLESS-1", "the lock screen", |shot| {
    mostly(shot, LOCK_SCREEN)
  });
  // After the resume, it holds sleep off again.
  logind.prepare_for_sleep(&bus, false);
  logind
    .inhibitors
    .recv_timeout(Duration::from_secs(10))
    .expect("a new inhibitor after the resume");
}

/// A channel that gets the time once every write end of `pipe` is
/// closed, as logind sees an inhibitor go.
fn hung_up(mut pipe: std::io::PipeReader) -> mpsc::Receiver<crabshell::jiff::Timestamp> {
  use std::io::Read;

  let (sender, receiver) = mpsc::channel();
  std::thread::spawn(move || {
    let mut byte = [0];
    while pipe.read(&mut byte).is_ok_and(|read| read > 0) {}
    let _ = sender.send(crabshell::jiff::Timestamp::now());
  });
  receiver
}

/// A stand-in for logind on a private system bus, answering on a thread
/// of its own until the bus goes. `Inhibit` hands out a pipe's write end
/// and sends its read end to [`inhibitors`](Self::inhibitors).
struct FakeLogind {
  inhibitors: mpsc::Receiver<std::io::PipeReader>,
}

impl FakeLogind {
  fn start(bus: &PrivateBus) -> FakeLogind {
    let (sender, inhibitors) = mpsc::channel();
    let (up, started) = mpsc::channel();
    let address = bus.address.clone();
    std::thread::spawn(move || zbus::block_on(fake_logind(address, sender, up)));
    started
      .recv_timeout(Duration::from_secs(5))
      .expect("the fake logind on the bus");
    FakeLogind { inhibitors }
  }

  /// Has it send `PrepareForSleep(start)`. Only logind's own connection
  /// counts, so a test method of its own asks it to.
  fn prepare_for_sleep(&self, bus: &PrivateBus, start: bool) {
    let output = Command::new("gdbus")
      .args(["call", "--address", &bus.address])
      .args(["--dest", "org.freedesktop.login1"])
      .args(["--object-path", "/org/freedesktop/login1"])
      .args(["--method", "org.example.FakeLogind.PrepareForSleep"])
      .arg(start.to_string())
      .output()
      .expect("gdbus runs");
    assert!(output.status.success(), "gdbus: {output:?}");
  }
}

/// The fake logind: `Inhibit`, `Get` of `PreparingForSleep`, which is
/// false, and the test's `PrepareForSleep`.
async fn fake_logind(
  address: String,
  inhibitors: mpsc::Sender<std::io::PipeReader>,
  up: mpsc::Sender<()>,
) {
  use std::future::poll_fn;
  use std::os::fd::OwnedFd;
  use std::pin::Pin;

  use zbus::export::futures_core::Stream;

  let connection = zbus::connection::Builder::address(address.as_str())
    .name("org.freedesktop.login1")
    .build()
    .await
    .expect("the fake logind joins the bus");
  let mut messages = zbus::MessageStream::from(&connection);
  let _ = up.send(());
  while let Some(Ok(call)) = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await {
    let header = call.header();
    if header.message_type() != zbus::message::Type::MethodCall {
      continue;
    }
    let reply = zbus::Message::method_return(&header);
    let reply = match header.member().map(|member| member.as_str()) {
      Some("Inhibit") => {
        let (read, write) = std::io::pipe().expect("a pipe");
        let _ = inhibitors.send(read);
        reply.build(&zbus::Fd::from(OwnedFd::from(write)))
      }
      Some("Get") => reply.build(&zbus::Variant::from(false)),
      Some("PrepareForSleep") => {
        let start: bool = call.body().deserialize().expect("a bool");
        let (path, interface) = ("/org/freedesktop/login1", "org.freedesktop.login1.Manager");
        connection
          .emit_signal(None::<&str>, path, interface, "PrepareForSleep", &start)
          .await
          .expect("PrepareForSleep sent");
        reply.build(&())
      }
      _ => zbus::Message::error(&header, "org.freedesktop.DBus.Error.UnknownMethod")
        .build(&"the fake logind has no such method"),
    };
    if connection.send(&reply.expect("a reply")).await.is_err() {
      return;
    }
  }
}

/// A stand-in for Hyprland: its two sockets under the private Sway's
/// runtime directory, answered on threads of their own from `lists`.
/// Hyprland itself is never started.
struct FakeHyprland {
  /// `j/workspaces`, `j/monitors`, `j/clients` and `j/activewindow`.
  lists: Arc<Mutex<[String; 4]>>,
  commands: Arc<Mutex<Vec<String>>>,
  events: Arc<Mutex<Option<std::os::unix::net::UnixStream>>>,
}

impl FakeHyprland {
  const SIGNATURE: &str = "crabshell-test";

  /// HEADLESS-1 shows workspace 1, which has the focus and a window
  /// titled `notes`, and workspace 2 is on it too.
  fn start(sway: &Sway) -> FakeHyprland {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    let dir = sway.runtime_dir().join("hypr").join(Self::SIGNATURE);
    std::fs::create_dir_all(&dir).expect("Hyprland's directory");
    let fake = FakeHyprland {
      lists: Arc::new(Mutex::new([
        String::new(),
        String::new(),
        String::new(),
        String::new(),
      ])),
      commands: Arc::default(),
      events: Arc::default(),
    };
    fake.focus(1);
    let requests = UnixListener::bind(dir.join(".socket.sock")).expect("the request socket");
    let (lists, commands) = (Arc::clone(&fake.lists), Arc::clone(&fake.commands));
    std::thread::spawn(move || {
      for stream in requests.incoming() {
        let Ok(mut stream) = stream else { return };
        let mut bytes = vec![0; 64 * 1024];
        let Ok(read) = stream.read(&mut bytes) else {
          continue;
        };
        let command = String::from_utf8_lossy(&bytes[..read]).into_owned();
        let lists = lists.lock().expect("the lists");
        let reply = match command.as_str() {
          "j/workspaces" => lists[0].clone(),
          "j/monitors" => lists[1].clone(),
          "j/clients" => lists[2].clone(),
          "j/activewindow" => lists[3].clone(),
          "j/status" => String::from(r#"{"configProvider":"hyprlang","backend":"drm"}"#),
          _ => {
            commands.lock().expect("the commands").push(command);
            String::from("ok")
          }
        };
        let _ = stream.write_all(reply.as_bytes());
      }
    });
    let events = UnixListener::bind(dir.join(".socket2.sock")).expect("the event socket");
    let stream = Arc::clone(&fake.events);
    std::thread::spawn(move || {
      if let Ok((accepted, _)) = events.accept() {
        *stream.lock().expect("the event stream") = Some(accepted);
      }
    });
    fake
  }

  /// Workspace `id`, 1 or 2, gets the focus.
  fn focus(&self, id: u32) {
    let workspace = |n: u32| {
      format!(
        r#"{{"id":{n},"name":"{n}","monitor":"HEADLESS-1","monitorID":0,"windows":{},"hasfullscreen":false}}"#,
        u32::from(n == 1)
      )
    };
    *self.lists.lock().expect("the lists") = [
      format!("[{},{}]", workspace(1), workspace(2)),
      format!(
        r#"[{{"id":0,"name":"HEADLESS-1","description":"","width":1920,"height":1080,"x":0,"y":0,"activeWorkspace":{{"id":{id},"name":"{id}"}},"scale":1.00,"focused":true}}]"#
      ),
      String::from(
        r#"[{"address":"0x5a1","workspace":{"id":1,"name":"1"},"floating":false,"monitor":0,"class":"notes","title":"notes","fullscreen":0}]"#,
      ),
      if id == 1 {
        String::from(r#"{"address":"0x5a1","class":"notes","title":"notes"}"#)
      } else {
        String::from("{}")
      },
    ];
  }

  /// Sends `line` on the event socket, once the shell connected to it.
  fn send(&self, line: &str) {
    use std::io::Write;
    let deadline = Instant::now() + TIMEOUT;
    loop {
      if let Some(stream) = self.events.lock().expect("the event stream").as_mut() {
        stream
          .write_all(format!("{line}\n").as_bytes())
          .expect("an event sent");
        return;
      }
      assert!(Instant::now() < deadline, "the shell never followed events");
      std::thread::sleep(Duration::from_millis(20));
    }
  }

  fn commands(&self) -> Vec<String> {
    self.commands.lock().expect("the commands").clone()
  }
}

/// Phase 12's exit check: the Hyprland example, against a fake Hyprland
/// on headless Sway, matches its goldens at scales 1, 1.5 and 2, switches
/// workspaces on a click, follows Hyprland's events, and sleeps.
#[test]
#[ignore = "needs headless Sway"]
fn the_hyprland_bar_follows_hyprland_switches_and_sleeps() {
  let env = [("HYPRLAND_INSTANCE_SIGNATURE", FakeHyprland::SIGNATURE)];
  for (scale, factor, suffix) in [("1", 1.0, "1x"), ("1.5", 1.5, "1.5x"), ("2", 2.0, "2x")] {
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let _hyprland = FakeHyprland::start(&sway);
    let bar = ExampleBar::start(&sway, "hyprland", false, &env);
    bar.wait_for(FIRST_FRAME);
    let shot = steady_shot(&sway, "HEADLESS-1");
    check_golden(&format!("hyprland@{suffix}"), &shot, (30.0 * factor) as u32);
  }

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let hyprland = FakeHyprland::start(&sway);
  // A headless seat has no pointer until a program gives it one.
  let mut pointer = VirtualPointer::new(&sway);
  let bar = ExampleBar::start(&sway, "hyprland", false, &env);
  bar.wait_for(FIRST_FRAME);
  let before = steady_shot(&sway, "HEADLESS-1");

  // Workspace 2's button: 8 of padding, 28 for workspace 1, a gap of 4.
  pointer.move_to(8 + 28 + 4 + 14, 15, (1920, 1080));
  pointer.click();
  let deadline = Instant::now() + TIMEOUT;
  while !hyprland
    .commands()
    .contains(&String::from("dispatch workspace 2"))
  {
    assert!(Instant::now() < deadline, "{:?}", hyprland.commands());
    std::thread::sleep(Duration::from_millis(20));
  }

  // Hyprland switches and says so; the bar follows.
  hyprland.focus(2);
  hyprland.send("workspacev2>>2,2");
  wait_for_shot(&sway, "HEADLESS-1", "workspace 2 focused", |shot| {
    shot.rgba[..1920 * 30 * 4] != before.rgba[..1920 * 30 * 4]
  });

  let wakeups = bar.wakeups_for(Duration::from_secs(10));
  assert!(wakeups.is_empty(), "woke while idle: {wakeups:?}");
}

/// The motion example's output: 1680 by 1050 is a whole number of logical
/// pixels at every scale from 1 to 2 in quarters.
const MOTION_MODE: &str = "1680x1050";

/// A sink at `percent`, for the motion example's OSD.
fn sink_at(percent: u32) -> Sink {
  Sink {
    name: String::from("test"),
    volume_percent: percent,
    muted: false,
  }
}

/// A notification with only an id and a summary.
fn notification(id: u32, summary: &str) -> Notification {
  Notification {
    id,
    app_name: String::new(),
    app_icon: String::new(),
    summary: summary.to_owned(),
    body: String::from("from the test"),
    actions: Vec::new(),
    urgency: Urgency::Normal,
    timeout: Timeout::Never,
    image: None,
    desktop_entry: String::new(),
    category: String::new(),
    resident: false,
    transient: false,
    received: SystemTime::now(),
  }
}

/// The window whose namespace is `namespace`.
fn window_named(platform: &Platform, namespace: &str) -> WindowId {
  platform
    .windows()
    .find(|(_, window)| window.surface().namespace == namespace)
    .map(|(id, _)| id)
    .unwrap_or_else(|| panic!("no {namespace} window"))
}

/// What `id` takes pointer input in, as its mask says.
fn mask_of(platform: &Platform, id: WindowId) -> Option<Region> {
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the window is held");
  window.surface().mask
}

/// How many toasts the motion example's toasts window holds, leaving
/// ones included.
fn toast_count(platform: &Platform, id: WindowId) -> usize {
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the toasts are held");
  let view = window.view();
  let column = view.root().children().next().expect("the toasts' column");
  column.children().count()
}

/// Dispatches until `at`, 20 ms at a time, so a signal set from here is
/// flushed at once.
fn dispatch_until(shell: &mut Shell, at: Instant) {
  while Instant::now() < at {
    let left = at.saturating_duration_since(Instant::now());
    shell
      .dispatch(Some(left.min(Duration::from_millis(20))))
      .expect("the connection works");
  }
}

/// Part of Phase 14's exit check, in this process with plain signals for
/// the services: the motion example's spinner draws only while it turns,
/// at scale 2, and rests where it started; its OSD shows on a change,
/// hides 1.5 s after the last one, slides out and then lets the pointer
/// through; and a toast slides in, leaves over its exit and frees
/// everything it made.
#[test]
#[ignore = "needs headless Sway"]
fn the_motion_windows_draw_only_while_they_move() {
  type Handles = (
    Signal<bool>,
    Signal<Option<Sink>>,
    Signal<Vec<Notification>>,
  );
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", MOTION_MODE, "scale", "2"]);
  let runtime = Runtime::current();
  let handles: Rc<std::cell::Cell<Option<Handles>>> = Rc::default();
  let held = Rc::clone(&handles);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let busy = cx.signal(false);
    let sink = cx.signal(Some(sink_at(50)));
    let popups = cx.signal(Vec::new());
    let pinned = cx.signal(false);
    held.set(Some((busy, sink, popups)));
    cx.per_output(move |cx, output| {
      vec![
        motion::bar(cx, output, busy.read_only()),
        motion::osd(cx, output, sink.read_only(), pinned.read_only()),
        motion::toasts(output, popups.read_only()),
      ]
    });
  })
  .expect("the root closure runs");
  let (busy, sink, popups) = handles.take().expect("the root closure ran");
  run_until(&mut shell, "every window's first frame", |platform| {
    platform.windows().all(|(id, _)| {
      platform
        .window_info(id)
        .is_some_and(|info| info.commits > 0)
    }) && platform.windows().count() == 3
  });
  settle(&mut shell);
  let platform = shell.platform();
  let osd = window_named(platform, "osd");
  let toasts = window_named(platform, "toasts");
  let bar = platform
    .windows()
    .map(|(id, _)| id)
    .find(|id| ![osd, toasts].contains(id))
    .expect("the bar");
  // Hidden from the start, and never taking space.
  assert_eq!(mask_of(platform, osd), Some(Region::empty()));
  let (_, window) = platform.windows().find(|(id, _)| *id == osd).expect("held");
  assert_eq!(window.surface().exclusive_zone, 0);

  // The spinner turns while busy, and only then.
  let resting = shell.platform().snapshot(bar).expect("the bar draws");
  let before = shell.platform().stats();
  busy.set(true);
  dispatch_until(&mut shell, Instant::now() + Duration::from_secs(2));
  let turning = shell.platform().stats();
  let frames = turning.commits - before.commits;
  // 2 s at 60 Hz is 120 frames.
  assert!(frames >= 60, "{frames} frames in 2 s of turning");
  eprintln!(
    "the spinner drew {frames} frames in 2 s at scale 2, in {:.3} ms each",
    (turning.drawing - before.drawing).as_secs_f64() * 1000.0 / frames as f64
  );
  busy.set(false);
  settle(&mut shell);
  let rested = shell.platform().snapshot(bar).expect("the bar draws");
  assert!(
    rested.rgba == resting.rgba,
    "the spinner rests where it began"
  );
  // Asleep: one dispatch sleeps the whole time.
  let before = shell.platform().stats().iterations;
  let started = Instant::now();
  shell
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  let slept = started.elapsed();
  assert!(slept >= Duration::from_millis(1900), "woke after {slept:?}");
  assert_eq!(shell.platform().stats().iterations, before + 1);

  // A change shows the OSD; a second one 1 s later keeps it 1.5 s more.
  let changed = Instant::now();
  let before = shell.platform().stats().commits;
  sink.set(Some(sink_at(60)));
  run_until(&mut shell, "the OSD to show", |platform| {
    mask_of(platform, osd).is_none()
  });
  dispatch_until(&mut shell, changed + Duration::from_millis(600));
  let shown = shell.platform().stats().commits;
  assert!(
    shown - before >= 5,
    "it slid in over {} frames",
    shown - before
  );
  dispatch_until(&mut shell, changed + Duration::from_millis(1000));
  assert_eq!(shell.platform().stats().commits, shown, "still while shown");
  sink.set(Some(sink_at(70)));
  run_until(&mut shell, "the OSD to hide and slide out", |platform| {
    mask_of(platform, osd) == Some(Region::empty())
  });
  let hidden = changed.elapsed();
  // 1 s, then 1.5 s, then the 300 ms slide.
  assert!(
    (Duration::from_millis(2800)..Duration::from_millis(3600)).contains(&hidden),
    "hidden after {hidden:?}"
  );
  let empty = shell.platform().snapshot(osd).expect("the OSD draws");
  assert!(empty.rgba.iter().all(|byte| *byte == 0), "nothing shows");

  // A toast slides in, then out over its exit, and frees what it made.
  settle(&mut shell);
  let nodes = runtime.node_count();
  let before = shell.platform().stats().commits;
  popups.set(vec![notification(1, "Hello")]);
  run_until(&mut shell, "the toast", |platform| {
    toast_count(platform, toasts) == 1
  });
  settle(&mut shell);
  let slid = shell.platform().stats().commits - before;
  assert!(slid >= 5, "it slid in over {slid} frames");
  let shot = shell.platform().snapshot(toasts).expect("the toasts draw");
  // 6 logical pixels into the toast, at scale 2.
  let at = (12 * shot.width as usize + 12) * 4;
  assert_eq!(
    &shot.rgba[at..at + 4],
    &[0x31, 0x32, 0x44, 0xff],
    "in place"
  );
  let left = Instant::now();
  let before = shell.platform().stats().commits;
  popups.set(Vec::new());
  run_until(&mut shell, "the toast to go", |platform| {
    toast_count(platform, toasts) == 0
  });
  let stayed = left.elapsed();
  let frames = shell.platform().stats().commits - before;
  assert!(
    stayed >= Duration::from_millis(250),
    "it left after {stayed:?}"
  );
  assert!(frames >= 5, "it slid out over {frames} frames");
  settle(&mut shell);
  assert_eq!(runtime.node_count(), nodes, "the toast freed what it made");
  let empty = shell.platform().snapshot(toasts).expect("the toasts draw");
  assert!(empty.rgba.iter().all(|byte| *byte == 0), "nothing shows");
}

/// The motion example's OSD card color, from examples/motion.rs.
const OSD_CARD: [u8; 3] = [0x31, 0x32, 0x44];

/// The motion example's accent color, which fills the OSD's volume bar.
const ACCENT: [u8; 3] = [0x89, 0xb4, 0xfa];

/// Whether a screenshot at `scale` shows the motion example's OSD with a
/// volume, so not "no audio", and its first toast in place: a point in
/// the volume bar's fill at 25% and one 6 pixels into the toast.
fn motion_settled(shot: &Image, scale: f32) -> bool {
  let at = |x: f32, y: f32| rgb(shot, x as u32, y as u32);
  let (width, height) = (shot.width as f32, shot.height as f32);
  at(width / 2.0 - 50.0 * scale, height - 37.0 * scale) == ACCENT
    && at(width - 342.0 * scale, 42.0 * scale) == OSD_CARD
}

/// Whether the motion example's OSD card shows at the bottom middle of a
/// screenshot at scale 1.
fn osd_shows(shot: &Image) -> bool {
  let middle = shot.width / 2;
  (middle - 140..middle + 140).any(|x| rgb(shot, x, shot.height - 48) == OSD_CARD)
}

/// Phase 14's exit check: the motion example, on a private audio server
/// and session bus, matches its goldens at all five scales with its OSD
/// pinned up and a toast shown; a volume change shows the OSD until 1.5 s
/// after it; a program run over IPC turns the spinner until it ends; a
/// notification shows as a toast and goes when it expires; and then the
/// shell sleeps for a minute.
#[test]
#[ignore = "needs headless Sway and pipewire"]
fn the_motion_example_slides_spins_and_sleeps() {
  let desktop = Desktop::new("motion");
  desktop
    .server
    .pactl(&["set-sink-volume", "@DEFAULT_SINK@", "50%"]);
  for scale in ["1", "1.25", "1.5", "1.75", "2"] {
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", MOTION_MODE, "scale", scale]);
    let shell = desktop.run(&sway, "motion");
    shell.wait_for(FIRST_FRAME);
    steady_shot(&sway, "HEADLESS-1");
    // Settled states only: a toast that never expires, the OSD held up.
    desktop.notify("Hello", 0);
    assert_eq!(ipc_call(&sway, "osd", "pin"), "{\"ok\":\"\"}\n");
    // Both replies come before the shell draws, so wait to see both.
    let factor: f32 = scale.parse().expect("a scale");
    wait_for_shot(&sway, "HEADLESS-1", "the toast and the OSD", |shot| {
      motion_settled(shot, factor)
    });
    let shot = steady_shot(&sway, "HEADLESS-1");
    check_golden(&format!("motion@{scale}x"), &shot, shot.height);
  }

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", MOTION_MODE, "scale", "1"]);
  // A headless seat has no pointer until a program gives it one.
  let mut pointer = VirtualPointer::new(&sway);
  let shell = desktop.run(&sway, "motion");
  shell.wait_for(FIRST_FRAME);
  let idle = steady_shot(&sway, "HEADLESS-1");
  assert!(!osd_shows(&idle), "no OSD at startup");

  let changed = Instant::now();
  desktop
    .server
    .pactl(&["set-sink-volume", "@DEFAULT_SINK@", "60%"]);
  wait_for_shot(&sway, "HEADLESS-1", "the OSD", osd_shows);
  wait_for_shot(&sway, "HEADLESS-1", "the OSD to hide", |shot| {
    !osd_shows(shot)
  });
  let hidden = changed.elapsed();
  assert!(
    hidden >= Duration::from_millis(1500),
    "hid after {hidden:?}"
  );
  eprintln!("the OSD was gone {hidden:?} after the volume changed");
  assert!(
    steady_shot(&sway, "HEADLESS-1").rgba == idle.rgba,
    "all gone"
  );

  assert_eq!(
    ipc_call_with(&sway, "motion", "run", &["sleep", "2"]),
    "{\"ok\":\"\"}\n"
  );
  wait_for_shot(&sway, "HEADLESS-1", "the spinner", |shot| {
    shot.rgba != idle.rgba
  });
  let one = sway.screenshot("HEADLESS-1");
  std::thread::sleep(Duration::from_millis(150));
  assert!(sway.screenshot("HEADLESS-1").rgba != one.rgba, "it turns");
  assert!(
    steady_shot(&sway, "HEADLESS-1").rgba == idle.rgba,
    "it stopped once the program ended"
  );

  // Inside the toast: 8 in from the right edge, 36 down.
  let inside = (1680 - 8 - 340 + 6, 36 + 6);
  desktop.notify("Bye", 1500);
  wait_for_shot(&sway, "HEADLESS-1", "the toast", |shot| {
    rgb(shot, inside.0, inside.1) == OSD_CARD
  });
  wait_for_shot(&sway, "HEADLESS-1", "the toast to go", |shot| {
    shot.rgba == idle.rgba
  });

  // One that never expires goes when clicked.
  desktop.notify("Click me", 0);
  wait_for_shot(&sway, "HEADLESS-1", "the toast to click", |shot| {
    rgb(shot, inside.0, inside.1) == OSD_CARD
  });
  pointer.move_to(1680 - 8 - 170, 36 + 20, (1680, 1050));
  pointer.click();
  wait_for_shot(&sway, "HEADLESS-1", "the clicked toast to go", |shot| {
    shot.rgba == idle.rgba
  });

  // Asleep: no wakeups for a minute.
  shell.wait_until_quiet();
  let wakeups = shell.wakeups_for(Duration::from_secs(60));
  assert!(wakeups.is_empty(), "woke while idle: {wakeups:?}");
}

/// The date the control example's calendar is pinned to in its tests, a
/// Monday in a month that starts on a Sunday, so the first week is all
/// blanks but one.
const CONTROL_TODAY: &str = "2026-11-16";

/// The control example's slider handle color, from examples/control.rs.
const HANDLE: [u8; 3] = [0xcd, 0xd6, 0xf4];

/// The middle of the control example's slider handle in a screenshot at
/// scale 1: the middle of the longest run of its color in the panel's
/// top part. Glyphs never draw 10 pixels of one color in a row.
fn slider_handle(shot: &Image) -> Option<(u32, u32)> {
  let left = shot.width - 8 - control::WIDTH;
  let mut best: Option<(u32, u32, u32)> = None;
  for y in 8..120 {
    let mut start = left;
    for x in left..=shot.width - 8 {
      if x < shot.width - 8 && rgb(shot, x, y) == HANDLE {
        continue;
      }
      let run = x - start;
      if run >= 10 && best.is_none_or(|(_, _, longest)| run > longest) {
        best = Some(((start + x) / 2, y, run));
      }
      start = x + 1;
    }
  }
  best.map(|(x, y, _)| (x, y))
}

/// The default sink's volume in percent, as `pactl` says: its first
/// channel's.
fn pactl_volume(server: &PrivateServer) -> u32 {
  let out = server.pactl(&["get-sink-volume", "@DEFAULT_SINK@"]);
  out
    .split('%')
    .next()
    .and_then(|before| before.rsplit(' ').next())
    .and_then(|percent| percent.parse().ok())
    .unwrap_or_else(|| panic!("no volume in {out:?}"))
}

/// Waits until `pactl` says the default sink's volume is within a
/// percent of `percent`.
fn wait_for_volume(server: &PrivateServer, percent: u32) {
  let deadline = Instant::now() + TIMEOUT;
  loop {
    let now = pactl_volume(server);
    if now.abs_diff(percent) <= 1 {
      return;
    }
    assert!(
      Instant::now() < deadline,
      "the volume stayed at {now}%, not {percent}%"
    );
    std::thread::sleep(Duration::from_millis(50));
  }
}

/// The control center's day comes from a day clock, which wakes nothing
/// before midnight, and the date read from it goes with its context.
#[test]
#[ignore = "needs headless Sway"]
fn the_control_centers_day_sleeps_until_midnight_and_goes_with_its_context() {
  type Held = (Cx, Memo<crabshell::jiff::civil::Date>, usize);
  let sway = Sway::start();
  let runtime = Runtime::current();
  let held: Rc<std::cell::RefCell<Option<Held>>> = Rc::default();
  let into = Rc::clone(&held);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    // The day clock is the root's, shared by every caller.
    cx.clock(Precision::Day);
    let nodes = Runtime::current().node_count();
    let child = cx.child();
    let today = control::today(&child);
    *into.borrow_mut() = Some((child, today, nodes));
  })
  .expect("the root closure runs");
  let (child, today, nodes) = held.borrow_mut().take().expect("the root closure ran");
  // Until the first frame, and the clock's system bus watch, which
  // starts a second after the clock, are done.
  let began = Instant::now();
  let settled = (0..50).any(|_| {
    let started = Instant::now();
    shell
      .dispatch(Some(Duration::from_millis(300)))
      .expect("the connection works");
    began.elapsed() > Duration::from_millis(1500) && started.elapsed() >= Duration::from_millis(290)
  });
  assert!(settled, "the loop never slept");
  assert_eq!(today.get_untracked(), crabshell::jiff::Zoned::now().date());
  let before = shell.platform().stats().iterations;
  let started = Instant::now();
  shell
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  let slept = started.elapsed();
  assert!(slept >= Duration::from_millis(1950), "woke after {slept:?}");
  assert_eq!(shell.platform().stats().iterations, before + 1);
  child.owner().dispose();
  runtime.flush();
  assert_eq!(runtime.node_count(), nodes, "the date was freed");
}

/// The control example's highlight and panel colors, from
/// examples/control.rs.
const HIGHLIGHTED: [u8; 4] = [0x31, 0x32, 0x44, 0xff];
const PANEL_BACKGROUND: [u8; 4] = [0x1e, 0x1e, 0x2e, 0xff];

/// The notifications the in-process control panel lists, oldest first:
/// every other one without a body, so the rows differ in height.
fn twelve_notifications() -> Vec<Notification> {
  (1..=12)
    .map(|id| {
      let mut notification = notification(id, &format!("Message {id}"));
      if id % 2 == 0 {
        notification.body.clear();
      }
      notification
    })
    .collect()
}

/// The control panel's list and the rectangles of its rows built, in
/// the window's logical pixels, top first.
fn control_rows(platform: &Platform, id: WindowId) -> (Rect, Vec<Rect>) {
  let (list, rows) = control_rows_named(platform, id);
  (list, rows.into_iter().map(|(_, rect)| rect).collect())
}

/// [`control_rows`] with each row's first line, its summary.
fn control_rows_named(platform: &Platform, id: WindowId) -> (Rect, Vec<(String, Rect)>) {
  fn first_text(node: NodeView<'_>) -> Option<String> {
    if let Content::Text(text) = node.content() {
      return Some(text.runs().map(|run| run.text).collect());
    }
    node.children().find_map(first_text)
  }
  fn walk(node: NodeView<'_>, list: &mut Option<Rect>, rows: &mut Vec<(String, Rect)>) {
    if node.name() == "ListView" {
      *list = Some(node.rect());
    }
    if node.name() == "Button" && node.path().contains("/ListView") {
      rows.push((first_text(node).unwrap_or_default(), node.rect()));
    }
    for child in node.children() {
      walk(child, list, rows);
    }
  }
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the panel is held");
  let view = window.view();
  let (mut list, mut rows) = (None, Vec::new());
  walk(view.root(), &mut list, &mut rows);
  rows.sort_by(|a, b| a.1.y.total_cmp(&b.1.y));
  (list.expect("the panel has a list"), rows)
}

/// How far the control panel's list of [`twelve_notifications`] is
/// scrolled, from where its first row built is: newest first, the rows
/// are 36 pixels tall without a body and 46 with one.
fn control_offset(platform: &Platform, id: WindowId) -> f32 {
  let (list, rows) = control_rows_named(platform, id);
  let (summary, rect) = rows.first().expect("a row is built");
  let n: u32 = summary
    .strip_prefix("Message ")
    .and_then(|n| n.parse().ok())
    .unwrap_or_else(|| panic!("a row's summary is {summary:?}"));
  let index = 12 - n;
  let start = (index / 2) * 82 + (index % 2) * 36;
  list.y + start as f32 - rect.y
}

/// The pixel of a snapshot at scale 2 just inside the left of `row`,
/// halfway down: the highlight's color when it is behind the row.
fn left_of(shot: &crabshell_wayland::Snapshot, row: Rect) -> [u8; 4] {
  let (x, y) = ((row.x + 3.0) * 2.0, (row.y + row.height / 2.0) * 2.0);
  let at = (y as usize * shot.width as usize + x as usize) * 4;
  shot.rgba[at..at + 4].try_into().expect("four bytes")
}

/// Part of Phase 15's exit check, in this process with plain signals for
/// the services, at scale 2: the control panel's highlight slides to the
/// row the pointer is over, and its frames end; a touchpad fling on the
/// list runs on after the fingers lift and comes to rest; and then the
/// loop sleeps.
#[test]
#[ignore = "needs headless Sway"]
fn the_control_centers_highlight_follows_the_pointer_and_its_fling_rests() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", CONTROL_MODE, "scale", "2"]);
  let mut pointer = VirtualPointer::new(&sway);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let sink = cx.signal(Some(sink_at(50)));
    let date = CONTROL_TODAY.parse().expect("a date");
    let today = cx.memo(move || date);
    let notifications = cx.signal(twelve_notifications());
    cx.per_output(move |cx, output| {
      control::panel(
        cx,
        output,
        sink.read_only(),
        today,
        notifications.read_only(),
      )
    });
  })
  .expect("the root closure runs");
  run_until(&mut shell, "the panel's first frame", |platform| {
    shown_on(platform, "HEADLESS-1").is_some()
  });
  settle(&mut shell);
  let id = window_named(shell.platform(), "control");
  let (list, rows) = control_rows(shell.platform(), id);
  eprintln!("the list is at {list:?}, its rows at {rows:?}");
  // Where the out-of-process check points.
  let bottom = (control::HEIGHT - control::PADDING) as f32;
  assert_eq!(list.y + list.height, bottom, "the list ends at the padding");
  assert!(
    rows[0].height < rows[1].height,
    "a row with a body is taller"
  );
  // Logical pixels at scale 2: half the mode.
  let layout = (CONTROL_LAYOUT.0 / 2, CONTROL_LAYOUT.1 / 2);
  let origin = (layout.0 - 8 - control::WIDTH, 8);
  let on_screen = |row: Rect| {
    let (x, y) = middle(row);
    (origin.0 + x, origin.1 + y)
  };

  // Over the first row, the highlight shows behind it.
  let (x, y) = on_screen(rows[0]);
  pointer.move_to(x, y, layout);
  run_until(&mut shell, "the highlight on the first row", |platform| {
    platform
      .snapshot(id)
      .is_some_and(|shot| left_of(&shot, rows[0]) == HIGHLIGHTED)
  });
  settle(&mut shell);
  // Over the third, it slides there over its transition, then stops.
  let before = shell.platform().stats().commits;
  let (x, y) = on_screen(rows[2]);
  pointer.move_to(x, y, layout);
  settle(&mut shell);
  let slid = shell.platform().stats().commits - before;
  assert!(slid >= 3, "it slid over {slid} frames");
  let shot = shell.platform().snapshot(id).expect("the panel draws");
  assert_eq!(left_of(&shot, rows[2]), HIGHLIGHTED, "on the third row");
  assert_eq!(left_of(&shot, rows[0]), PANEL_BACKGROUND, "off the first");
  sleeps(&mut shell);

  // The fifth row is cut off at the list's bottom. Over its strip that
  // shows, it is current, so the list scrolls it whole, as Qt's ListView
  // keeps its current item in view; the pointer is still over it, so
  // nothing scrolls further.
  let (list, _) = control_rows(shell.platform(), id);
  let (_, rows) = control_rows_named(shell.platform(), id);
  let (summary, clipped) = rows[4].clone();
  let bottom = list.y + list.height;
  assert!(
    clipped.y < bottom && clipped.y + clipped.height > bottom,
    "{summary} is cut off at {bottom}: {clipped:?}"
  );
  let strip = Rect {
    height: bottom - clipped.y,
    ..clipped
  };
  let (x, y) = on_screen(strip);
  pointer.move_to(x, y, layout);
  settle(&mut shell);
  let whole = clipped.y + clipped.height - bottom;
  assert_eq!(
    control_offset(shell.platform(), id),
    whole,
    "scrolled whole"
  );
  sleeps(&mut shell);
  assert_eq!(
    control_offset(shell.platform(), id),
    whole,
    "and no further"
  );
  let (_, rows) = control_rows_named(shell.platform(), id);
  let (_, now) = rows
    .into_iter()
    .find(|(name, _)| *name == summary)
    .expect("the row is still built");
  assert_eq!(now.y + now.height, bottom, "{summary} ends at the bottom");
  let shot = shell.platform().snapshot(id).expect("the panel draws");
  assert_eq!(left_of(&shot, now), HIGHLIGHTED, "still highlighted");

  // Five 10-pixel moves 10 ms apart lift at about 1000 pixels a second.
  // Each event carries the time it was sent, so the shell, which runs on
  // this thread, reads them all after the lift: drawing between them
  // would space them by a debug build's frame time.
  let before = shell.platform().stats();
  let resting = shell.platform().snapshot(id).expect("the panel draws");
  for _ in 0..5 {
    pointer.swipe(0.0, 10.0, false);
    std::thread::sleep(Duration::from_millis(10));
  }
  pointer.swipe(0.0, 0.0, true);
  assert_eq!(control_offset(shell.platform(), id), whole, "not read yet");
  settle(&mut shell);
  let after = shell.platform().stats();
  assert!(!crabshell_ui::animating(), "at rest");
  let rested = shell.platform().snapshot(id).expect("the panel draws");
  assert!(rested.rgba != resting.rgba, "the list moved");
  // The fingers moved it 50 pixels; the fling took it on from there.
  let offset = control_offset(shell.platform(), id);
  assert!(offset > whole + 100.0, "it flung on to {offset}");
  let frames = after.commits - before.commits;
  eprintln!(
    "the fling drew {frames} frames at scale 2, in {:.3} ms each",
    (after.drawing - before.drawing).as_secs_f64() * 1000.0 / frames as f64
  );
  sleeps(&mut shell);
}

/// Asserts that one dispatch sleeps a whole 2 s: nothing wakes the loop.
fn sleeps(shell: &mut Shell) {
  let before = shell.platform().stats().iterations;
  let started = Instant::now();
  shell
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  let slept = started.elapsed();
  assert!(slept >= Duration::from_millis(1950), "woke after {slept:?}");
  assert_eq!(shell.platform().stats().iterations, before + 1);
}

/// The control example's output: 3360 by 2100 is a whole number of
/// logical pixels at every scale from 1 to 2 in quarters, and tall
/// enough for the panel at 2.
const CONTROL_MODE: &str = "3360x2100";

/// Its logical size at scale 1, as the virtual pointer takes it.
const CONTROL_LAYOUT: (u32, u32) = (3360, 2100);

/// The middle of the control example's notification list at scale 1, in
/// the output: the list ends at the panel's padding, which the in-process
/// test checks.
fn control_list_middle() -> (u32, u32) {
  let x = CONTROL_LAYOUT.0 - 8 - control::WIDTH / 2;
  let y = 8 + control::HEIGHT - control::PADDING - control::LIST_HEIGHT / 2;
  (x, y)
}

/// The bottom row of the control example's scroll bar thumb in a
/// screenshot at scale 1: the lowest of its color in the bar's column,
/// beside the list.
fn thumb_bottom(shot: &Image) -> u32 {
  let x = CONTROL_LAYOUT.0 - 8 - control::PADDING - 3;
  let end = 8 + control::HEIGHT - control::PADDING;
  (end - control::LIST_HEIGHT..end)
    .filter(|&y| rgb(shot, x, y) == [0x45, 0x47, 0x5a])
    .max()
    .expect("the scroll bar shows")
}

/// Sends the control example ten notifications, every other one without
/// a body, so its rows differ in height and its list scrolls.
fn notify_ten(desktop: &Desktop) {
  for n in 1..=10 {
    let body = if n % 2 == 0 {
      ""
    } else {
      "from the exit check"
    };
    desktop.notify_with(&format!("Message {n}"), body, 0);
  }
}

/// Phase 15's exit check: the control example, on a private audio server
/// and session bus with its date pinned and ten notifications, matches
/// its goldens at all five scales; a virtual pointer drags its slider,
/// and the sink's volume follows until `pactl` agrees; a touchpad fling
/// on its list runs on after the fingers lift and comes to rest; and then
/// the shell wakes nothing for a minute.
#[test]
#[ignore = "needs headless Sway and pipewire"]
fn the_control_center_sets_the_volume_flings_its_list_and_sleeps() {
  let mut desktop = Desktop::new("control");
  desktop
    .env
    .push(("CONTROL_TODAY", CONTROL_TODAY.to_owned()));
  desktop
    .server
    .pactl(&["set-sink-volume", "@DEFAULT_SINK@", "50%"]);
  for scale in ["1", "1.25", "1.5", "1.75", "2"] {
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", CONTROL_MODE, "scale", scale]);
    let shell = desktop.run(&sway, "control");
    shell.wait_until_connected(&desktop.server);
    shell.wait_for(FIRST_FRAME);
    notify_ten(&desktop);
    let factor: f32 = scale.parse().expect("a scale");
    // The volume shows once the server answers; until then, no handle.
    wait_for_shot(&sway, "HEADLESS-1", "the volume", |shot| {
      let x = shot.width as f32 - (8 + control::WIDTH / 2) as f32 * factor;
      (0..(120.0 * factor) as u32).any(|y| rgb(shot, x as u32, y) == HANDLE)
    });
    let shot = steady_shot(&sway, "HEADLESS-1");
    check_golden(&format!("control@{scale}x"), &shot, shot.height);

    // A pointer over the list's first row makes it current, and its
    // highlight shows behind it once it rests.
    let mut pointer = VirtualPointer::new(&sway);
    let layout = (
      (CONTROL_LAYOUT.0 as f32 / factor) as u32,
      (CONTROL_LAYOUT.1 as f32 / factor) as u32,
    );
    let left = layout.0 - 8 - control::WIDTH + control::PADDING;
    let top = 8 + control::HEIGHT - control::PADDING - control::LIST_HEIGHT;
    pointer.move_to(left + control::WIDTH / 2, top + 18, layout);
    wait_for_shot(&sway, "HEADLESS-1", "the highlight", |shot| {
      let at = |n: u32| (n as f32 * factor) as u32;
      rgb(shot, at(left + 3), at(top + 18)) == HIGHLIGHTED[..3]
    });
    let shot = steady_shot(&sway, "HEADLESS-1");
    check_golden(&format!("control-hover@{scale}x"), &shot, shot.height);
  }

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", CONTROL_MODE, "scale", "1"]);
  let mut pointer = VirtualPointer::new(&sway);
  let shell = desktop.run(&sway, "control");
  shell.wait_until_connected(&desktop.server);
  shell.wait_for(FIRST_FRAME);
  notify_ten(&desktop);
  wait_for_shot(&sway, "HEADLESS-1", "the handle", |shot| {
    slider_handle(shot).is_some()
  });
  let (x, y) = slider_handle(&steady_shot(&sway, "HEADLESS-1")).expect("the handle shows");
  // The handle's middle runs from half a handle in to half a handle
  // short of the track's end.
  let track = (CONTROL_LAYOUT.0 - 8 - control::WIDTH + control::PADDING) as f32;
  let at = |percent: f32| {
    let travel = control::TRACK_WIDTH - control::HANDLE;
    (track + control::HANDLE / 2.0 + travel * percent / 100.0).round() as u32
  };
  assert!(x.abs_diff(at(50.0)) <= 1, "the handle is at {x}, not 50%");
  pointer.move_to(x, y, CONTROL_LAYOUT);
  pointer.press(VirtualPointer::LEFT);
  pointer.move_to(at(60.0), y, CONTROL_LAYOUT);
  wait_for_volume(&desktop.server, 60);
  // Off the track, it follows along it.
  pointer.move_to(at(75.0), y + 40, CONTROL_LAYOUT);
  wait_for_volume(&desktop.server, 75);
  pointer.release(VirtualPointer::LEFT);
  steady_shot(&sway, "HEADLESS-1");
  let volume = pactl_volume(&desktop.server);
  assert!(volume.abs_diff(75) <= 1, "pactl says {volume}%");
  eprintln!("pactl says {volume}% after the drag to 75%");
  let (x, _) = slider_handle(&steady_shot(&sway, "HEADLESS-1")).expect("the handle shows");
  assert!(x.abs_diff(at(75.0)) <= 1, "the handle rests at {x}");

  // Five 10-pixel moves 10 ms apart lift at about 1000 pixels a second.
  let (x, y) = control_list_middle();
  pointer.move_to(x, y, CONTROL_LAYOUT);
  let before = steady_shot(&sway, "HEADLESS-1");
  for _ in 0..5 {
    pointer.swipe(0.0, 10.0, false);
    std::thread::sleep(Duration::from_millis(10));
  }
  pointer.swipe(0.0, 0.0, true);
  let rest = steady_shot(&sway, "HEADLESS-1");
  assert!(rest.rgba != before.rgba, "the list scrolled");
  // Ten rows, 5 × 46 + 5 × 36 tall in a list 180 tall, scroll 230
  // pixels. The fingers moved it 50; only the fling after
  // their lift takes it to the end, where it springs back and rests.
  let bottom = thumb_bottom(&rest);
  let end = 8 + control::HEIGHT - control::PADDING;
  assert!(
    bottom + 2 >= end,
    "the scroll bar rests at {bottom}, not {end}"
  );

  // Asleep: no wakeups for a minute.
  shell.wait_until_quiet();
  let wakeups = shell.wakeups_for(Duration::from_secs(60));
  assert!(wakeups.is_empty(), "woke while idle: {wakeups:?}");
}

/// The icon theme in `tests/fixtures/icons` the pinned dashboard draws
/// its symbolic icons from.
const FIXTURE_ICONS: &str = "crabshell-fixtures";

/// A battery at 72% with 2 h 10 min left, as UPower's display device
/// gives one.
fn pinned_battery() -> PowerDevice {
  PowerDevice {
    path: String::from("/org/freedesktop/UPower/devices/DisplayDevice"),
    kind: DeviceKind::Battery,
    icon_name: String::from("battery-level-70-symbolic"),
    present: true,
    state: BatteryState::Discharging,
    percent: 72.0,
    time_to_empty: Some(Duration::from_secs(130 * 60)),
    ..PowerDevice::default()
  }
}

/// A player playing a track whose art is the file at `art`, or the URL
/// `art`.
fn pinned_player(art: &str) -> Player {
  Player {
    id: String::from("org.mpris.MediaPlayer2.test"),
    identity: String::from("Test Player"),
    desktop_entry: String::new(),
    status: PlaybackStatus::Playing,
    title: String::from("Northern Lights"),
    artists: vec![String::from("The Exit Checks")],
    album: String::from("Phase 16"),
    art_url: if art.contains("://") {
      art.to_owned()
    } else {
      format!("file://{art}")
    },
    length: Some(Duration::from_secs(215)),
    can_play: true,
    can_pause: true,
    can_go_next: true,
    can_go_previous: false,
    can_seek: true,
    can_control: true,
    volume: Some(1.0),
    shuffle: None,
    loop_status: None,
    rate: 1.0,
    position: Duration::ZERO,
    position_known_at: Instant::now(),
  }
}

/// A minute of CPU samples that rise and fall, the same each run.
fn pinned_cpu(cx: &Cx) -> ReadSignal<Vec<f32>> {
  let samples = (0..dashboard::HISTORY)
    .map(|at| {
      let at = at as f32;
      0.35 + 0.2 * (at / 6.0).sin() + if at as usize % 17 == 9 { 0.3 } else { 0.0 }
    })
    .collect();
  cx.signal(samples).read_only()
}

/// A PNG `width` by `height` at `path`, its pixel at x, y `color(x, y)`.
fn pattern_png(path: &Path, width: u32, height: u32, color: impl Fn(u32, u32) -> [u8; 3]) {
  let file = std::fs::File::create(path).expect("create the PNG");
  let mut encoder = png::Encoder::new(file, width, height);
  encoder.set_color(png::ColorType::Rgb);
  let mut writer = encoder.write_header().expect("the header");
  let pixels: Vec<u8> = (0..height)
    .flat_map(|y| (0..width).map(move |x| (x, y)))
    .flat_map(|(x, y)| color(x, y))
    .collect();
  writer.write_image_data(&pixels).expect("the pixels");
}

/// A 1920 by 1080 PNG at `path`, from `from` at the top left to `to` at
/// the bottom right.
fn gradient_png(path: &Path, from: [u8; 3], to: [u8; 3]) {
  pattern_png(path, 1920, 1080, |x, y| {
    let t = (x + y) as f32 / (1920 + 1080) as f32;
    let mix =
      |at: usize| (f32::from(from[at]) + (f32::from(to[at]) - f32::from(from[at])) * t) as u8;
    [mix(0), mix(1), mix(2)]
  });
}

/// The pictures the dashboard's tests show, in a temporary directory: a
/// wallpaper of two colors and album art of colored squares.
struct DashboardFiles {
  dir: PathBuf,
}

impl DashboardFiles {
  fn new() -> DashboardFiles {
    let dir = std::env::temp_dir().join(format!("crabshell-dashboard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the pictures' directory");
    small_png(
      &dir.join("wallpaper.png"),
      &[[0x24, 0x27, 0x3a], [0x5b, 0x4a, 0x7a]],
    );
    const SQUARES: [[u8; 3]; 4] = [
      [0xf3, 0x8b, 0xa8],
      [0xf9, 0xe2, 0xaf],
      [0x89, 0xdc, 0xeb],
      [0xcb, 0xa6, 0xf7],
    ];
    pattern_png(&dir.join("art.png"), 64, 64, |x, y| {
      SQUARES[((x / 16 + y / 16) % 4) as usize]
    });
    DashboardFiles { dir }
  }

  fn path(&self, name: &str) -> String {
    self.dir.join(name).display().to_string()
  }

  /// A 1920 by 1080 wallpaper, a picture as large as a 1080p screen, from
  /// blue at the top left to orange at the bottom right.
  fn wallpaper_1080p(&self) -> PathBuf {
    let path = self.dir.join("wallpaper-1080p.png");
    gradient_png(&path, [0x1e, 0x66, 0xf5], [0xfe, 0x64, 0x0b]);
    path
  }

  /// The pinned dashboard's environment: its pictures, and the fixtures'
  /// icon theme, under a name no theme in `~/.icons` or the machine's
  /// data directories takes.
  fn env(&self) -> Vec<(&'static str, String)> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    vec![
      (CHILD, String::from("dashboard")),
      ("WALLPAPER", self.path("wallpaper.png")),
      ("DASHBOARD_ART", self.path("art.png")),
      ("XDG_DATA_DIRS", fixtures.display().to_string()),
    ]
  }
}

impl Drop for DashboardFiles {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

/// The dashboard example's output: 1260 by 840 is a whole number of
/// logical pixels at every scale from 1 to 2 in quarters, and wide
/// enough for the dashboard at 2.
const DASHBOARD_MODE: &str = "1260x840";

/// Part of Phase 16's exit check: the dashboard example, its battery,
/// CPU samples, player and pictures pinned and its icons from the
/// fixtures' theme, matches its goldens at all five scales.
#[test]
#[ignore = "needs headless Sway"]
fn the_dashboard_matches_its_goldens() {
  const TEST: &str = "the_dashboard_matches_its_goldens";
  run_as_child();
  let files = DashboardFiles::new();
  let env = files.env();
  let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
  for scale in ["1", "1.25", "1.5", "1.75", "2"] {
    let sway = Sway::start();
    sway.msg(&[
      "output",
      "HEADLESS-1",
      "mode",
      DASHBOARD_MODE,
      "scale",
      scale,
    ]);
    let shell = ExampleBar::spawn(&sway, this_as_shell(TEST), &env);
    shell.wait_for(FIRST_FRAME);
    // The first frame logged may be the wallpaper's; the dashboard's,
    // with its art blurred in a debug build, can come far later.
    let factor: f32 = scale.parse().expect("a scale");
    wait_for_shot(&sway, "HEADLESS-1", "the album art", |shot| {
      let [r, g, b] = rgb(shot, (70.0 * factor) as u32, (200.0 * factor) as u32);
      r.abs_diff(0xf3) <= 6 && g.abs_diff(0x8b) <= 6 && b.abs_diff(0xa8) <= 6
    });
    wait_for_decoded_pictures(&sway, scale);
    let shot = steady_shot(&sway, "HEADLESS-1");
    check_golden(&format!("dashboard@{scale}x"), &shot, shot.height);
  }
}

/// A config that names no icon theme gets GTK's, and one that names its
/// own keeps it over GTK's.
#[test]
#[ignore = "needs headless Sway"]
fn icon_path_sees_a_new_icon_theme_at_once() {
  const TEST: &str = "icon_path_sees_a_new_icon_theme_at_once";
  run_as_child();
  let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
  let env = [
    (CHILD, "icon path"),
    ("XDG_DATA_DIRS", fixtures.to_str().expect("UTF-8")),
  ];
  let sway = Sway::start();
  let shell = ExampleBar::spawn(&sway, this_as_shell(TEST), &env);
  shell.wait_for("set then found: true");
  shell.wait_for(&format!("bound to {FIXTURE_ICONS}: true"));
  shell.wait_for("bound to hicolor: false");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_config_without_an_icon_theme_gets_gtks_and_its_own_wins_over_it() {
  const TEST: &str = "a_config_without_an_icon_theme_gets_gtks_and_its_own_wins_over_it";
  run_as_child();
  let files = DashboardFiles::new();
  // With the user's fontconfig, as the goldens' fonts need.
  let config = files.dir.join("config");
  std::fs::create_dir_all(&config).expect("a config directory");
  if let Some(user) = user_config() {
    let _ = std::os::unix::fs::symlink(user.join("fontconfig"), config.join("fontconfig"));
  }
  let gtk_names = |theme: &str| {
    std::fs::create_dir_all(config.join("gtk-3.0")).expect("GTK's directory");
    let settings = format!("[Settings]\ngtk-icon-theme-name={theme}\n");
    std::fs::write(config.join("gtk-3.0/settings.ini"), settings).expect("GTK's settings");
  };
  let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
  let red_pixels = |shot: &Image| {
    let rows = shot.rgba[..shot.width as usize * 32 * 4].chunks(4);
    rows.filter(|pixel| pixel[0] > 200 && pixel[1] < 50).count()
  };
  for (theme, drawn) in [("hicolor", false), (FIXTURE_ICONS, true)] {
    gtk_names(theme);
    let env = [
      (CHILD, "themed icon"),
      ("XDG_DATA_DIRS", fixtures.to_str().expect("UTF-8")),
      ("XDG_CONFIG_HOME", config.to_str().expect("UTF-8")),
    ];
    let sway = Sway::start();
    let shell = ExampleBar::spawn(&sway, this_as_shell(TEST), &env);
    shell.wait_for(&format!("the theme has computer-symbolic: {drawn}"));
    shell.wait_for(FIRST_FRAME);
    let shot = steady_shot(&sway, "HEADLESS-1");
    assert_eq!(red_pixels(&shot) > 100, drawn, "GTK names {theme}");
  }

  // The dashboard names its own, which GTK's doesn't replace.
  gtk_names("hicolor");
  let mut env = files.env();
  env.push(("XDG_CONFIG_HOME", config.display().to_string()));
  let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", DASHBOARD_MODE, "scale", "1"]);
  let shell = ExampleBar::spawn(&sway, this_as_shell(TEST), &env);
  shell.wait_for(FIRST_FRAME);
  wait_for_shot(&sway, "HEADLESS-1", "the album art", |shot| {
    let [r, g, b] = rgb(shot, 70, 200);
    r.abs_diff(0xf3) <= 6 && g.abs_diff(0x8b) <= 6 && b.abs_diff(0xa8) <= 6
  });
  wait_for_decoded_pictures(&sway, "1");
  let shot = steady_shot(&sway, "HEADLESS-1");
  check_golden("dashboard@1x", &shot, shot.height);
}

/// Waits for the dashboard's wallpaper and the blurred art behind its
/// player, decoded off the loop, to match its golden at `scale`: past the
/// dashboard, in the output's pixels, and in the player's card.
fn wait_for_decoded_pictures(sway: &Sway, scale: &str) {
  let golden =
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/dashboard@{scale}x.png"));
  let Ok(bytes) = std::fs::read(&golden) else {
    return;
  };
  let golden = Image::decode(&bytes);
  let factor: f32 = scale.parse().expect("a scale");
  for at in [
    (1200, 780),
    ((520.0 * factor) as u32, (280.0 * factor) as u32),
  ] {
    let wanted = rgb(&golden, at.0, at.1);
    wait_for_shot(sway, "HEADLESS-1", "a decoded picture", |shot| {
      let shown = rgb(shot, at.0, at.1);
      shown.iter().zip(wanted).all(|(a, b)| a.abs_diff(b) <= 6)
    });
  }
}

#[test]
fn the_dashboard_reads_the_cpus_use_from_proc_stat() {
  let stat = "cpu  100 5 50 800 45 0 0 0 0 0\ncpu0 50 2 25 400 20 0 0 0 0 0\n";
  assert_eq!(dashboard::cpu_times(stat), Some((155, 1000)));
  // Guest time is already counted in user and nice time.
  let guests = "cpu  100 5 50 800 45 0 0 0 30 2\n";
  assert_eq!(dashboard::cpu_times(guests), Some((155, 1000)));
  assert_eq!(
    dashboard::usage((155, 1000), (205, 1100)),
    Some(0.5),
    "50 of 100 ticks busy"
  );
  assert_eq!(dashboard::usage((155, 1000), (155, 1000)), None, "no time");
  assert_eq!(dashboard::usage((155, 1000), (100, 900)), None, "went back");
  for bad in [
    "",
    "intr 1 2 3",
    "cpu  1 2 x 4",
    "cpu  1 2 3",
    "cpu0 1 2 3 4",
  ] {
    assert_eq!(dashboard::cpu_times(bad), None, "{bad:?}");
  }
  // This machine's parses.
  let real = std::fs::read_to_string("/proc/stat").expect("/proc/stat");
  assert!(dashboard::cpu_times(&real).is_some(), "{real}");
}

/// Whether a dashboard window is shown.
fn dashboard_shown(platform: &mut Platform) -> bool {
  platform
    .windows()
    .any(|(_, window)| window.surface().namespace == "dashboard")
}

/// Part of Phase 16's exit check: the dashboard samples the CPU once a
/// second, each sample a frame, only while it shows. Hidden, its window
/// and its sampling go, its nodes are freed and nothing wakes the loop;
/// shown again, it samples again.
#[test]
#[ignore = "needs headless Sway"]
fn the_dashboard_samples_the_cpu_only_while_it_shows() {
  let sway = Sway::start();
  let runtime = Runtime::current();
  let held: Rc<std::cell::Cell<Option<Signal<bool>>>> = Rc::default();
  let into = Rc::clone(&held);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let shown = cx.signal(false);
    let feeds = dashboard::Feeds {
      battery: cx.signal(None).read_only(),
      players: cx.signal(Vec::new()).read_only(),
      cpu: dashboard::cpu_usage,
    };
    dashboard::dashboard(cx, shown.read_only(), feeds);
    into.set(Some(shown));
  })
  .expect("the root closure runs");
  let shown = held.take().expect("the root closure ran");
  shell
    .dispatch(Some(Duration::from_millis(20)))
    .expect("the connection works");
  runtime.flush();
  let nodes = runtime.node_count();
  assert!(
    crabshell_ui::next_timer().is_none(),
    "hidden, nothing samples"
  );

  shown.set(true);
  run_until(&mut shell, "the dashboard", dashboard_shown);
  let due = crabshell_ui::next_timer().expect("the sampling is armed");
  assert!(due <= Instant::now() + Duration::from_secs(1));
  // Past its first frames: the samples come a second apart.
  settle(&mut shell);
  let before = shell.platform().stats();
  let started = Instant::now();
  let mut wakes = 0;
  while let Some(left) =
    (started + Duration::from_millis(3500)).checked_duration_since(Instant::now())
  {
    shell.dispatch(Some(left)).expect("the connection works");
    wakes += 1;
  }
  let frames = shell.platform().stats().commits - before.commits;
  assert!(
    (3..=4).contains(&frames),
    "{frames} frames in 3.5 s of sampling"
  );
  assert!(wakes <= 4 * 4, "{wakes} wakeups for {frames} samples");

  shown.set(false);
  run_until(&mut shell, "the dashboard to go", |platform| {
    !dashboard_shown(platform)
  });
  runtime.flush();
  assert!(crabshell_ui::next_timer().is_none(), "the sampling stopped");
  assert_eq!(runtime.node_count(), nodes, "everything it made was freed");
  sleeps(&mut shell);

  shown.set(true);
  run_until(&mut shell, "the dashboard again", dashboard_shown);
  assert!(crabshell_ui::next_timer().is_some(), "sampling again");
}

/// This process's resident memory in MiB, from `/proc/self/status`.
fn own_rss_mib() -> f64 {
  let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
  let kib: u64 = status
    .lines()
    .find_map(|line| line.strip_prefix("VmRSS:"))
    .and_then(|rest| rest.trim().strip_suffix("kB")?.trim().parse().ok())
    .expect("a VmRSS line");
  kib as f64 / 1024.0
}

/// Part of Phase 16's exit check, in this process: the dashboard's
/// wallpaper, filling a 1920x1080 output at scale 1, crossfades to a new
/// picture over a frame each refresh, holding both decoded pictures,
/// about 16 MB, only while it runs; then the old one is freed and the
/// loop sleeps. Run it in a release build for the frame time:
/// `env -u SWAYSOCK cargo test --release -p crabshell --test headless a_1080p -- --ignored --nocapture`.
///
/// It runs again as a child with the `GLIBC_TUNABLES` the supervisor
/// starts every shell with, so it checks what `crabshell run` gives a
/// real shell: glibc's mmap threshold pinned at 128 KiB. Left to move, the
/// threshold rises to the size of a large block once one is freed, so the
/// next picture is put on the heap, and a picture freed there leaves the
/// RSS at its high-water mark. Run with `GLIBC_TUNABLES` set to anything
/// else, it checks that process as it is.
#[test]
#[ignore = "needs headless Sway"]
fn a_1080p_wallpaper_crossfade_holds_both_pictures_only_while_it_runs() {
  if std::env::var_os("GLIBC_TUNABLES").is_none() {
    let status =
      this_as_shell("a_1080p_wallpaper_crossfade_holds_both_pictures_only_while_it_runs")
        // crabshell-cli's `supervise::TUNABLE`.
        .env("GLIBC_TUNABLES", "glibc.malloc.mmap_threshold=131072")
        .status()
        .expect("the test runs again");
    assert!(status.success(), "the crossfade check failed: {status}");
    return;
  }
  let files = DashboardFiles::new();
  let (first, second) = (files.dir.join("first.png"), files.dir.join("second.png"));
  gradient_png(&first, [0x1e, 0x66, 0xf5], [0xfe, 0x64, 0x0b]);
  gradient_png(&second, [0xa6, 0xe3, 0xa1], [0x31, 0x32, 0x44]);
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let held: Rc<std::cell::Cell<Option<Signal<Option<PathBuf>>>>> = Rc::default();
  let into = Rc::clone(&held);
  let start = first.clone();
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let wallpaper = cx.signal(Some(start.clone()));
    let outputs = cx.outputs();
    cx.per_output(move |cx, output| {
      dashboard::wallpaper_window(cx, output, outputs, wallpaper.read_only())
    });
    into.set(Some(wallpaper));
  })
  .expect("the root closure runs");
  let wallpaper = held.take().expect("the root closure ran");
  run_until(&mut shell, "the wallpaper", |platform| {
    platform
      .windows()
      .any(|(_, window)| window.surface().namespace == "wallpaper")
  });
  settle(&mut shell);
  let resting = own_rss_mib();

  wallpaper.set(Some(second));
  let before = shell.platform().stats();
  // The first frame decodes the new picture; the crossfade's own frames
  // come after it.
  run_until(&mut shell, "the new picture's first frame", |platform| {
    platform.stats().commits > before.commits
  });
  assert!(crabshell_ui::animating(), "the crossfade runs");
  let fading = shell.platform().stats();
  let mut both = own_rss_mib();
  let deadline = Instant::now() + TIMEOUT;
  while crabshell_ui::animating() {
    assert!(Instant::now() < deadline, "the crossfade never ended");
    shell
      .dispatch(Some(Duration::from_millis(20)))
      .expect("the connection works");
    both = both.max(own_rss_mib());
  }
  let after = shell.platform().stats();
  settle(&mut shell);
  let ended = own_rss_mib();
  let frames = after.commits - fading.commits;
  let ms = (after.drawing - fading.drawing).as_secs_f64() * 1000.0 / frames as f64;
  eprintln!(
    "a 1920x1080 crossfade at scale 1 drew {frames} frames in {ms:.2} ms each; RSS {resting:.1} MiB \
     resting, {both:.1} MiB at most while both pictures were held, {ended:.1} MiB once it ended"
  );
  // About 48 at 60 Hz in a release build; a debug build draws a few.
  assert!(frames >= 3, "{frames} frames for the crossfade");
  // A 1920x1080 picture decoded is 7.9 MiB.
  assert!(both >= resting + 6.0, "both pictures held: {both:.1} MiB");
  assert!(ended <= both - 6.0, "the old picture freed: {ended:.1} MiB");
  sleeps(&mut shell);
}

/// A shell started again by `run` counts its first frame from its first
/// start, which the first process leaves in `CRABSHELL_STARTED`, so the
/// logged time includes the restart. Here the variable says 2 s ago and
/// the tunable is set already, so nothing starts again.
#[test]
#[ignore = "needs headless Sway"]
fn a_restarted_shell_counts_its_first_frame_from_its_first_start() {
  let sway = Sway::start();
  let ago = std::time::SystemTime::now() - Duration::from_secs(2);
  let nanos = ago
    .duration_since(std::time::UNIX_EPOCH)
    .expect("after 1970")
    .as_nanos()
    .to_string();
  let counter = ExampleBar::start(
    &sway,
    "counter",
    false,
    &[
      ("CRABSHELL_STARTED", &nanos),
      ("GLIBC_TUNABLES", "glibc.malloc.mmap_threshold=131072"),
    ],
  );
  let ms = counter.first_frame_ms();
  assert!(
    (2000.0..4000.0).contains(&ms),
    "first frame {ms:.1} ms after the first start"
  );
}

/// A shell started without the supervisor, as the dashboard example
/// from a terminal, starts itself again with the `GLIBC_TUNABLES` the
/// supervisor gives a shell, so a wallpaper it replaces is given back:
/// before, the RSS kept the high-water mark of both pictures, 52 MiB
/// against 30.
#[test]
#[ignore = "needs headless Sway"]
fn a_shell_run_alone_starts_again_with_the_mmap_threshold_and_frees_a_replaced_wallpaper() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let files = DashboardFiles::new();
  let mut desktop = Desktop::new("tuned");
  let first = files.wallpaper_1080p().display().to_string();
  desktop.env.push(("WALLPAPER", first));
  let dashboard = desktop.run(&sway, "dashboard");
  dashboard.wait_for(FIRST_FRAME);
  let environ = std::fs::read(format!("/proc/{}/environ", dashboard.child.id()))
    .expect("the shell's environment");
  let tuned = environ
    .split(|&byte| byte == 0)
    .any(|entry| entry == b"GLIBC_TUNABLES=glibc.malloc.mmap_threshold=131072");
  assert!(
    tuned,
    "the shell started again with the supervisor's tunable"
  );
  let first_start = environ
    .split(|&byte| byte == 0)
    .any(|entry| entry.starts_with(b"CRABSHELL_STARTED="));
  assert!(first_start, "the first start's time carried over");
  ipc_call(&sway, "dashboard", "close");
  dashboard.wait_until_quiet();
  let resting = dashboard.rss_kib() as f64 / 1024.0;

  let second = files.dir.join("second.png");
  gradient_png(&second, [0xa6, 0xe3, 0xa1], [0x31, 0x32, 0x44]);
  ipc_call_with(&sway, "wallpaper", "set", &[&second.display().to_string()]);
  // The crossfade runs 800 ms after the new picture is decoded.
  std::thread::sleep(Duration::from_secs(2));
  dashboard.wait_until_quiet();
  let after = dashboard.rss_kib() as f64 / 1024.0;
  eprintln!("a 1080p wallpaper replaced: RSS {resting:.1} MiB before, {after:.1} MiB after");
  // A 1920x1080 picture decoded is 7.9 MiB.
  assert!(
    after < resting + 4.0,
    "the old picture given back: {after:.1} MiB"
  );
}

thread_local! {
  /// How many times [`counted_cpu`] started the CPU's samples.
  static CPU_STARTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// [`pinned_cpu`], counting each start in [`CPU_STARTS`].
fn counted_cpu(cx: &Cx) -> ReadSignal<Vec<f32>> {
  CPU_STARTS.with(|starts| starts.set(starts.get() + 1));
  pinned_cpu(cx)
}

/// `dashboard open` on a dashboard already open sets `shown` to the value
/// it has: nothing is built again, so its windows stay and its CPU
/// history goes on.
#[test]
#[ignore = "needs headless Sway"]
fn opening_an_open_dashboard_keeps_its_windows_and_cpu_history() {
  let sway = Sway::start();
  let held: Rc<std::cell::Cell<Option<Signal<bool>>>> = Rc::default();
  let into = Rc::clone(&held);
  let mut shell = Shell::new(sway.connect(), move |cx| {
    let shown = cx.signal(true);
    let feeds = dashboard::Feeds {
      battery: cx.signal(None).read_only(),
      players: cx.signal(Vec::new()).read_only(),
      cpu: counted_cpu,
    };
    dashboard::dashboard(cx, shown.read_only(), feeds);
    into.set(Some(shown));
  })
  .expect("the root closure runs");
  let shown = held.take().expect("the root closure ran");
  run_until(&mut shell, "the dashboard", dashboard_shown);
  settle(&mut shell);
  let window = window_named(shell.platform(), "dashboard");
  assert_eq!(CPU_STARTS.with(std::cell::Cell::get), 1);

  shown.set(true);
  settle(&mut shell);
  assert_eq!(
    window_named(shell.platform(), "dashboard"),
    window,
    "the same window"
  );
  assert_eq!(
    CPU_STARTS.with(std::cell::Cell::get),
    1,
    "the same CPU history"
  );
}

#[test]
fn the_dashboards_wallpaper_takes_absolute_paths_only() {
  let args = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
  assert_eq!(
    dashboard::wallpaper_path(&args(&["/home/me/hills.jpg"])),
    Ok(PathBuf::from("/home/me/hills.jpg"))
  );
  let refused = Err(String::from("set takes an absolute path"));
  for bad in [&[][..], &[""], &["hills.jpg"], &["./hills.jpg"]] {
    assert_eq!(dashboard::wallpaper_path(&args(bad)), refused, "{bad:?}");
  }
}

/// When the notes example's clock is stopped in its tests, as its
/// `NOTES_NOW` takes it.
const NOTES_NOW: &str = "2026-11-17T11:11:11";

/// The notifications the notes example's tests send, oldest first, as
/// summary and markup body: one too long for a card's three lines, one
/// with the other tags, and one with a link.
const NOTES_SENT: [(&str, &str); 3] = [
  (
    "Disk almost full",
    "Only <b>2 GB</b> left on <i>/home</i>. This body runs on well past the \
     three lines a card shows, so the last line it shows ends in an ellipsis, \
     and the rest of it waits for the app that sent it.",
  ),
  (
    "Meeting moved",
    "Design review is now at <i>3 pm</i> in room 4. <u>Bring the notes.</u>",
  ),
  (
    "Build finished",
    "<b>crabshell</b> passed 1,204 tests &amp; failed none. See \
     <a href=\"https://example.org/log\">the log</a>.",
  ),
];

/// Where the link in [`NOTES_SENT`] goes.
const NOTES_LINK: &str = "https://example.org/log";

/// `at` as the notes example's clock shows it.
fn notes_time(at: &str) -> crabshell::jiff::Zoned {
  let at: crabshell::jiff::civil::DateTime = at.parse().expect("a date and time");
  at.to_zoned(crabshell::jiff::tz::TimeZone::UTC)
    .expect("in range")
}

/// [`NOTES_SENT`] as the server keeps them with body markup on, ids from 1.
fn notes_notifications() -> Vec<Notification> {
  NOTES_SENT
    .iter()
    .zip(1..)
    .map(|((summary, body), id)| Notification {
      app_name: String::from("exit-check"),
      body: (*body).to_owned(),
      ..notification(id, summary)
    })
    .collect()
}

/// Sends [`NOTES_SENT`] on the bus at `address`, once something serves
/// notifications there.
fn notify_notes(address: &str) {
  let deadline = Instant::now() + TIMEOUT;
  let served = || {
    let output = Command::new("gdbus")
      .args(["call", "--address", address])
      .args(["--dest", "org.freedesktop.DBus"])
      .args(["--object-path", "/org/freedesktop/DBus"])
      .args(["--method", "org.freedesktop.DBus.NameHasOwner"])
      .arg("org.freedesktop.Notifications")
      .output()
      .expect("gdbus runs");
    String::from_utf8_lossy(&output.stdout).contains("true")
  };
  while !served() {
    assert!(Instant::now() < deadline, "nothing serves notifications");
    std::thread::sleep(Duration::from_millis(50));
  }
  for (summary, body) in NOTES_SENT {
    notify_on(address, summary, body, 0);
  }
}

/// Where the notes panel is in an output's logical pixels: 8 in from the
/// top right of an output `layout` wide.
fn notes_origin(layout: (u32, u32)) -> (f32, f32) {
  ((layout.0 - 8 - notes::WIDTH) as f32, 8.0)
}

/// The notes panel's link in [`NOTES_SENT`]: the middle of its glyphs in
/// the window, and its text's widget path.
fn notes_link(platform: &Platform) -> ((f32, f32), String) {
  let id = window_named(platform, "notes");
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the panel is held");
  let body = parse_markup(NOTES_SENT[2].1);
  // A glyph's metadata is its span's index plus one.
  let link = 1
    + body
      .iter()
      .position(|span| span.link.is_some())
      .expect("a link");
  fn find(node: NodeView<'_>, link: usize) -> Option<((f32, f32), String)> {
    if let Content::Text(text) = node.content() {
      for run in text.runs() {
        let glyphs: Vec<_> = run.glyphs.iter().filter(|g| g.metadata == link).collect();
        if let (Some(first), Some(last)) = (glyphs.first(), glyphs.last())
          && run.text.contains("the log")
        {
          let origin = text.origin();
          let x = origin.x + (first.x + last.x + last.w) / 2.0;
          let y = origin.y + run.line_top + run.line_height / 2.0;
          return Some(((x, y), node.path().to_owned()));
        }
      }
    }
    node.children().find_map(|child| find(child, link))
  }
  find(window.view().root(), link).expect("the link is laid out")
}

/// The middle of the notes panel's note area, in the window.
fn notes_area(platform: &Platform) -> (f32, f32) {
  let id = window_named(platform, "notes");
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the panel is held");
  fn find(node: NodeView<'_>) -> Option<Rect> {
    match node.content() {
      Content::Input(_) => Some(node.rect()),
      _ => node.children().find_map(find),
    }
  }
  let area = find(window.view().root()).expect("the note area");
  (area.x + area.width / 2.0, area.y + area.height / 2.0)
}

/// The notes panel for [`notes_notifications`] at `scale`, built in this
/// process with plain signals, as the example lays it out: where its
/// link is on an output `layout` big, and its path. Nothing is clicked,
/// so the desktop's session bus is never asked for notifications.
fn notes_link_on_screen(sway: &Sway, layout: (u32, u32)) -> ((u32, u32), String) {
  let mut shell = Shell::new(sway.connect(), |cx| {
    let feeds = notes::Feeds {
      now: cx.signal(notes_time(NOTES_NOW)).read_only(),
      notifications: cx.signal(notes_notifications()).read_only(),
      open: |_, _| {},
    };
    cx.per_output(move |cx, output| notes::panel(cx, output, feeds));
  })
  .expect("the root closure runs");
  run_until(&mut shell, "the panel's first frame", |platform| {
    shown_on(platform, "HEADLESS-1").is_some()
  });
  settle(&mut shell);
  let ((x, y), path) = notes_link(shell.platform());
  let (left, top) = notes_origin(layout);
  (((left + x) as u32, (top + y) as u32), path)
}

/// The notes example's clock in proportional digits, at its size.
const PROPORTIONAL_CLOCK: Style = Style::new().font(
  Font::new("sans-serif", 36.0)
    .weight(300)
    .features(&PROPORTIONAL_DIGITS),
);
const PROPORTIONAL_DIGITS: [FontFeature; 1] = [FontFeature::new(*b"pnum", 1)];

/// Part of Phase 17's exit check, in this process with plain signals for
/// the services: the notes example's clock, in tabular digits, is as wide
/// at 00:00:00 as at 11:11:11, as its `geometry` says, while one in
/// proportional digits beside it changes width; its note takes
/// typed keys and an input method's text into its persisted signal; and
/// then the loop sleeps.
#[test]
#[ignore = "needs headless Sway"]
fn the_notes_clock_holds_its_width_and_its_note_takes_keys_and_input_method_text() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", CONTROL_MODE, "scale", "1"]);
  let mut pointer = VirtualPointer::new(&sway);
  let mut keyboard = VirtualKeyboard::new(&sway);
  let mut method = InputMethod::new(&sway);
  let knobs = Cell::new(None);
  let mut shell = Shell::new(sway.connect(), |cx| {
    let now = cx.signal(notes_time(NOTES_NOW));
    let clock = cx.signal(Rect::default());
    let proportional = cx.signal(Rect::default());
    let note = cx.persist(notes::NOTE_KEY, String::new());
    knobs.set(Some((now, clock, proportional, note)));
    let feeds = notes::Feeds {
      now: now.read_only(),
      notifications: cx.signal(notes_notifications()).read_only(),
      open: |_, _| {},
    };
    cx.per_output(move |cx, output| {
      let probe = PanelWindow::new(output)
        .anchors(Edge::Top | Edge::Left)
        .width(400)
        .height(60)
        .namespace("clock")
        .child(
          Row::new()
            .gap(8)
            .child(notes::clock(now.read_only()).geometry(clock))
            .child(
              notes::clock(now.read_only())
                .style(PROPORTIONAL_CLOCK)
                .geometry(proportional),
            ),
        );
      vec![notes::panel(cx, output, feeds), probe]
    });
  })
  .expect("the root closure runs");
  let (now, clock, proportional, note) = knobs.get().expect("the root closure ran");
  run_until(&mut shell, "both windows' first frames", |platform| {
    let shown = platform.windows().filter(|(id, _)| {
      platform
        .window_info(*id)
        .is_some_and(|info| info.commits > 0)
    });
    shown.count() == 2
  });
  settle(&mut shell);
  let clock_text = |platform: &Platform| {
    let id = window_named(platform, "clock");
    in_row(platform, id, 0, |text| {
      let Content::Text(text) = text.content() else {
        panic!("the clock is text");
      };
      text.runs().map(|run| run.text).collect::<String>()
    })
  };
  assert_eq!(clock_text(shell.platform()), "11:11:11");
  let ones = (clock.get_untracked(), proportional.get_untracked());
  assert!(ones.0.width > 0.0, "laid out: {ones:?}");
  now.set(notes_time("2026-11-17T00:00:00"));
  settle(&mut shell);
  assert_eq!(clock_text(shell.platform()), "00:00:00");
  let zeros = (clock.get_untracked(), proportional.get_untracked());
  // Noto Sans, the goldens' sans-serif, has tabular figures anyway, so the
  // example's feature matters for fonts like Inter. The proportional clock
  // shows this probe sees digits change width.
  assert_ne!(ones.1.width, zeros.1.width, "proportional digits");
  assert_eq!(ones.0.width, zeros.0.width, "11:11:11, then 00:00:00");

  // A click in the note gives the panel the keyboard and the area the focus.
  let (x, y) = notes_area(shell.platform());
  let (left, top) = notes_origin(CONTROL_LAYOUT);
  pointer.move_to((left + x) as u32, (top + y) as u32, CONTROL_LAYOUT);
  pointer.click();
  run_until(&mut shell, "the input method to activate", |_| {
    method.poll();
    method.active()
  });
  // Tab types a tab in the note rather than moving the focus.
  for key in [keys::H, keys::ENTER, keys::I, keys::TAB] {
    keyboard.tap(key);
  }
  run_until(&mut shell, "two typed lines and a tab", |_| {
    note.get_untracked() == "h\ni\t"
  });
  // Sway ignores a commit whose serial isn't the latest `done`, so wait
  // until the input method has heard the typed text.
  run_until(&mut shell, "the input method to hear it", |_| {
    method.poll();
    method.surrounding() == "h\ni\t"
  });
  method.commit("你");
  run_until(&mut shell, "the committed text", |_| {
    note.get_untracked() == "h\ni\t你"
  });
  run_until(&mut shell, "the surrounding text", |_| {
    method.poll();
    method.surrounding() == "h\ni\t你"
  });
  // One event comes after the input method's last change, and then the
  // loop sleeps.
  settle(&mut shell);
  shell
    .dispatch(Some(Duration::from_secs(1)))
    .expect("the connection works");
  sleeps(&mut shell);
}

/// A fake `xdg-open` in its own directory, which writes the URL it is
/// given to `opened` beside it, for a `PATH` that finds it first.
struct FakeOpener {
  dir: PathBuf,
}

impl FakeOpener {
  fn new() -> FakeOpener {
    use std::os::unix::fs::PermissionsExt;
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("opener-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the opener's directory");
    let script = dir.join("xdg-open");
    let body = "#!/bin/sh\nprintf '%s' \"$1\" > \"$(dirname \"$0\")/opened.part\"\n\
                mv \"$(dirname \"$0\")/opened.part\" \"$(dirname \"$0\")/opened\"\n";
    std::fs::write(&script, body).expect("write the opener");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
      .expect("make it runnable");
    FakeOpener { dir }
  }

  /// This directory, then this process's `PATH`.
  fn path(&self) -> String {
    let rest = std::env::var("PATH").unwrap_or_default();
    format!("{}:{rest}", self.dir.display())
  }

  /// The URL it was run with, once it has been.
  fn opened(&self) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
      if let Ok(url) = std::fs::read_to_string(self.dir.join("opened")) {
        return url;
      }
      assert!(Instant::now() < deadline, "xdg-open never ran");
      std::thread::sleep(Duration::from_millis(50));
    }
  }
}

impl Drop for FakeOpener {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

/// Phase 17's exit check: the notes example, on a private session bus
/// with its clock stopped at 11:11:11 and three notifications sent with
/// markup, matches its goldens at all five scales; a click on the link in
/// a body runs `xdg-open` with its URL, a fake one first on a private
/// `PATH`; and then the shell wakes nothing for a minute.
#[test]
#[ignore = "needs headless Sway"]
fn the_notes_example_shows_markup_opens_a_link_and_sleeps() {
  let bus = PrivateBus::start();
  let opener = FakeOpener::new();
  let path = opener.path();
  let env = [
    ("DBUS_SESSION_BUS_ADDRESS", bus.address.as_str()),
    ("NOTES_NOW", NOTES_NOW),
    ("PATH", path.as_str()),
  ];
  for scale in ["1", "1.25", "1.5", "1.75", "2"] {
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", CONTROL_MODE, "scale", scale]);
    let shell = ExampleBar::start(&sway, "notes", true, &env);
    shell.wait_for(FIRST_FRAME);
    notify_notes(&bus.address);
    let shot = steady_shot(&sway, "HEADLESS-1");
    check_golden(&format!("notes@{scale}x"), &shot, shot.height);
  }

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", CONTROL_MODE, "scale", "1"]);
  let ((x, y), _) = notes_link_on_screen(&sway, CONTROL_LAYOUT);
  let mut pointer = VirtualPointer::new(&sway);
  let shell = ExampleBar::start(&sway, "notes", true, &env);
  shell.wait_for(FIRST_FRAME);
  notify_notes(&bus.address);
  steady_shot(&sway, "HEADLESS-1");
  pointer.move_to(x, y, CONTROL_LAYOUT);
  pointer.click();
  assert_eq!(opener.opened(), NOTES_LINK);

  // Asleep: no wakeups for a minute.
  shell.wait_until_quiet();
  let wakeups = shell.wakeups_for(Duration::from_secs(60));
  assert!(wakeups.is_empty(), "woke while idle: {wakeups:?}");
}

/// Part of Phase 17's exit check: a link handler in the notes panel that
/// panics is logged with its widget path, and the shell runs on, its
/// server still taking notifications.
#[test]
#[ignore = "needs headless Sway"]
fn a_panicking_link_in_the_notes_panel_is_logged_with_its_path_and_the_shell_runs_on() {
  run_as_child();
  let bus = PrivateBus::start();
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", CONTROL_MODE, "scale", "1"]);
  let ((x, y), path) = notes_link_on_screen(&sway, CONTROL_LAYOUT);
  let mut pointer = VirtualPointer::new(&sway);
  let mut shell = ExampleBar::spawn(
    &sway,
    this_as_shell(
      "a_panicking_link_in_the_notes_panel_is_logged_with_its_path_and_the_shell_runs_on",
    ),
    &[
      (CHILD, "notes"),
      ("DBUS_SESSION_BUS_ADDRESS", &bus.address),
      ("NOTES_NOW", NOTES_NOW),
    ],
  );
  shell.wait_for(FIRST_FRAME);
  notify_notes(&bus.address);
  steady_shot(&sway, "HEADLESS-1");
  pointer.move_to(x, y, CONTROL_LAYOUT);
  pointer.click();
  let line = shell.wait_for("on_link panicked");
  // A keyed child's index in a path is its place when it was built, and
  // here the cards came one at a time, the link's last.
  let (card, text) = path
    .split_once("/Button[0]/")
    .expect("the link is in the first card");
  let expected = format!("handler {card}/Button[2]/{text}.on_link panicked and is disabled");
  assert!(line.contains(&expected), "{line:?} names {expected:?}");
  notify_on(&bus.address, "After", "the panic", 0);
  assert!(
    shell
      .child
      .try_wait()
      .expect("the shell's status")
      .is_none(),
    "the shell runs on"
  );
}

/// The system example's output: 1680 by 1050 is a whole number of logical
/// pixels at every scale from 1 to 2 in quarters, and wide enough for the
/// bar at 2.
const SYSTEM_MODE: &str = "1680x1050";

/// The time the system example's tests pin: Wednesday 18 November 2026,
/// 09:41.
const SYSTEM_NOW: &str = "2026-11-18T09:41:00";

/// Open-Meteo's reply for 7.6°C and partly cloudy, code 2, whose icon
/// the fixtures' theme lacks, so the bar draws its fallback.
const WEATHER: &str = r#"{"latitude":52.52,"longitude":13.419998,"current_units":{"temperature_2m":"°C","weather_code":"wmo code"},"current":{"time":"2026-11-18T09:30","interval":900,"temperature_2m":7.6,"weather_code":2}}"#;

/// How much of the screen, in logical pixels from the top, the
/// clipboard's goldens hold.
const POPUP_ROWS: u32 = 260;

/// The same for the wallpaper picker, two rows of pictures tall.
const WALLPAPER_ROWS: u32 = 320;

fn pinned_backlight() -> Backlight {
  Backlight {
    name: String::from("crabshell_backlight"),
    brightness: 60,
    max_brightness: 100,
  }
}

/// 23% CPU, 40% of 16 GiB in use, and a CPU at 52.4°C listed after
/// another sensor.
fn pinned_stats() -> SystemStats {
  let gib = 1 << 30;
  SystemStats {
    cpu: 0.23,
    cpus: vec![0.2, 0.26],
    memory_total: 16 * gib,
    memory_available: 16 * gib * 3 / 5,
    swap_total: 0,
    swap_free: 0,
    temperatures: vec![
      Temperature {
        name: String::from("acpitz temp1"),
        celsius: 40.0,
      },
      Temperature {
        name: String::from("k10temp Tctl"),
        celsius: 52.4,
      },
    ],
  }
}

/// The system example with its clock and stats pinned and `backlights`,
/// its icons from the fixtures' theme, logging its clipboard history.
fn run_system(cx: &Cx, backlights: ReadSignal<Vec<Backlight>>) {
  let now: crabshell::jiff::civil::DateTime = SYSTEM_NOW.parse().expect("a time");
  let now = now
    .to_zoned(crabshell::jiff::tz::TimeZone::UTC)
    .expect("a time in UTC");
  let feeds = system::Feeds {
    now: cx.signal(now).read_only(),
    stats: cx.signal(pinned_stats()).read_only(),
    backlights,
  };
  system::shell(cx, feeds);
  cx.set_icon_theme(FIXTURE_ICONS);
  let history = cx.clipboard().history();
  cx.effect(move || {
    let top = history.with(|all| match all.first().map(|entry| &entry.content) {
      Some(ClipboardContent::Text(text)) => text.clone(),
      Some(ClipboardContent::Image(_)) => String::from("<image>"),
      None => String::new(),
    });
    eprintln!(
      "clipboard: {} entries, {top} on top",
      history.with(Vec::len)
    );
  });
}

/// A plain HTTP server on a free local port, on a thread of its own,
/// answering each GET with the page for its path, or 404.
struct LocalServer {
  port: u16,
  served: Arc<Mutex<Vec<String>>>,
}

impl LocalServer {
  fn start(pages: Vec<(&'static str, Vec<u8>)>) -> LocalServer {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
    let port = listener.local_addr().expect("its address").port();
    let served = Arc::<Mutex<Vec<String>>>::default();
    let log = Arc::clone(&served);
    std::thread::spawn(move || {
      for stream in listener.incoming() {
        let Ok(mut stream) = stream else { return };
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|end| end == b"\r\n\r\n") {
          match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
          }
        }
        let request = String::from_utf8_lossy(&request);
        let path = request.split_whitespace().nth(1).unwrap_or_default();
        let page = pages.iter().find(|(name, _)| *name == path);
        let (status, body) = match page {
          Some((_, body)) => ("200 OK", body.as_slice()),
          None => ("404 Not Found", &[][..]),
        };
        let head = format!(
          "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
          body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
        log.lock().expect("the log").push(path.to_owned());
      }
    });
    LocalServer { port, served }
  }

  fn url(&self, path: &str) -> String {
    format!("http://127.0.0.1:{}{path}", self.port)
  }

  /// Waits until `path` was served.
  fn wait_for(&self, path: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while !self.served().iter().any(|served| served == path) {
      assert!(Instant::now() < deadline, "{path} was never asked for");
      std::thread::sleep(Duration::from_millis(20));
    }
  }

  fn served(&self) -> Vec<String> {
    self.served.lock().expect("the log").clone()
  }
}

/// A stand-in for niri: its socket in the private Sway's runtime
/// directory, answered on a thread of its own. niri itself is never
/// started.
struct FakeNiri {
  path: PathBuf,
  actions: Arc<Mutex<Vec<String>>>,
  events: Arc<Mutex<Option<std::os::unix::net::UnixStream>>>,
}

impl FakeNiri {
  /// HEADLESS-1 shows workspace 1, which has the focus, and holds `web`;
  /// DP-9, which the private Sway lacks, shows 3.
  const STATE: &str = concat!(
    r#"{"WorkspacesChanged":{"workspaces":["#,
    r#"{"id":1,"idx":1,"name":null,"output":"HEADLESS-1","is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":null},"#,
    r#"{"id":2,"idx":2,"name":"web","output":"HEADLESS-1","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":null},"#,
    r#"{"id":3,"idx":1,"name":null,"output":"DP-9","is_urgent":false,"is_active":true,"is_focused":false,"active_window_id":null}]}}"#,
    "\n",
    r#"{"WindowsChanged":{"windows":[]}}"#,
    "\n",
    r#"{"KeyboardLayoutsChanged":{"keyboard_layouts":{"names":["English (US)"],"current_idx":0}}}"#,
    "\n",
    r#"{"OverviewOpenedOrClosed":{"is_open":false}}"#,
    "\n",
    r#"{"ConfigLoaded":{"failed":false}}"#,
    "\n",
  );

  fn start(sway: &Sway) -> FakeNiri {
    use std::io::Write;
    let path = sway.runtime_dir().join("niri.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("niri's socket");
    let fake = FakeNiri {
      path,
      actions: Arc::default(),
      events: Arc::default(),
    };
    let (actions, events) = (Arc::clone(&fake.actions), Arc::clone(&fake.events));
    std::thread::spawn(move || {
      for stream in listener.incoming() {
        let Ok(mut stream) = stream else { return };
        let mut request = String::new();
        if BufReader::new(&stream).read_line(&mut request).is_err() {
          continue;
        }
        let handled = "{\"Ok\":\"Handled\"}\n";
        if request == "\"EventStream\"\n" {
          let _ = stream.write_all(format!("{handled}{}", Self::STATE).as_bytes());
          *events.lock().expect("the event stream") = Some(stream);
        } else {
          let _ = stream.write_all(handled.as_bytes());
          let action = request.trim_end().to_owned();
          actions.lock().expect("the actions").push(action);
        }
      }
    });
    fake
  }

  /// Sends `event` on the event stream, once the shell opened it.
  fn send(&self, event: &str) {
    use std::io::Write;
    let deadline = Instant::now() + TIMEOUT;
    loop {
      if let Some(stream) = self.events.lock().expect("the event stream").as_mut() {
        stream
          .write_all(format!("{event}\n").as_bytes())
          .expect("an event sent");
        return;
      }
      assert!(Instant::now() < deadline, "the shell never followed niri");
      std::thread::sleep(Duration::from_millis(20));
    }
  }

  /// Waits until the shell sent `action`.
  fn wait_for_action(&self, action: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
      let actions = self.actions.lock().expect("the actions").clone();
      if actions.iter().any(|sent| sent == action) {
        return;
      }
      assert!(Instant::now() < deadline, "no {action} in {actions:?}");
      std::thread::sleep(Duration::from_millis(20));
    }
  }
}

/// The wallpaper picker's folder, in the private Sway's runtime
/// directory: three pictures, a text file and a folder, which it leaves
/// out.
fn wallpaper_folder(sway: &Sway) -> PathBuf {
  let folder = sway.runtime_dir().join("wallpapers");
  std::fs::create_dir_all(folder.join("old")).expect("the folder");
  gradient_png(
    &folder.join("dunes.png"),
    [0xfa, 0xb3, 0x87],
    [0x8c, 0x4a, 0x2f],
  );
  pattern_png(&folder.join("lake.png"), 160, 90, |x, y| {
    if y > 50 + x / 20 {
      [0x1e, 0x66, 0xf5]
    } else {
      [0x89, 0xdc, 0xeb]
    }
  });
  pattern_png(&folder.join("Night Sky.PNG"), 64, 36, |x, y| {
    if (x * 7 + y * 13) % 29 == 0 {
      [0xf9, 0xe2, 0xaf]
    } else {
      [0x11, 0x11, 0x1b]
    }
  });
  std::fs::write(folder.join("notes.txt"), "not a picture").expect("a text file");
  folder
}

/// The environment the system example's child runs in, as `mode`.
fn system_env(
  niri: &FakeNiri,
  server: &LocalServer,
  folder: &Path,
  mode: &str,
) -> Vec<(&'static str, String)> {
  let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
  vec![
    (CHILD, mode.to_owned()),
    ("NIRI_SOCKET", niri.path.display().to_string()),
    ("WEATHER_URL", server.url("/weather")),
    ("WALLPAPERS", folder.display().to_string()),
    ("XDG_DATA_DIRS", fixtures.display().to_string()),
  ]
}

/// The system example, run as this test binary for `test`, on one test
/// thread, so libtest never wakes it to say the test runs long.
fn start_system(sway: &Sway, test: &str, env: &[(&'static str, String)]) -> ExampleBar {
  let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
  let mut command = this_as_shell(test);
  command.arg("--test-threads=1");
  ExampleBar::spawn(sway, command, &env)
}

/// Copies two texts and a picture with `wl-copy`, each once the shell
/// has the one before in its history. Returns the copies, which serve
/// their selection until killed.
fn fill_clipboard(sway: &Sway, shell: &ExampleBar, picture: &Path) -> Vec<Child> {
  let copy = |args: &[&str], stdin: Stdio| {
    sway
      .client("wl-copy")
      .arg("--foreground")
      .args(args)
      .stdin(stdin)
      .spawn()
      .expect("wl-copy runs")
  };
  let first = copy(
    &["cargo run --release -p crabshell --example system"],
    Stdio::null(),
  );
  shell.wait_for("clipboard: 1 entries");
  let file = std::fs::File::open(picture).expect("the picture");
  let second = copy(&["--type", "image/png"], Stdio::from(file));
  shell.wait_for("clipboard: 2 entries, <image> on top");
  let line =
    "A line long enough that the clipboard's popup cuts it off at its edge\nand a second line";
  let third = copy(&[line], Stdio::null());
  shell.wait_for("clipboard: 3 entries");
  vec![first, second, third]
}

/// Whether `wl-copy` runs here.
fn has_wl_copy() -> bool {
  Command::new("wl-copy")
    .arg("--version")
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .is_ok_and(|status| status.success())
}

/// The top `rows` of `shot`.
fn top_rows(shot: &Image, rows: u32) -> Image {
  Image {
    rgba: shot.rgba[..shot.width as usize * rows as usize * 4].to_vec(),
    width: shot.width,
    height: rows,
  }
}

/// A screenshot of HEADLESS-1 whose top `rows` match the golden `name`,
/// waiting for what arrives off the loop, such as a fetched reply or a
/// decoded picture. Without a golden, while blessing, or once it times
/// out, a steady one, which `check_golden` then judges.
fn shot_matching(sway: &Sway, name: &str, rows: u32) -> Image {
  let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/{name}.png"));
  if let (None, Ok(bytes)) = (std::env::var_os("CRABSHELL_BLESS"), std::fs::read(&path)) {
    let golden = Image::decode(&bytes);
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
      let shot = sway.screenshot("HEADLESS-1");
      if golden_difference(&golden, &top_rows(&shot, rows)).is_none() {
        return shot;
      }
      std::thread::sleep(Duration::from_millis(50));
    }
  }
  steady_shot(sway, "HEADLESS-1")
}

/// Phase 18's exit check: the system example, with workspaces from a
/// fake niri, the weather from a local HTTP server, its clipboard filled
/// by `wl-copy` and its pictures in a temporary folder, matches its
/// goldens at all five scales: the bar, the clipboard's popup, and the
/// wallpaper picker. A click on the clipboard's picture puts it back on
/// top and closes the popup.
#[test]
#[ignore = "needs headless Sway and wl-clipboard"]
fn the_system_bar_and_its_popups_match_their_goldens() {
  const TEST: &str = "the_system_bar_and_its_popups_match_their_goldens";
  run_as_child();
  if !has_wl_copy() {
    eprintln!("skipped: wl-copy from wl-clipboard isn't installed");
    return;
  }
  for scale in ["1", "1.25", "1.5", "1.75", "2"] {
    let factor: f32 = scale.parse().expect("a scale");
    let rows = |logical: u32| (logical as f32 * factor) as u32;
    let layout = ((1680.0 / factor) as u32, (1050.0 / factor) as u32);
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", SYSTEM_MODE, "scale", scale]);
    let niri = FakeNiri::start(&sway);
    let server = LocalServer::start(vec![("/weather", WEATHER.as_bytes().to_vec())]);
    let folder = wallpaper_folder(&sway);
    let mut pointer = VirtualPointer::new(&sway);
    let shell = start_system(&sway, TEST, &system_env(&niri, &server, &folder, "system"));
    shell.wait_for(FIRST_FRAME);
    server.wait_for("/weather");
    let mut copies = fill_clipboard(&sway, &shell, &folder.join("lake.png"));

    let name = format!("system@{scale}x");
    let bar = shot_matching(&sway, &name, rows(system::HEIGHT));
    check_golden(&name, &bar, rows(system::HEIGHT));

    // A popup covers this point, logical 150 from the right and 100 down.
    let inside = ((layout.0 - 150) as f32 * factor) as u32;
    let inside = (inside, (100.0 * factor) as u32);
    let shown = |what: &str| {
      wait_for_shot(&sway, "HEADLESS-1", what, |shot| {
        rgb(shot, inside.0, inside.1) != rgb(&bar, inside.0, inside.1)
      });
    };

    // The clipboard's button, second from the right.
    pointer.move_to(layout.0 - 62, 15, layout);
    pointer.click();
    shown("the clipboard's popup");
    let name = format!("system-clipboard@{scale}x");
    check_golden(
      &name,
      &shot_matching(&sway, &name, rows(POPUP_ROWS)),
      rows(POPUP_ROWS),
    );
    // The picture, the second entry.
    pointer.move_to(layout.0 - 150, 120, layout);
    pointer.click();
    shell.wait_for("clipboard: 3 entries, <image> on top");
    wait_for_shot(&sway, "HEADLESS-1", "the popup to close", |shot| {
      let below = |shot: &Image| top_rows(shot, rows(POPUP_ROWS));
      golden_difference(&below(&bar), &below(shot)).is_none()
    });

    // The wallpaper picker's button, at the right.
    pointer.move_to(layout.0 - 22, 15, layout);
    pointer.click();
    shown("the wallpaper picker");
    let name = format!("system-wallpapers@{scale}x");
    check_golden(
      &name,
      &shot_matching(&sway, &name, rows(WALLPAPER_ROWS)),
      rows(WALLPAPER_ROWS),
    );
    for copy in &mut copies {
      let _ = copy.kill();
      let _ = copy.wait();
    }
  }
}

/// Part of Phase 18's exit check: the system bar follows a workspace
/// switch niri announces, asks niri for a workspace on a click, follows
/// its wallpaper folder only while the picker is open, and then, with its
/// clock and stats pinned, wakes for nothing in a minute: not the niri
/// stream, the weather's half-hourly fetch, the clipboard, the
/// backlights, or the folder.
#[test]
#[ignore = "needs headless Sway"]
fn the_system_bar_follows_niri_and_sleeps() {
  const TEST: &str = "the_system_bar_follows_niri_and_sleeps";
  run_as_child();
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", SYSTEM_MODE, "scale", "1"]);
  let niri = FakeNiri::start(&sway);
  let server = LocalServer::start(vec![("/weather", WEATHER.as_bytes().to_vec())]);
  let folder = wallpaper_folder(&sway);
  let mut pointer = VirtualPointer::new(&sway);
  let env = system_env(&niri, &server, &folder, "system idle");
  let shell = start_system(&sway, TEST, &env);
  shell.wait_for(FIRST_FRAME);
  server.wait_for("/weather");
  // Another program's window, whose click closes a popup.
  let _app = AppWindow::new(&sway, "org.example.App", "another program");
  let before = steady_shot(&sway, "HEADLESS-1");
  let bar = |shot: &Image| top_rows(shot, system::HEIGHT);

  // niri switches to `web` and says so; the bar follows.
  niri.send(r#"{"WorkspaceActivated":{"id":2,"focused":true}}"#);
  wait_for_shot(&sway, "HEADLESS-1", "web shown", |shot| {
    golden_difference(&bar(&before), &bar(shot)).is_some()
  });
  // Workspace 1's button, at the left, asks niri for it.
  pointer.move_to(8 + 11, 15, (1680, 1050));
  pointer.click();
  niri.wait_for_action(r#"{"Action":{"FocusWorkspace":{"reference":{"Id":1}}}}"#);

  // The picker follows its folder while open: a new picture shows.
  pointer.move_to(1680 - 22, 15, (1680, 1050));
  pointer.click();
  wait_for_shot(&sway, "HEADLESS-1", "the picker", |shot| {
    rgb(shot, 1680 - 150, 100) != rgb(&before, 1680 - 150, 100)
  });
  let opened = steady_shot(&sway, "HEADLESS-1");
  let picker = |shot: &Image| top_rows(shot, WALLPAPER_ROWS);
  small_png(&folder.join("added.png"), &[[0xa6, 0xe3, 0xa1]]);
  wait_for_shot(&sway, "HEADLESS-1", "the new picture", |shot| {
    golden_difference(&picker(&opened), &picker(shot)).is_some()
  });
  // A click on the other program closes it.
  pointer.move_to(840, 600, (1680, 1050));
  pointer.click();
  wait_for_shot(&sway, "HEADLESS-1", "the picker to close", |shot| {
    rgb(shot, 1680 - 150, 100) == rgb(&before, 1680 - 150, 100)
  });

  // Closed, it follows nothing: a picture added within the idle minute
  // wakes nothing.
  shell.wait_until_quiet();
  let mut wakeups = shell.wakeups_for(Duration::from_secs(20));
  small_png(&folder.join("later.png"), &[[0xf3, 0x8b, 0xa8]]);
  wakeups.extend(shell.wakeups_for(Duration::from_secs(40)));
  assert!(wakeups.is_empty(), "woke while idle: {wakeups:?}");
}

/// Part of Phase 18's exit check: the dashboard shows album art from a
/// plain `http:` URL on a local server, fetched once through
/// `cx.cover_art` into the cache, exactly as it shows the same picture
/// from a file.
#[test]
#[ignore = "needs headless Sway"]
fn the_dashboard_shows_cover_art_from_an_http_server() {
  const TEST: &str = "the_dashboard_shows_cover_art_from_an_http_server";
  run_as_child();
  let files = DashboardFiles::new();
  let art = std::fs::read(files.path("art.png")).expect("the art");
  let server = LocalServer::start(vec![("/covers/northern-lights", art)]);
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", DASHBOARD_MODE, "scale", "1"]);
  let cache = sway.runtime_dir().join("cache");
  let mut env = files.env();
  env.retain(|(name, _)| *name != "DASHBOARD_ART");
  env.push(("DASHBOARD_ART", server.url("/covers/northern-lights")));
  env.push(("XDG_CACHE_HOME", cache.display().to_string()));
  let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
  let shell = ExampleBar::spawn(&sway, this_as_shell(TEST), &env);
  shell.wait_for(FIRST_FRAME);
  wait_for_shot(&sway, "HEADLESS-1", "the album art", |shot| {
    let [r, g, b] = rgb(shot, 70, 200);
    r.abs_diff(0xf3) <= 6 && g.abs_diff(0x8b) <= 6 && b.abs_diff(0xa8) <= 6
  });
  wait_for_decoded_pictures(&sway, "1");
  let shot = steady_shot(&sway, "HEADLESS-1");
  check_golden("dashboard@1x", &shot, shot.height);
  assert_eq!(server.served(), ["/covers/northern-lights"]);
  let kept = std::fs::read_dir(cache.join("crabshell/art"))
    .expect("the art cache")
    .count();
  assert_eq!(kept, 1, "one picture in the cache");
}

#[test]
fn the_system_bar_reads_the_weather_pictures_and_the_cpus_temperature() {
  assert_eq!(system::parse_weather(WEATHER), Some((7.6, 2)));
  for bad in [
    "",
    "not json",
    "{}",
    r#"{"current":{"temperature_2m":"warm","weather_code":2}}"#,
    r#"{"current":{"temperature_2m":7.6}}"#,
    r#"{"current":{"temperature_2m":7.6,"weather_code":-1}}"#,
  ] {
    assert_eq!(system::parse_weather(bad), None, "{bad:?}");
  }
  assert_eq!(system::weather_icon(0), "weather-clear-symbolic");
  assert_eq!(system::weather_icon(65), "weather-showers-symbolic");
  assert_eq!(system::weather_icon(1000), "weather-severe-alert-symbolic");

  for picture in ["a.png", "B.JPG", "c.jpeg", "d.webp", "e.gif", "f.g.png"] {
    assert!(system::is_picture(picture), "{picture}");
  }
  for other in ["notes.txt", "png", ".png.txt", "icon.svg", ""] {
    assert!(!system::is_picture(other), "{other}");
  }

  let empty = SystemStats::default();
  assert_eq!(
    system::share("cpu", &empty, empty.cpu),
    "cpu –",
    "before a sample"
  );
  assert_eq!(system::cpu_temperature(&empty), None);
  let mut stats = pinned_stats();
  assert_eq!(system::share("mem", &stats, 0.4), "mem 40%");
  assert_eq!(
    system::cpu_temperature(&stats),
    Some(52.4),
    "the CPU's chip"
  );
  stats.temperatures.remove(1);
  assert_eq!(
    system::cpu_temperature(&stats),
    Some(40.0),
    "else the first"
  );
  stats.temperatures.clear();
  assert_eq!(system::cpu_temperature(&stats), None);
}

#[test]
fn the_wallpaper_folder_is_absolute_against_the_current_directory() {
  let here = std::env::current_dir().expect("a current directory");
  let wallpapers = |folder: Option<&str>, home: Option<&str>| {
    system::wallpapers_from(folder.map(Into::into), home.map(Into::into))
  };
  assert_eq!(
    wallpapers(Some("/srv/walls"), None),
    Path::new("/srv/walls")
  );
  assert_eq!(
    wallpapers(Some("walls"), Some("/home/me")),
    here.join("walls")
  );
  assert_eq!(
    wallpapers(None, Some("/home/me")),
    Path::new("/home/me/Pictures/Wallpapers")
  );
  for home in [None, Some(""), Some("me")] {
    let expected = here
      .join(home.unwrap_or_default())
      .join("Pictures/Wallpapers");
    assert_eq!(wallpapers(None, home), expected, "HOME {home:?}");
  }
}

#[test]
fn a_file_uri_escapes_all_but_unreserved_bytes() {
  assert_eq!(
    system::file_uri(Path::new("/home/me/Pictures/Night Sky #2 (ü).png")),
    "file:///home/me/Pictures/Night%20Sky%20%232%20%28%C3%BC%29.png"
  );
  assert_eq!(
    system::file_uri(Path::new("/a-b_c.d~e")),
    "file:///a-b_c.d~e"
  );
}

/// Part of Phase 18's exit check, on the fake compositor: a picture in
/// the wallpaper picker dragged out of its window offers other programs
/// its file's URI as `text/uri-list`.
#[test]
fn a_wallpaper_dragged_out_of_the_picker_offers_its_file_uri() {
  use smithay_client_toolkit::reexports::client::Connection;
  let (fake, stream) = fake::Fake::start(fake::Globals {
    pointer: true,
    data_device: true,
    ..fake::Globals::default()
  });
  let connection = Connection::from_socket(stream).expect("a connection");
  let mut event_loop = EventLoop::with_connection(connection).expect("the platform starts");
  let wait =
    |event_loop: &mut EventLoop, what: &str, done: &mut dyn FnMut(&mut EventLoop) -> bool| {
      let deadline = Instant::now() + TIMEOUT;
      while !done(event_loop) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        event_loop
          .dispatch(Some(Duration::from_millis(5)))
          .expect("the connection works");
      }
    };
  let cx = Cx::new(Runtime::current().new_owner());
  let picture = FolderEntry {
    path: PathBuf::from("/pictures/Night Sky.png"),
    name: String::from("Night Sky.png"),
    is_dir: false,
    size: 1,
    modified: None,
  };
  let bar = PanelWindow::new(&Output::new("FAKE-1"))
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(100)
    .child(Row::new().child(system::thumbnail(picture)));
  let id = event_loop
    .platform()
    .add_window(crabshell_ui::Window::new(&cx, bar));
  wait(&mut event_loop, "the first frame", &mut |event_loop| {
    let info = event_loop.platform().window_info(id);
    info.is_some_and(|info| info.commits > 0)
  });
  fake.run(|state| state.press_out(0, (40.0, 40.0), (40.0, 300.0)));
  let mut started = None;
  wait(&mut event_loop, "the drag", &mut |_| {
    started = fake
      .log()
      .into_iter()
      .find(|line| line.starts_with("start_drag "));
    started.is_some()
  });
  let types = started
    .expect("a drag")
    .rsplit(' ')
    .next()
    .map(String::from);
  assert_eq!(
    types.as_deref(),
    Some("text/uri-list,application/x-crabshell-drag")
  );
  let (sent, received) = mpsc::channel();
  fake.run(move |state| {
    sent
      .send(state.read_started("text/uri-list"))
      .expect("the test waits")
  });
  let mut stream = received.recv().expect("a stream");
  stream.set_nonblocking(true).expect("non-blocking");
  let mut bytes = Vec::new();
  wait(&mut event_loop, "the URI list", &mut |_| {
    use std::io::Read;
    let mut chunk = [0; 64];
    match stream.read(&mut chunk) {
      Ok(0) => true,
      Ok(n) => {
        bytes.extend_from_slice(&chunk[..n]);
        false
      }
      Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
      Err(error) => panic!("{error}"),
    }
  });
  assert_eq!(bytes, b"file:///pictures/Night%20Sky.png\r\n");
  cx.owner().dispose();
}
