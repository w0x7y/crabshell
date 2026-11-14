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
  reason = "the example's lock screen is tested; its main isn't run here"
)]
#[path = "../examples/lock.rs"]
mod lock;

#[allow(dead_code, reason = "the services' own tests use the rest")]
#[path = "../../crabshell-services/tests/pipewire/mod.rs"]
mod pipewire;

use std::io::{self, BufRead, BufReader};
use std::ops::Range;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crabshell::Shell;
use crabshell::prelude::*;
use crabshell_reactive::Runtime;
use crabshell_ui::{Content, NodeView, Rect};
use crabshell_wayland::{EventLoop, Platform, WindowId};
use pipewire::PrivateServer;
use sway::{AppWindow, Image, Sway, VirtualKeyboard, VirtualPointer, keys};

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
      .flat_map(|run| run.glyphs.iter())
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
  let golden = Image::decode(&bytes);
  let failure = if (golden.width, golden.height) != (top.width, top.height) {
    Some(format!(
      "size {}x{}, golden {}x{}",
      top.width, top.height, golden.width, golden.height
    ))
  } else {
    let differing = top
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
  };
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
  fn spawn(sway: &Sway, mut command: Command, extra: &[(&str, &str)]) -> ExampleBar {
    command.env_clear();
    let nowhere = sway.runtime_dir().join("no-pulse-server");
    command.env("PULSE_SERVER", format!("unix:{}", nowhere.display()));
    for (name, value) in extra {
      command.env(name, value);
    }
    let name = Path::new(command.get_program())
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_default();
    let mut child = command
      .env("PATH", std::env::var_os("PATH").unwrap_or_default())
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
    let tasks = format!("/proc/{}/task", self.child.id());
    std::fs::read_dir(&tasks)
      .expect("the example is running")
      .filter_map(Result::ok)
      .filter_map(|task| std::fs::read_to_string(task.path().join("status")).ok())
      .flat_map(|status| {
        status
          .lines()
          .filter(|line| line.contains("ctxt_switches"))
          .filter_map(|line| line.split_whitespace().last()?.parse::<u64>().ok())
          .collect::<Vec<u64>>()
      })
      .sum()
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
    let mut last = self.context_switches();
    let mut seen = Vec::new();
    while Instant::now() < end {
      std::thread::sleep(Duration::from_millis(50));
      let now = self.context_switches();
      if now != last {
        let wall = SystemTime::now()
          .duration_since(SystemTime::UNIX_EPOCH)
          .expect("the clock is past 1970");
        seen.push((wall.as_secs_f64() % 60.0, now - last));
        last = now;
      }
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
  if std::env::var_os("CRABSHELL_MEASURE").is_none() {
    eprintln!("not measuring: set CRABSHELL_MEASURE=1 to, which takes two minutes");
    return;
  }
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let counter = ExampleBar::start(&sway, "counter", true, &[]);
  let mut report = format!("counter example, {}\n", measure(&counter));
  drop(counter);
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
    Ok("counter") => crabshell::run(|cx| cx.per_output(counter::bar)),
    Ok("v0.1") => run_v0_1_bar_logging_its_volume(),
    Ok("exits") => crabshell::run(|cx| {
      let program = cx.process(|| ["sh", "-c", "echo $$; exec sleep 30"]);
      let _program = program.on_line(|pid| eprintln!("the program is {pid}"));
      let shell = cx.clone();
      cx.ipc("shell", "exit", move |_| shell.exit(3));
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

/// Waits for a shell to connect to `listener` and say it is ready; gives
/// the connection back.
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
  let mut said = [0_u8; 6];
  stream
    .read_exact(&mut said)
    .expect("the shell says something");
  assert_eq!(&said, b"ready\n");
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

/// A 4 by 4 PNG of one color, in the temp dir.
fn one_color_png(name: &str, [r, g, b]: [u8; 3]) -> PathBuf {
  let path = std::env::temp_dir().join(format!("crabshell-{}-{name}.png", std::process::id()));
  let file = std::fs::File::create(&path).expect("create the PNG");
  let mut encoder = png::Encoder::new(file, 4, 4);
  encoder.set_color(png::ColorType::Rgb);
  let mut writer = encoder.write_header().expect("the header");
  writer
    .write_image_data(&[r, g, b].repeat(16))
    .expect("the pixels");
  path
}

#[test]
#[ignore = "needs headless Sway"]
fn cx_quantize_colors_follows_its_path_then_sleeps_and_ends_with_its_context() {
  let sway = Sway::start();
  let runtime = Runtime::current();
  let (red, blue) = (
    one_color_png("quantize-red", [255, 0, 0]),
    one_color_png("quantize-blue", [0, 0, 255]),
  );
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
  _server: PrivateServer,
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
      _server: server,
      _system: system,
      session,
      env,
    }
  }

  fn start(&self, sway: &Sway) -> ExampleBar {
    let env: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    ExampleBar::start(sway, "desktop", true, &env)
  }

  /// Sends a notification, as an app would, and returns gdbus's answer.
  fn notify(&self, summary: &str, timeout_ms: i32) -> String {
    let output = Command::new("gdbus")
      .args(["call", "--address", &self.session.address])
      .args(["--dest", "org.freedesktop.Notifications"])
      .args(["--object-path", "/org/freedesktop/Notifications"])
      .args(["--method", "org.freedesktop.Notifications.Notify"])
      .args([
        "exit-check",
        "0",
        "",
        summary,
        "from the exit check",
        "[]",
        "{}",
      ])
      .arg(timeout_ms.to_string())
      .env_clear()
      .env("PATH", std::env::var_os("PATH").unwrap_or_default())
      .output()
      .expect("gdbus runs");
    assert!(output.status.success(), "gdbus: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
  }
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
        "#!/bin/sh\necho \"$1\" > {}\n",
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
  writeln!(
    stream,
    r#"{{"target":"{target}","function":"{function}","args":[]}}"#
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
#[test]
#[ignore = "needs headless Sway"]
fn cx_exit_ends_the_shell_with_its_status_programs_and_socket() {
  use std::io::Write;

  run_as_child();
  let sway = Sway::start();
  let mut shell = ExampleBar::spawn(
    &sway,
    this_as_shell("cx_exit_ends_the_shell_with_its_status_programs_and_socket"),
    &[(CHILD, "exits")],
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
