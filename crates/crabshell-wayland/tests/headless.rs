//! The platform on a real compositor: headless Sway, one per test.
//!
//! They need `sway`, `swaymsg` and `grim`, and `labwc` for the one test
//! on another wlroots compositor, so they are ignored by default. Run them without the session's `SWAYSOCK`, which
//! `Sway::start()` refuses:
//!
//! ```text
//! env -u SWAYSOCK cargo test -p crabshell-wayland --test headless -- --ignored --test-threads=1
//! ```

mod sway;

use std::cell::RefCell;
use std::io;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crabshell_reactive::{Runtime, Signal};
use crabshell_ui::{
  Align, Button, Color, Column, Content, Cx, Edge, Key, KeyEvent, KeyboardFocus, Margins, Output,
  PanelWindow, PopupWindow, Rect, Region, Row, Spacer, Style, Text, TextInput, Window,
};
use crabshell_ui::{CaptureFrame, CaptureSource, FloatingWindow, ScreencopyView, Transition};
use crabshell_wayland::{Event, EventLoop, Platform, Toplevel, WindowId};
use sway::{
  AppWindow, Image, InputMethod, Sway, VirtualKeyboard, VirtualPointer, keys, run_until, settle,
};

/// Set in the child that [`sway_refuses_to_start_beside_a_swaysock`] runs.
const GUARD_CHILD: &str = "CRABSHELL_GUARD_CHILD";

/// Needs no Sway: the refusal comes first. So it runs by default too.
#[test]
fn sway_refuses_to_start_beside_a_swaysock() {
  if std::env::var_os(GUARD_CHILD).is_some() {
    Sway::start();
    return;
  }
  // This test again, as its own process with a SWAYSOCK that names
  // nothing, and no PATH, so a missing refusal can't find sway either.
  let output = Command::new(std::env::current_exe().expect("the test binary"))
    .args(["--exact", "sway_refuses_to_start_beside_a_swaysock"])
    .args(["--nocapture"])
    .env_clear()
    .env(GUARD_CHILD, "1")
    .env("SWAYSOCK", "/nonexistent/sway.sock")
    .output()
    .expect("the test binary runs");
  let printed = format!(
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(!output.status.success(), "Sway started: {printed}");
  assert!(
        printed.contains(
            "env -u SWAYSOCK cargo test -p crabshell-wayland --test headless -- --ignored --test-threads=1"
        ),
        "{printed}"
    );
}

/// How the test bar is built.
#[derive(Clone, Copy)]
struct Setup {
  /// Whether the bar's button opens a [`menu`].
  popups: bool,
  /// Whether the bar opens a menu as it is built, before it draws or any
  /// button is pressed.
  at_start: bool,
  /// The edge the bar runs along.
  edge: Edge,
  /// The bar's margins.
  margins: Margins,
  /// When the bar takes the keyboard.
  keyboard: KeyboardFocus,
  /// Whether the bar's text field takes the focus as it is built.
  field: bool,
  /// Whether the bar has a box whose width animates with `wide`.
  animated: bool,
}

/// A bar across the top that opens nothing.
const PLAIN: Setup = Setup {
  popups: false,
  at_start: false,
  edge: Edge::Top,
  margins: Margins::all(0),
  keyboard: KeyboardFocus::None,
  field: false,
  animated: false,
};

/// What the test bar's bindings read and write.
#[derive(Clone, Copy)]
struct Knobs {
  label: Signal<String>,
  clicks: Signal<u32>,
  /// The first entry of every [`menu`].
  entry: Signal<String>,
  setup: Setup,
  /// Whether the button opens its menu from an effect, which runs in a flush, as a
  /// menu waiting on its app's reply would. The effect opens another each
  /// time `reopen` changes.
  from_effect: Signal<bool>,
  reopen: Signal<u32>,
  /// Whether the bar's mask leaves out the button.
  masked: Signal<bool>,
  /// The keys the bar's `on_key` got.
  keys: Signal<Vec<KeyEvent>>,
  /// When the bar takes the keyboard, from `setup.keyboard`.
  keyboard: Signal<KeyboardFocus>,
  /// What the bar's text field holds.
  typed: Signal<String>,
  /// The animated box's target: wide or narrow.
  wide: Signal<bool>,
  /// Whether the bar asks for a blur behind it.
  blurred: Signal<bool>,
  /// Whether the bar keeps the screen from idling.
  awake: Signal<bool>,
}

/// A popup with two entries: the first opens another menu beside it, and
/// the second closes them all.
fn menu(knobs: Knobs) -> PopupWindow {
  PopupWindow::new()
    .style(
      Style::new()
        .background(Color::hex("#313244"))
        .color(Color::hex("#cdd6f4"))
        .padding(4, 4),
    )
    .child(
      Column::new()
        .child(
          Button::new(Text::new(move || knobs.entry.get()))
            .style(Style::new().padding(2, 8))
            .on_click(move |cx| cx.open_popup(move |_| menu(knobs))),
        )
        .child(
          Button::new(Text::new("close"))
            .style(Style::new().padding(2, 8))
            .on_click(|cx| cx.close_popup()),
        ),
    )
}

/// A bar along one edge of `output`, 30 pixels tall or 100 wide: a
/// button at the start and a label at the end, on an opaque background.
fn bar(cx: &Cx, output: &str, knobs: Knobs) -> Window {
  let Knobs { label, clicks, .. } = knobs;
  let (anchors, width, height) = match knobs.setup.edge {
    Edge::Top => (Edge::Top | Edge::Left | Edge::Right, 0, 30),
    Edge::Bottom => (Edge::Bottom | Edge::Left | Edge::Right, 0, 30),
    Edge::Left => (Edge::Left | Edge::Top | Edge::Bottom, 100, 0),
    Edge::Right => (Edge::Right | Edge::Top | Edge::Bottom, 100, 0),
  };
  let panel = PanelWindow::new(&Output::new(output))
    .anchors(anchors)
    .width(width)
    .height(height)
    .margins(knobs.setup.margins)
    .keyboard_focus(move || knobs.keyboard.get())
    .mask(move || {
      knobs
        .masked
        .get()
        .then(|| Region::rect(200, 0, 100_000, 100_000))
    })
    .blur(move || {
      knobs
        .blurred
        .get()
        .then(|| Region::rect(0, 0, 100_000, 100_000))
    })
    .inhibit_idle(move || knobs.awake.get())
    .on_key(move |_, event| {
      knobs.keys.update(|keys| keys.push(event.clone()));
      false
    })
    .style(
      Style::new()
        .background(Color::hex("#1e1e2e"))
        .color(Color::hex("#cdd6f4")),
    )
    .child(
      Row::new()
        .padding(4, 6)
        .gap(6)
        .align(Align::Center)
        .child(
          Button::new(Text::new("+"))
            .style(
              Style::new()
                .background(Color::hex("#89b4fa"))
                .radius(4.0)
                .padding(2, 8),
            )
            .on_click(move |cx| {
              clicks.update(|count| *count += 1);
              if !knobs.setup.popups {
              } else if knobs.from_effect.get_untracked() {
                let opener = cx.clone();
                cx.effect(move || {
                  knobs.reopen.get();
                  opener.open_popup(move |_| menu(knobs));
                });
              } else {
                cx.open_popup(move |_| menu(knobs));
              }
            }),
        )
        .children_keyed(
          move || {
            if knobs.setup.at_start {
              vec![()]
            } else {
              Vec::new()
            }
          },
          |_| (),
          move |cx, _| {
            let opener = cx.clone();
            cx.effect(move || opener.open_popup(move |_| menu(knobs)));
            Spacer::new()
          },
        )
        .child(Spacer::new())
        .children_keyed(
          move || {
            if knobs.setup.animated {
              vec![()]
            } else {
              Vec::new()
            }
          },
          |_| (),
          move |cx, _| {
            let width = cx.animate(
              move || if knobs.wide.get() { 200.0 } else { 20.0 },
              Transition::new(300),
            );
            Row::new().style(move || {
              Style::new()
                .width(width.get())
                .height(10.0)
                .background(Color::hex("#a6e3a1"))
            })
          },
        )
        // Only some tests want a text field.
        .children_keyed(
          move || {
            if knobs.setup.field {
              vec![()]
            } else {
              Vec::new()
            }
          },
          |_| (),
          move |cx, _| {
            Row::new()
              .child(TextInput::new(knobs.typed).focus(true))
              .child(TextInput::new(cx.signal(String::new())).password(true))
          },
        )
        .child(Text::new(move || label.get())),
    );
  Window::new(cx, panel)
}

/// A connected loop that builds a bar on every output as it comes, and
/// the windows built, by output.
struct Shell {
  runtime: Runtime,
  knobs: Knobs,
  event_loop: EventLoop,
  built: Rc<RefCell<Vec<(String, WindowId)>>>,
  removed: Rc<RefCell<Vec<String>>>,
}

impl Shell {
  fn new(sway: &Sway) -> Shell {
    Shell::build(sway, PLAIN)
  }

  fn build(sway: &Sway, setup: Setup) -> Shell {
    let runtime = Runtime::current();
    let cx = Cx::new(runtime.new_owner());
    let knobs = Knobs {
      label: cx.signal(String::from("12:34")),
      clicks: cx.signal(0),
      entry: cx.signal(String::from("more")),
      setup,
      from_effect: cx.signal(false),
      reopen: cx.signal(0),
      masked: cx.signal(false),
      keys: cx.signal(Vec::new()),
      keyboard: cx.signal(setup.keyboard),
      typed: cx.signal(String::new()),
      wide: cx.signal(false),
      blurred: cx.signal(false),
      awake: cx.signal(false),
    };
    let mut event_loop = sway.connect();
    let built = Rc::new(RefCell::new(Vec::new()));
    let removed = Rc::new(RefCell::new(Vec::new()));
    let (on_built, on_removed) = (Rc::clone(&built), Rc::clone(&removed));
    event_loop
      .platform()
      .on_event(move |platform: &mut Platform, event| match event {
        Event::OutputAdded(output) => {
          let id = platform.add_window(bar(&cx, &output.name, knobs));
          on_built.borrow_mut().push((output.name.clone(), id));
        }
        Event::OutputRemoved(output) => {
          on_removed.borrow_mut().push(output.name.clone());
        }
        Event::WindowClosed(_) => {}
      });
    Shell {
      runtime,
      knobs,
      event_loop,
      built,
      removed,
    }
  }

  /// The window built for `output`.
  fn window(&self, output: &str) -> WindowId {
    self
      .built
      .borrow()
      .iter()
      .rev()
      .find(|(name, _)| name == output)
      .map(|(_, id)| *id)
      .unwrap_or_else(|| panic!("no window was built for {output}"))
  }

  /// Dispatches until `output` has a window that committed a frame, then
  /// until drawing stops.
  fn wait_for_bar(&mut self, output: &str) -> WindowId {
    let built = Rc::clone(&self.built);
    run_until(&mut self.event_loop, "the bar's first frame", |platform| {
      built
        .borrow()
        .iter()
        .filter(|(name, _)| name == output)
        .filter_map(|(_, id)| platform.window_info(*id))
        .any(|info| info.commits > 0)
    });
    settle(&mut self.event_loop);
    self.window(output)
  }
}

/// Checks the top of `shot` against `snapshot`, within 2/255 per channel.
/// On a mismatch both are saved under the target directory.
fn assert_matches(name: &str, shot: &Image, snapshot: &crabshell_wayland::Snapshot) {
  assert_eq!(
    shot.width, snapshot.width,
    "{name}: a bar is as wide as the output"
  );
  assert_matches_at(name, shot, snapshot, (0, 0));
}

/// Checks the part of `shot` at `(x, y)` in pixels against `snapshot`,
/// within 2/255 per channel. On a mismatch both are saved under the
/// target directory.
fn assert_matches_at(
  name: &str,
  shot: &Image,
  snapshot: &crabshell_wayland::Snapshot,
  (x, y): (u32, u32),
) {
  assert!(
    shot.width >= x + snapshot.width && shot.height >= y + snapshot.height,
    "{name}: a {}x{} screenshot can't hold a {}x{} window at {x},{y}",
    shot.width,
    shot.height,
    snapshot.width,
    snapshot.height
  );
  let row = |image_y: u32| {
    let start = ((y + image_y) * shot.width + x) as usize * 4;
    &shot.rgba[start..start + snapshot.width as usize * 4]
  };
  let shown: Vec<u8> = (0..snapshot.height).flat_map(row).copied().collect();
  let differing = shown
    .as_chunks::<4>()
    .0
    .iter()
    .zip(snapshot.rgba.as_chunks::<4>().0)
    .filter(|(shot, drawn)| {
      shot
        .iter()
        .zip(drawn.iter())
        .any(|(a, b)| a.abs_diff(*b) > 2)
    })
    .count();
  if differing > 0 {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("headless");
    std::fs::create_dir_all(&dir).expect("create the failure directory");
    shot.save(&dir.join(format!("{name}-screenshot.png")));
    Image {
      rgba: snapshot.rgba.clone(),
      width: snapshot.width,
      height: snapshot.height,
    }
    .save(&dir.join(format!("{name}-offscreen.png")));
    panic!(
      "{name}: {differing} pixels differ from the offscreen render by more than 2/255; both are in {}",
      dir.display()
    );
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn a_bar_has_the_right_buffer_and_matches_the_offscreen_render_at_each_scale() {
  for (scale, factor) in [("1", 1.0), ("1.5", 1.5), ("2", 2.0)] {
    let sway = Sway::start();
    // 1920 pixels is a whole number of logical pixels at every scale.
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let mut shell = Shell::new(&sway);
    let id = shell.wait_for_bar("HEADLESS-1");
    let platform = shell.event_loop.platform();
    let info = platform.window_info(id).expect("the bar is held");
    let logical_width = (1920.0 / factor) as u32;
    assert_eq!(info.size, Some((logical_width, 30)), "at {scale}");
    assert_eq!(info.scale, factor, "at {scale}");
    assert!(info.fractional, "Sway offers wp_fractional_scale_v1");
    let height = (30.0 * factor) as u32;
    assert_eq!(info.buffer_size, Some((1920, height)), "at {scale}");
    let snapshot = platform.snapshot(id).expect("the bar draws offscreen");
    assert_matches(
      &format!("bar@{scale}"),
      &sway.screenshot("HEADLESS-1"),
      &snapshot,
    );
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn an_idle_bar_commits_nothing_after_its_first_frame() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  let id = shell.wait_for_bar("HEADLESS-1");
  let platform = shell.event_loop.platform();
  let commits = platform.window_info(id).map(|info| info.commits);
  let wakeups = platform.stats().wayland_wakeups;
  for _ in 0..20 {
    shell
      .event_loop
      .dispatch(Some(Duration::from_millis(100)))
      .expect("the connection works");
  }
  let platform = shell.event_loop.platform();
  assert_eq!(platform.window_info(id).map(|info| info.commits), commits);
  assert_eq!(
    platform.stats().wayland_wakeups,
    wakeups,
    "the compositor sent nothing in two idle seconds"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn signal_changes_repaint_only_what_changed_and_match_a_full_draw() {
  for (scale, factor) in [("1", 1.0), ("1.5", 1.5)] {
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let mut shell = Shell::new(&sway);
    let id = shell.wait_for_bar("HEADLESS-1");
    // A clock's labels change width, and the spacer before the label
    // with them; the spacer draws nothing, so only the label is
    // repainted. The first change lands in a new buffer, which repaints
    // whole; after that the buffers are reused and only the label is
    // repainted, which the screenshots check.
    for label in ["1:05", "10:00", "9:59", "23:59", "0:00"] {
      let before = shell
        .event_loop
        .platform()
        .window_info(id)
        .expect("the bar is held");
      shell.knobs.label.set(String::from(label));
      run_until(&mut shell.event_loop, "the new label", |platform| {
        platform
          .window_info(id)
          .is_some_and(|info| info.commits > before.commits)
      });
      let info = shell
        .event_loop
        .platform()
        .window_info(id)
        .expect("the bar is held");
      let (width, height) = info.buffer_size.expect("a buffer was committed");
      assert_eq!(height, (30.0 * factor) as u32, "at {scale}");
      let damaged: u64 = info.last_damage.iter().map(|rect| rect.area()).sum();
      assert!(damaged > 0, "{label} at {scale}: the change was reported");
      assert!(
        damaged * 10 < u64::from(width) * u64::from(height),
        "{label} at {scale}: {damaged} pixels reported of {width}x{height}: {:?}",
        info.last_damage
      );
      settle(&mut shell.event_loop);
      let snapshot = shell
        .event_loop
        .platform()
        .snapshot(id)
        .expect("the bar draws offscreen");
      assert_matches(
        &format!("{label}@{scale}"),
        &sway.screenshot("HEADLESS-1"),
        &snapshot,
      );
    }
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn unplugging_an_output_drops_its_windows_and_their_nodes() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  shell.runtime.flush();
  let nodes = shell.runtime.node_count();

  sway.msg(&["create_output"]);
  shell.wait_for_bar("HEADLESS-2");
  assert!(
    shell.runtime.node_count() > nodes,
    "the second bar has nodes"
  );

  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  run_until(&mut shell.event_loop, "the second bar to go", |platform| {
    platform.windows().count() == 1 && platform.outputs().all(|output| output.name != "HEADLESS-2")
  });
  shell.runtime.flush();
  assert_eq!(shell.runtime.node_count(), nodes);
  assert_eq!(*shell.removed.borrow(), ["HEADLESS-2"]);
}

/// The button's rectangle in the bar, in logical pixels.
fn button_rect(platform: &Platform, id: WindowId) -> Rect {
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the bar is held");
  let view = window.view();
  let row = view.root().children().next().expect("the bar has a row");
  let button = row.children().next().expect("the row starts with a button");
  button.rect()
}

/// Starts Sway at scale 1 with a virtual pointer, waits for the bar and
/// for the platform to get the seat's pointer, and returns the center of
/// the bar's button.
fn clickable(sway: &Sway) -> (Shell, VirtualPointer, (u32, u32)) {
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut shell = Shell::new(sway);
  let mut pointer = VirtualPointer::new(sway);
  let id = shell.wait_for_bar("HEADLESS-1");
  let rect = button_rect(shell.event_loop.platform(), id);
  let center = (
    (rect.x + rect.width / 2.0) as u32,
    (rect.y + rect.height / 2.0) as u32,
  );
  // Moving the pointer makes the seat send it to the bar.
  pointer.move_to(center.0, center.1, (1920, 1080));
  (shell, pointer, center)
}

#[test]
#[ignore = "needs headless Sway"]
fn a_click_from_swaymsg_reaches_the_button() {
  let sway = Sway::start();
  let (mut shell, _pointer, (x, y)) = clickable(&sway);
  let (x, y) = (x.to_string(), y.to_string());
  sway.msg(&["seat", "-", "cursor", "set", &x, &y]);
  sway.msg(&["seat", "-", "cursor", "press", "button1"]);
  sway.msg(&["seat", "-", "cursor", "release", "button1"]);
  let clicks = shell.knobs.clicks;
  run_until(&mut shell.event_loop, "a click from swaymsg", |_| {
    clicks.get_untracked() == 1
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn a_click_from_a_virtual_pointer_reaches_the_button() {
  let sway = Sway::start();
  let (mut shell, mut pointer, _) = clickable(&sway);
  pointer.click();
  let clicks = shell.knobs.clicks;
  run_until(
    &mut shell.event_loop,
    "a click from the virtual pointer",
    |_| clicks.get_untracked() == 1,
  );
}

/// `rect.y` of the workspace on `output`: where the space other windows
/// get starts, below the top bars' exclusive zones and their margins.
fn workspace_top(sway: &Sway, output: &str) -> u32 {
  let json: String = sway
    .msg(&["-t", "get_workspaces"])
    .split_whitespace()
    .collect();
  // Sway writes each workspace's rect before its output.
  let before = json
    .find(&format!("\"output\":\"{output}\""))
    .map(|at| &json[..at])
    .unwrap_or_else(|| panic!("no workspace on {output}: {json}"));
  let rect = &before[before.rfind("\"rect\":").expect("a workspace has a rect")..];
  rect
    .split("\"y\":")
    .nth(1)
    .and_then(|rest| rest.split([',', '}']).next())
    .and_then(|y| y.trim().parse().ok())
    .unwrap_or_else(|| panic!("no rect.y in {rect}"))
}

#[test]
#[ignore = "needs headless Sway"]
fn margins_move_the_bar_and_count_in_its_zone() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let setup = Setup {
    margins: Margins::new(8, 8, 0, 8),
    keyboard: KeyboardFocus::OnDemand,
    ..PLAIN
  };
  let mut shell = Shell::build(&sway, setup);
  shell.wait_for_bar("HEADLESS-1");
  // The zone of 30 plus the top margin.
  assert_eq!(workspace_top(&sway, "HEADLESS-1"), 38);
  let shot = sway.screenshot("HEADLESS-1");
  let pixel = |x: u32, y: u32| {
    let at = ((y * shot.width + x) * 4) as usize;
    [shot.rgba[at], shot.rgba[at + 1], shot.rgba[at + 2]]
  };
  let bar = [0x1e, 0x1e, 0x2e];
  assert_eq!(pixel(960, 8), bar);
  assert_eq!(pixel(1911, 37), bar);
  assert_ne!(pixel(960, 7), bar, "the top margin");
  assert_ne!(pixel(7, 20), bar, "the left margin");
  assert_ne!(pixel(1912, 20), bar, "the right margin");
  assert_ne!(pixel(960, 38), bar, "below the bar");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_mask_lets_clicks_through_until_it_changes() {
  let sway = Sway::start();
  let (mut shell, mut pointer, (x, y)) = clickable(&sway);
  let clicks = shell.knobs.clicks;
  shell.knobs.masked.set(true);
  settle(&mut shell.event_loop);
  // The pointer has to move for the seat to look again at what is under
  // it.
  pointer.move_to(900, 500, (1920, 1080));
  pointer.move_to(x, y, (1920, 1080));
  pointer.click();
  settle(&mut shell.event_loop);
  assert_eq!(clicks.get_untracked(), 0, "a click outside the mask");

  shell.knobs.masked.set(false);
  settle(&mut shell.event_loop);
  pointer.move_to(900, 500, (1920, 1080));
  pointer.move_to(x, y, (1920, 1080));
  pointer.click();
  run_until(&mut shell.event_loop, "a click once unmasked", |_| {
    clicks.get_untracked() == 1
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn a_blur_the_compositor_cant_give_is_logged_once_and_the_bar_still_draws() {
  // Sway lacks ext-background-effect-v1.
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let mut shell = Shell::build(&sway, PLAIN);
  let id = shell.wait_for_bar("HEADLESS-1");
  shell.knobs.blurred.set(true);
  settle(&mut shell.event_loop);
  assert!(logs.text().contains("can't blur behind windows"));
  // Again after a resize and after asking again.
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720"]);
  settle(&mut shell.event_loop);
  shell.knobs.blurred.set(false);
  settle(&mut shell.event_loop);
  shell.knobs.blurred.set(true);
  let before = shell
    .event_loop
    .platform()
    .window_info(id)
    .expect("the bar is held")
    .commits;
  shell.knobs.label.set(String::from("still drawing"));
  run_until(
    &mut shell.event_loop,
    "a frame with the new label",
    |platform| {
      platform
        .window_info(id)
        .is_some_and(|info| info.commits > before)
    },
  );
  let text = logs.text();
  assert_eq!(
    text.matches("can't blur behind windows").count(),
    1,
    "{text}"
  );
}

/// A bar that takes the keyboard, on HEADLESS-1 at scale 1, and a
/// virtual keyboard. The keyboard comes first, as on a real seat: Sway
/// gives a layer surface the keyboard as it maps.
fn typing(sway: &Sway) -> (Shell, VirtualKeyboard) {
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let keyboard = VirtualKeyboard::new(sway);
  let setup = Setup {
    keyboard: KeyboardFocus::Exclusive,
    ..PLAIN
  };
  let mut shell = Shell::build(sway, setup);
  shell.wait_for_bar("HEADLESS-1");
  (shell, keyboard)
}

#[test]
#[ignore = "needs headless Sway"]
fn keys_reach_a_bar_that_takes_the_keyboard_and_tab_and_enter_click() {
  let sway = Sway::start();
  let (mut shell, mut keyboard) = typing(&sway);
  let (knobs, id) = (shell.knobs, shell.window("HEADLESS-1"));
  keyboard.tap(keys::A);
  run_until(&mut shell.event_loop, "the a key", |_| {
    knobs.keys.with_untracked(|keys| !keys.is_empty())
  });
  let a = knobs.keys.with_untracked(|keys| keys[0].clone());
  assert_eq!(
    (a.key, a.text.as_deref(), a.repeat),
    (Key::Character('a'), Some("a"), false)
  );

  keyboard.tap(keys::TAB);
  run_until(
    &mut shell.event_loop,
    "the focus on the button",
    |platform| {
      platform
        .windows()
        .find(|(held, _)| *held == id)
        .and_then(|(_, window)| window.focused())
        .is_some()
    },
  );
  keyboard.tap(keys::ENTER);
  run_until(&mut shell.event_loop, "a click from Enter", |_| {
    knobs.clicks.get_untracked() == 1
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn a_bar_takes_the_keyboard_once_its_focus_changes() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut keyboard = VirtualKeyboard::new(&sway);
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let knobs = shell.knobs;
  keyboard.tap(keys::A);
  settle(&mut shell.event_loop);
  assert!(
    knobs.keys.with_untracked(Vec::is_empty),
    "a bar without focus got a key"
  );

  knobs.keyboard.set(KeyboardFocus::Exclusive);
  settle(&mut shell.event_loop);
  keyboard.tap(keys::A);
  run_until(&mut shell.event_loop, "the a key", |_| {
    knobs.keys.with_untracked(|keys| keys.len() == 1)
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn a_held_key_repeats_until_released_and_then_nothing_wakes() {
  let sway = Sway::start();
  let (mut shell, mut keyboard) = typing(&sway);
  let knobs = shell.knobs;
  keyboard.press(keys::A);
  run_until(&mut shell.event_loop, "three repeats", |_| {
    knobs
      .keys
      .with_untracked(|keys| keys.iter().filter(|key| key.repeat).count() >= 3)
  });
  keyboard.release(keys::A);
  settle(&mut shell.event_loop);
  let typed = knobs.keys.with_untracked(Vec::len);
  // No repeat timer is left: one dispatch sleeps the whole time.
  let before = shell.event_loop.platform().stats().iterations;
  let started = std::time::Instant::now();
  shell
    .event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  assert!(
    started.elapsed() >= Duration::from_millis(1900),
    "{:?}",
    started.elapsed()
  );
  assert_eq!(shell.event_loop.platform().stats().iterations, before + 1);
  assert_eq!(knobs.keys.with_untracked(Vec::len), typed);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_key_held_as_the_keyboard_goes_stops_repeating() {
  let sway = Sway::start();
  let (mut shell, mut keyboard) = typing(&sway);
  let knobs = shell.knobs;
  keyboard.press(keys::A);
  run_until(&mut shell.event_loop, "a repeat", |_| {
    knobs
      .keys
      .with_untracked(|keys| keys.iter().any(|key| key.repeat))
  });
  keyboard.unplug();
  settle(&mut shell.event_loop);
  let before = shell.event_loop.platform().stats().iterations;
  let started = std::time::Instant::now();
  shell
    .event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  assert!(
    started.elapsed() >= Duration::from_millis(1900),
    "{:?}",
    started.elapsed()
  );
  assert_eq!(shell.event_loop.platform().stats().iterations, before + 1);
}

#[test]
#[ignore = "needs headless Sway"]
fn an_input_method_composes_and_commits_into_a_text_field() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut keyboard = VirtualKeyboard::new(&sway);
  let mut method = InputMethod::new(&sway);
  let setup = Setup {
    keyboard: KeyboardFocus::Exclusive,
    field: true,
    ..PLAIN
  };
  let mut shell = Shell::build(&sway, setup);
  let id = shell.wait_for_bar("HEADLESS-1");
  run_until(
    &mut shell.event_loop,
    "the input method to activate",
    |_| {
      method.poll();
      method.active()
    },
  );
  // What the first text field in the bar shows.
  fn field_text(node: crabshell_ui::NodeView<'_>) -> Option<String> {
    match node.content() {
      Content::Input(input) => Some(input.text.content().to_owned()),
      _ => node.children().find_map(field_text),
    }
  }
  let shown = |platform: &Platform| {
    platform
      .windows()
      .find(|(held, _)| *held == id)
      .and_then(|(_, window)| field_text(window.view().root()))
  };
  let before = method.dones();
  method.preedit("ni", 2);
  run_until(&mut shell.event_loop, "the preedit shown", |platform| {
    shown(platform).as_deref() == Some("ni")
  });
  // Sway ignores a commit whose serial isn't the latest `done`, so wait
  // for the one the field's answer brings.
  run_until(&mut shell.event_loop, "the field's answer", |_| {
    method.poll();
    method.dones() > before
  });
  assert_eq!(
    shell.knobs.typed.get_untracked(),
    "",
    "composing commits nothing"
  );

  method.commit("你");
  let typed = shell.knobs.typed;
  run_until(&mut shell.event_loop, "the committed text", |_| {
    typed.get_untracked() == "你"
  });
  // The field tells the input method what it now holds.
  run_until(&mut shell.event_loop, "the surrounding text", |_| {
    method.poll();
    method.surrounding() == "你"
  });
  // Tab moves to the password field, which input methods never see.
  keyboard.tap(keys::TAB);
  run_until(
    &mut shell.event_loop,
    "the input method to deactivate",
    |_| {
      method.poll();
      !method.active()
    },
  );
}

#[test]
#[ignore = "needs headless Sway and wl-clipboard"]
fn ctrl_c_copies_a_field_to_other_programs_and_ctrl_v_pastes_from_them() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut keyboard = VirtualKeyboard::new(&sway);
  let setup = Setup {
    keyboard: KeyboardFocus::Exclusive,
    field: true,
    ..PLAIN
  };
  let mut shell = Shell::build(&sway, setup);
  shell.wait_for_bar("HEADLESS-1");
  let typed = shell.knobs.typed;
  for key in [keys::H, keys::E, keys::L, keys::L, keys::O] {
    keyboard.tap(key);
  }
  run_until(&mut shell.event_loop, "the typing", |_| {
    typed.get_untracked() == "hello"
  });
  keyboard.ctrl(keys::A);
  keyboard.ctrl(keys::C);
  settle(&mut shell.event_loop);

  // wl-paste waits for the shell to write, so the loop runs meanwhile.
  let paste = sway
    .client("wl-paste")
    .arg("--no-newline")
    .stdout(std::process::Stdio::piped())
    .spawn()
    .expect("wl-paste runs");
  let paste = Rc::new(RefCell::new(Some(paste)));
  let output = Rc::new(RefCell::new(None));
  let (child, out) = (Rc::clone(&paste), Rc::clone(&output));
  run_until(&mut shell.event_loop, "wl-paste to finish", move |_| {
    let mut child = child.borrow_mut();
    match child.as_mut().map(std::process::Child::try_wait) {
      Some(Ok(Some(_))) => {
        let done = child.take().expect("still held");
        *out.borrow_mut() = Some(done.wait_with_output().expect("its output"));
        true
      }
      _ => false,
    }
  });
  let pasted = output.borrow_mut().take().expect("wl-paste finished");
  assert_eq!(String::from_utf8_lossy(&pasted.stdout), "hello");

  let mut copy = sway
    .client("wl-copy")
    .args(["--foreground", "--paste-once", "pasted"])
    .spawn()
    .expect("wl-copy runs");
  // The shell hears of wl-copy's offer in its own time: until then,
  // Ctrl+V pastes what the field itself copied.
  // Retried a few times a second, so pastes don't race each other.
  let mut last = std::time::Instant::now() - Duration::from_secs(1);
  run_until(&mut shell.event_loop, "the paste from wl-copy", |_| {
    if typed.get_untracked() != "pasted" && last.elapsed() > Duration::from_millis(300) {
      last = std::time::Instant::now();
      keyboard.ctrl(keys::A);
      keyboard.ctrl(keys::V);
    }
    typed.get_untracked() == "pasted"
  });
  let _ = copy.kill();
  let _ = copy.wait();
}

/// The `fullscreen_mode` Sway gives the window titled `title`: it comes
/// after the window's name in its object.
fn fullscreen_mode(tree: &str, title: &str) -> Option<String> {
  let view = &tree[tree.find(&format!("\"name\":\"{title}\""))?..];
  let mode = &view[view.find("\"fullscreen_mode\":")? + "\"fullscreen_mode\":".len()..];
  Some(mode.chars().take_while(char::is_ascii_digit).collect())
}

#[test]
#[ignore = "needs headless Sway"]
fn an_animation_draws_frames_while_it_moves_and_then_nothing_wakes() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let setup = Setup {
    animated: true,
    ..PLAIN
  };
  let mut shell = Shell::build(&sway, setup);
  let id = shell.wait_for_bar("HEADLESS-1");
  let commits = |shell: &mut Shell| {
    shell
      .event_loop
      .platform()
      .window_info(id)
      .expect("the bar is held")
      .commits
  };
  let before = commits(&mut shell);
  let woke = shell.event_loop.platform().stats().iterations;
  shell.knobs.wide.set(true);
  settle(&mut shell.event_loop);
  // A tick a frame, not a busy loop: settling's own 20 ms dispatches and
  // the frames come to well under 400 iterations.
  let iterations = shell.event_loop.platform().stats().iterations - woke;
  assert!(
    iterations < 400,
    "{iterations} loop iterations for one animation"
  );
  // 300 ms at 60 Hz is about 18 frames; frame callbacks may merge some.
  let frames = commits(&mut shell) - before;
  assert!(
    (6..=40).contains(&frames),
    "{frames} frames for a 300 ms animation"
  );
  let (_, window) = shell
    .event_loop
    .platform()
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the bar is held");
  // The animated box is the row's one child 10 pixels tall.
  let wide = window
    .view()
    .root()
    .children()
    .next()
    .and_then(|row| row.children().find(|child| child.rect().height == 10.0))
    .map(|animated| animated.rect().width);
  assert_eq!(wide, Some(200.0));

  // Ended: one dispatch sleeps the whole time.
  let iterations = shell.event_loop.platform().stats().iterations;
  let started = std::time::Instant::now();
  shell
    .event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  assert!(
    started.elapsed() >= Duration::from_millis(1900),
    "{:?}",
    started.elapsed()
  );
  assert_eq!(
    shell.event_loop.platform().stats().iterations,
    iterations + 1
  );
}

/// Sway's tree, without whitespace, for looking up windows in.
fn sway_tree(sway: &Sway) -> String {
  sway.msg(&["-t", "get_tree"]).split_whitespace().collect()
}

#[test]
#[ignore = "needs headless Sway"]
fn a_floating_window_shows_closes_comes_back_and_opens_popups() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut pointer = VirtualPointer::new(&sway);
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let cx = Cx::new(shell.runtime.new_owner());
  let (open, full) = (cx.signal(true), cx.signal(false));
  let floating = FloatingWindow::new()
    .title("crabshell-floating")
    .width(300)
    .height(200)
    .visible(move || open.get())
    .fullscreen(move || full.get())
    .on_close(move |_| open.set(false))
    .style(Style::new().background(Color::hex("#313244")))
    .child(Button::new(Text::new("menu")).on_click(|cx| {
      cx.open_popup(|_| {
        PopupWindow::new()
          .edges(Edge::Top)
          .gravity(Edge::Top)
          .child(Text::new("up"))
      })
    }));
  let id = shell
    .event_loop
    .platform()
    .add_window(Window::floating(&cx, floating));
  run_until(&mut shell.event_loop, "the floating window", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > 0)
  });
  assert!(sway_tree(&sway).contains("\"name\":\"crabshell-floating\""));

  // The window fills the workspace below the bar; its button fills it.
  // A virtual pointer's clicks never reached a toplevel on Sway, so
  // Sway's own cursor clicks.
  pointer.move_to(960, 600, (1920, 1080));
  settle(&mut shell.event_loop);
  sway.msg(&["seat", "-", "cursor", "set", "960", "600"]);
  sway.msg(&["seat", "-", "cursor", "press", "button1"]);
  sway.msg(&["seat", "-", "cursor", "release", "button1"]);
  run_until(
    &mut shell.event_loop,
    "a popup from the floating window",
    |platform| {
      popups(platform)
        .first()
        .and_then(|popup| platform.window_info(*popup))
        .is_some_and(|info| info.commits > 0)
    },
  );

  full.set(true);
  run_until(&mut shell.event_loop, "fullscreen", |_| {
    fullscreen_mode(&sway_tree(&sway), "crabshell-floating").as_deref() == Some("1")
  });
  full.set(false);
  settle(&mut shell.event_loop);

  // Sway's kill asks the window to close; on_close hides it.
  sway.msg(&["[title=crabshell-floating]", "kill"]);
  run_until(&mut shell.event_loop, "the window to close", |_| {
    !open.get_untracked()
  });
  settle(&mut shell.event_loop);
  assert!(!sway_tree(&sway).contains("crabshell-floating"));
  open.set(true);
  run_until(&mut shell.event_loop, "the window again", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > 0)
      && sway_tree(&sway).contains("crabshell-floating")
  });
}

/// The popups held, in the order they opened. Only one chain is ever
/// open, so each opened from the one before.
fn popups(platform: &Platform) -> Vec<WindowId> {
  platform
    .windows()
    .filter(|(_, window)| window.is_popup())
    .map(|(id, _)| id)
    .collect()
}

/// The rectangle of entry `index` of the [`menu`] in `popup`, in the
/// popup's logical pixels.
fn entry_rect(platform: &Platform, popup: WindowId, index: usize) -> Rect {
  let (_, window) = platform
    .windows()
    .find(|(id, _)| *id == popup)
    .expect("the popup is held");
  let view = window.view();
  let column = view.root().children().next().expect("a menu has a column");
  column.children().nth(index).expect("the entry").rect()
}

/// Where `popup` is relative to the bar, in logical pixels: its place
/// plus the places of the popups that opened it.
fn origin(platform: &Platform, popup: WindowId) -> (f32, f32) {
  let mut origin = (0.0, 0.0);
  for id in popups(platform) {
    let (x, y) = platform
      .window_info(id)
      .and_then(|info| info.position)
      .expect("the popup was placed");
    origin = (origin.0 + x as f32, origin.1 + y as f32);
    if id == popup {
      return origin;
    }
  }
  panic!("{popup:?} isn't open")
}

/// What this thread logs while the guard lives.
fn capture_logs() -> (Logs, tracing::subscriber::DefaultGuard) {
  let logs = Logs::default();
  let writer = logs.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_writer(move || writer.clone())
    .finish();
  (logs, tracing::subscriber::set_default(subscriber))
}

#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Logs {
  fn text(&self) -> String {
    String::from_utf8_lossy(&self.0.lock().expect("log buffer lock")).into_owned()
  }
}

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

fn center(rect: Rect, (x, y): (f32, f32)) -> (f32, f32) {
  (
    x + rect.x + rect.width / 2.0,
    y + rect.y + rect.height / 2.0,
  )
}

/// A shell whose bar's button opens a [`menu`], and a virtual pointer to
/// click with.
struct Menus {
  shell: Shell,
  pointer: VirtualPointer,
  bar: WindowId,
  /// Where the bar's output is in the layout, in logical pixels.
  offset: (f32, f32),
  /// The layout's size in logical pixels.
  layout: (u32, u32),
}

impl Menus {
  /// HEADLESS-1, 1920 by 1080 pixels at `scale`, with a bar across the
  /// top.
  fn start(sway: &Sway, scale: &str) -> Menus {
    let setup = Setup {
      popups: true,
      ..PLAIN
    };
    Menus::start_with(sway, scale, setup)
  }

  fn start_with(sway: &Sway, scale: &str, setup: Setup) -> Menus {
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let factor: f32 = scale.parse().expect("a scale");
    let mut shell = Shell::build(sway, setup);
    let pointer = VirtualPointer::new(sway);
    let bar = shell.wait_for_bar("HEADLESS-1");
    let layout = ((1920.0 / factor) as u32, (1080.0 / factor) as u32);
    let offset = match setup.edge {
      Edge::Bottom => (0.0, layout.1 as f32 - 30.0),
      Edge::Right => (layout.0 as f32 - 100.0, 0.0),
      Edge::Top | Edge::Left => (0.0, 0.0),
    };
    Menus {
      shell,
      pointer,
      bar,
      offset,
      layout,
    }
  }

  /// On a labwc from [`Sway::start_labwc`], at its output's own size and
  /// scale 1.
  fn start_on_labwc(labwc: &Sway) -> Menus {
    let setup = Setup {
      popups: true,
      ..PLAIN
    };
    let mut shell = Shell::build(labwc, setup);
    let pointer = VirtualPointer::new(labwc);
    let bar = shell.wait_for_bar("HEADLESS-1");
    let output = shell
      .event_loop
      .platform()
      .outputs()
      .find(|output| output.name == "HEADLESS-1")
      .cloned()
      .expect("labwc announced HEADLESS-1");
    let mode = output.mode.expect("a headless output has a mode");
    assert_eq!(output.scale, 1, "{output:?}");
    Menus {
      shell,
      pointer,
      bar,
      offset: (0.0, 0.0),
      layout: (mode.width, mode.height),
    }
  }

  /// Clicks at `(x, y)` in logical pixels from the bar's top left.
  fn click(&mut self, (x, y): (f32, f32)) {
    let (x, y) = (self.offset.0 + x, self.offset.1 + y);
    self.pointer.move_to(x as u32, y as u32, self.layout);
    self.pointer.click();
  }

  /// Clicks the bar's button and waits for its new menu.
  fn open_from_bar(&mut self) -> WindowId {
    let platform = self.shell.event_loop.platform();
    let before = popups(platform).first().copied();
    let button = button_rect(platform, self.bar);
    self.click(center(button, (0.0, 0.0)));
    self.wait_for("the bar's menu", |open| {
      open.len() == 1 && Some(open[0]) != before
    })[0]
  }

  /// Clicks entry `index` of `popup`.
  fn click_entry(&mut self, popup: WindowId, index: usize) {
    let platform = self.shell.event_loop.platform();
    let at = center(entry_rect(platform, popup, index), origin(platform, popup));
    self.click(at);
  }

  /// Waits until the open popups pass `done` and the last has drawn, then
  /// until drawing stops. Returns them.
  fn wait_for(&mut self, what: &str, done: impl Fn(&[WindowId]) -> bool) -> Vec<WindowId> {
    run_until(&mut self.shell.event_loop, what, |platform| {
      let open = popups(platform);
      done(&open)
        && open.last().is_none_or(|id| {
          platform
            .window_info(*id)
            .is_some_and(|info| info.commits > 0)
        })
    });
    settle(&mut self.shell.event_loop);
    popups(self.shell.event_loop.platform())
  }

  fn info(&mut self, popup: WindowId) -> crabshell_wayland::WindowInfo {
    self
      .shell
      .event_loop
      .platform()
      .window_info(popup)
      .expect("the popup is held")
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn a_click_opens_a_menu_below_the_button_at_each_scale() {
  for scale in ["1", "1.25", "1.5", "1.75", "2"] {
    let sway = Sway::start();
    let mut menus = Menus::start(&sway, scale);
    let popup = menus.open_from_bar();
    let info = menus.info(popup);
    let platform = menus.shell.event_loop.platform();
    let button = button_rect(platform, menus.bar);
    let below = (
      button.x.floor() as i32,
      (button.y + button.height).ceil() as i32,
    );
    assert_eq!(info.position, Some(below), "at {scale}");
    // Laid out after its bindings ran, at its opener's scale.
    assert_eq!(info.commits, 1, "the first frame was the last at {scale}");
    let (_, window) = platform
      .windows()
      .find(|(id, _)| *id == popup)
      .expect("held");
    let size = window.size();
    assert_eq!(
      info.size,
      Some((size.width as u32, size.height as u32)),
      "at {scale}"
    );
    let factor: f64 = scale.parse().expect("a scale");
    let pixels = |side: f32| (f64::from(side) * factor).round() as u32;
    assert_eq!(
      info.buffer_size,
      Some((pixels(size.width), pixels(size.height))),
      "at {scale}"
    );
    // At fractional scales the compositor rounds the place to pixels.
    if scale == "1" || scale == "2" {
      let snapshot = platform.snapshot(popup).expect("the menu draws offscreen");
      let at = (
        (f64::from(below.0) * factor) as u32,
        (f64::from(below.1) * factor) as u32,
      );
      assert_matches_at(
        &format!("menu@{scale}"),
        &sway.screenshot("HEADLESS-1"),
        &snapshot,
        at,
      );
    }
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn a_menu_an_effect_opens_is_shown_in_the_same_iteration() {
  let sway = Sway::start();
  let mut menus = Menus::start(&sway, "1");
  menus.shell.knobs.from_effect.set(true);
  let first = menus.open_from_bar();
  assert_eq!(
    menus.info(first).commits,
    1,
    "laid out after its bindings ran"
  );

  // Nothing else wakes the loop, so the iteration whose flush opens the
  // menu must also show it.
  menus.shell.knobs.reopen.update(|count| *count += 1);
  menus
    .shell
    .event_loop
    .dispatch(Some(Duration::ZERO))
    .expect("the connection works");
  let platform = menus.shell.event_loop.platform();
  let open = popups(platform);
  assert_eq!(open.len(), 1);
  assert_ne!(open[0], first, "the new menu replaced the first");
  assert!(
    platform
      .window_info(open[0])
      .is_some_and(|info| info.size.is_some()),
    "its surface was created"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn clicking_outside_a_menu_dismisses_it_and_frees_its_nodes() {
  let sway = Sway::start();
  let mut menus = Menus::start(&sway, "1");
  menus.shell.runtime.flush();
  let nodes = menus.shell.runtime.node_count();
  let menu = menus.open_from_bar();
  menus.click_entry(menu, 0);
  menus.wait_for("a submenu", |open| open.len() == 2);
  assert!(menus.shell.runtime.node_count() > nodes);
  // The compositor dismisses the whole chain.
  menus.click((900.0, 600.0));
  menus.wait_for("the compositor to dismiss the menu", <[_]>::is_empty);
  menus.shell.runtime.flush();
  assert_eq!(menus.shell.runtime.node_count(), nodes);
}

#[test]
#[ignore = "needs headless Sway"]
fn submenus_open_beside_their_entry_and_only_one_chain_stays_open() {
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let mut menus = Menus::start(&sway, "1");
  menus.shell.runtime.flush();
  let nodes = menus.shell.runtime.node_count();
  let first = menus.open_from_bar();
  menus.click_entry(first, 0);
  let chain = menus.wait_for("a submenu", |open| open.len() == 2);
  let entry = entry_rect(menus.shell.event_loop.platform(), first, 0);
  let beside = (
    (entry.x + entry.width).ceil() as i32,
    entry.y.floor() as i32,
  );
  assert_eq!(menus.info(chain[1]).position, Some(beside));

  // The entry again: the open submenu goes before a new one opens, or
  // the compositor would end the connection.
  menus.click_entry(first, 0);
  let again = menus.wait_for("a new submenu", |open| {
    open.len() == 2 && open[1] != chain[1]
  });
  assert_eq!(again[0], first);

  // The bar's button again: the whole chain goes, topmost first.
  let menu = menus.open_from_bar();
  menus.click_entry(menu, 0);
  let chain = menus.wait_for("a submenu", |open| open.len() == 2);
  // `close` in the submenu closes it and the menu that opened it.
  menus.click_entry(chain[1], 1);
  menus.wait_for("every menu to close", <[_]>::is_empty);
  menus.shell.runtime.flush();
  assert_eq!(menus.shell.runtime.node_count(), nodes);
  // A popup has no layer-shell settings to refuse.
  assert!(!logs.text().contains("isn't shown"), "{}", logs.text());
}

#[test]
#[ignore = "needs headless Sway"]
fn menus_open_toward_the_output_from_bars_on_the_other_edges() {
  for edge in [Edge::Bottom, Edge::Left, Edge::Right] {
    let sway = Sway::start();
    let setup = Setup {
      popups: true,
      edge,
      ..PLAIN
    };
    let mut menus = Menus::start_with(&sway, "1", setup);
    // Too wide to fit beside the button in a bar on the right.
    menus
      .shell
      .knobs
      .entry
      .set(String::from("a much longer first entry"));
    let popup = menus.open_from_bar();
    let platform = menus.shell.event_loop.platform();
    let button = button_rect(platform, menus.bar);
    let (_, window) = platform
      .windows()
      .find(|(id, _)| *id == popup)
      .expect("held");
    let size = window.size();
    let (width, height) = (size.width as i32, size.height as i32);
    let (left, top) = (button.x.floor() as i32, button.y.floor() as i32);
    let right = (button.x + button.width).ceil() as i32;
    let expected = match edge {
      // Below would leave the output, so it flips above the button.
      Edge::Bottom => (left, top - height),
      Edge::Left => (right, top),
      // Beside would leave the output, so it flips to the left.
      Edge::Right => (left - width, top),
      Edge::Top => unreachable!("tested above"),
    };
    assert_eq!(menus.info(popup).position, Some(expected), "{edge:?}");
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn a_menu_opened_before_the_bar_draws_waits_for_it_and_grabs_nothing() {
  let sway = Sway::start();
  let setup = Setup {
    popups: true,
    at_start: true,
    ..PLAIN
  };
  let mut menus = Menus::start_with(&sway, "1", setup);
  let open = menus.wait_for("the menu opened at start", |open| open.len() == 1);
  assert_eq!(menus.info(open[0]).commits, 1);
  // Below the bar's row, which opened it, as laid out: not at the corner
  // where the row was before its first layout.
  assert_eq!(menus.info(open[0]).position, Some((0, 30)));
  // No button was pressed, so it took no grab, and a submenu may take
  // none either: a grab under it would end the connection.
  menus.click_entry(open[0], 0);
  let chain = menus.wait_for("a submenu", |open| open.len() == 2);
  // With no grab, a click outside dismisses nothing.
  menus.click((900.0, 600.0));
  settle(&mut menus.shell.event_loop);
  assert_eq!(popups(menus.shell.event_loop.platform()), chain);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_menu_opened_after_the_seat_loses_its_pointer_grabs_nothing() {
  let sway = Sway::start();
  let mut menus = Menus::start(&sway, "1");
  menus.shell.knobs.from_effect.set(true);
  let first = menus.open_from_bar();
  // The only pointer goes, and the last button's serial with it.
  menus.pointer.unplug();
  settle(&mut menus.shell.event_loop);
  menus.shell.knobs.reopen.update(|count| *count += 1);
  let open = menus.wait_for("the reopened menu", |open| {
    open.len() == 1 && open[0] != first
  });
  menus.pointer = VirtualPointer::new(&sway);
  menus.click((900.0, 600.0));
  settle(&mut menus.shell.event_loop);
  assert_eq!(
    popups(menus.shell.event_loop.platform()),
    open,
    "with no grab, a click outside dismisses nothing"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn unplugging_an_output_with_open_menus_drops_them_first() {
  let sway = Sway::start();
  let mut menus = Menus::start(&sway, "1");
  menus.shell.runtime.flush();
  let nodes = menus.shell.runtime.node_count();
  sway.msg(&["create_output"]);
  // Placing one output moves the others Sway placed itself, so both.
  sway.msg(&["output", "HEADLESS-1", "position", "0", "0"]);
  sway.msg(&[
    "output",
    "HEADLESS-2",
    "mode",
    "1920x1080",
    "position",
    "1920",
    "0",
  ]);
  menus.bar = menus.shell.wait_for_bar("HEADLESS-2");
  menus.offset = (1920.0, 0.0);
  menus.layout = (3840, 1080);
  let menu = menus.open_from_bar();
  menus.click_entry(menu, 0);
  menus.wait_for("a submenu", |open| open.len() == 2);

  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  run_until(
    &mut menus.shell.event_loop,
    "HEADLESS-2's windows to go",
    |platform| {
      platform.windows().count() == 1
        && platform.outputs().all(|output| output.name != "HEADLESS-2")
    },
  );
  menus.shell.runtime.flush();
  assert_eq!(menus.shell.runtime.node_count(), nodes);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_menu_that_grows_is_resized_where_it_was() {
  let sway = Sway::start();
  let mut menus = Menus::start(&sway, "1");
  let popup = menus.open_from_bar();
  let before = menus.info(popup);
  menus
    .shell
    .knobs
    .entry
    .set(String::from("a much longer first entry"));
  let width = |info: &crabshell_wayland::WindowInfo| info.buffer_size.map(|(width, _)| width);
  run_until(&mut menus.shell.event_loop, "a wider menu", |platform| {
    platform
      .window_info(popup)
      .is_some_and(|info| width(&info) > width(&before))
  });
  settle(&mut menus.shell.event_loop);
  let after = menus.info(popup);
  assert_eq!(after.position, before.position);
  let (x, y) = after.position.expect("placed");
  let snapshot = menus
    .shell
    .event_loop
    .platform()
    .snapshot(popup)
    .expect("the menu draws offscreen");
  assert_matches_at(
    "grown menu",
    &sway.screenshot("HEADLESS-1"),
    &snapshot,
    (x as u32, y as u32),
  );

  // Wider than the output: the compositor slides it back on.
  menus.shell.knobs.entry.set("wide ".repeat(400));
  run_until(
    &mut menus.shell.event_loop,
    "the menu to slide",
    |platform| {
      platform
        .window_info(popup)
        .and_then(|info| info.position)
        .is_some_and(|(x, _)| x == 0)
    },
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn a_menu_that_grows_before_its_first_frame_is_placed_again() {
  let sway = Sway::start();
  let mut menus = Menus::start(&sway, "1");
  let button = button_rect(menus.shell.event_loop.platform(), menus.bar);
  menus.click(center(button, (0.0, 0.0)));
  // Created and placed at its first size, not yet configured.
  run_until(&mut menus.shell.event_loop, "the menu", |platform| {
    popups(platform).len() == 1
  });
  let popup = popups(menus.shell.event_loop.platform())[0];
  assert!(!menus.info(popup).configured);
  // Wider than the output by its first frame: once mapped, the
  // compositor must hear the new size and slide it back on.
  menus.shell.knobs.entry.set("wide ".repeat(400));
  run_until(
    &mut menus.shell.event_loop,
    "the menu to slide",
    |platform| {
      platform
        .window_info(popup)
        .and_then(|info| info.position)
        .is_some_and(|(x, _)| x == 0)
    },
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn an_open_menu_commits_nothing_while_idle() {
  let sway = Sway::start();
  let mut menus = Menus::start(&sway, "1");
  menus.open_from_bar();
  let stats = menus.shell.event_loop.platform().stats();
  for _ in 0..20 {
    menus
      .shell
      .event_loop
      .dispatch(Some(Duration::from_millis(100)))
      .expect("the connection works");
  }
  let now = menus.shell.event_loop.platform().stats();
  assert_eq!(now.commits, stats.commits);
  assert_eq!(
    now.wayland_wakeups, stats.wayland_wakeups,
    "the compositor sent nothing in two idle seconds"
  );
}

/// The grab and the popup rules on another wlroots compositor: menus,
/// submenus, a sibling submenu and a new menu from the bar replace the
/// open chain without a protocol error, and a click outside dismisses it.
#[test]
#[ignore = "needs labwc"]
fn on_labwc_menus_open_close_and_a_click_outside_dismisses_them() {
  let labwc = Sway::start_labwc();
  let mut menus = Menus::start_on_labwc(&labwc);
  menus.shell.runtime.flush();
  let nodes = menus.shell.runtime.node_count();
  let first = menus.open_from_bar();
  let button = button_rect(menus.shell.event_loop.platform(), menus.bar);
  let below = (
    button.x.floor() as i32,
    (button.y + button.height).ceil() as i32,
  );
  assert_eq!(menus.info(first).position, Some(below));

  menus.click_entry(first, 0);
  let chain = menus.wait_for("a submenu", |open| open.len() == 2);
  menus.click_entry(first, 0);
  menus.wait_for("a new submenu", |open| {
    open.len() == 2 && open[1] != chain[1]
  });
  let menu = menus.open_from_bar();
  menus.click_entry(menu, 0);
  menus.wait_for("a submenu", |open| open.len() == 2);

  // The grab: labwc dismisses the whole chain on a click outside it.
  let (width, height) = menus.layout;
  menus.click((width as f32 / 2.0, height as f32 / 2.0));
  menus.wait_for("labwc to dismiss the menus", <[_]>::is_empty);
  menus.shell.runtime.flush();
  assert_eq!(menus.shell.runtime.node_count(), nodes);
}

/// What an idle watch said, in order.
fn watch_idle(shell: &mut Shell, timeout: Duration) -> Rc<RefCell<Vec<bool>>> {
  let said = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&said);
  shell
    .event_loop
    .platform()
    .watch_idle(timeout, move |idle| log.borrow_mut().push(idle))
    .expect("Sway has ext-idle-notify-v1 and a seat");
  said
}

#[test]
#[ignore = "needs headless Sway"]
fn an_idle_watch_fires_after_its_timeout_and_input_ends_it() {
  let sway = Sway::start();
  let mut pointer = VirtualPointer::new(&sway);
  let mut shell = Shell::build(&sway, PLAIN);
  shell.wait_for_bar("HEADLESS-1");
  settle(&mut shell.event_loop);
  let said = watch_idle(&mut shell, Duration::from_millis(400));
  let started = Instant::now();
  run_until(&mut shell.event_loop, "idle", |_| {
    said.borrow().as_slice() == [true]
  });
  assert!(
    started.elapsed() >= Duration::from_millis(350),
    "{:?}",
    started.elapsed()
  );

  // Idle, nothing wakes the shell: one dispatch sleeps the whole time.
  let started = Instant::now();
  shell
    .event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  let slept = started.elapsed();
  assert!(slept >= Duration::from_millis(1900), "woke after {slept:?}");

  pointer.move_to(900, 500, (1920, 1080));
  run_until(&mut shell.event_loop, "back", |_| {
    said.borrow().as_slice() == [true, false]
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn a_bar_that_inhibits_idle_holds_the_watch_off_until_it_stops() {
  let sway = Sway::start();
  let mut shell = Shell::build(&sway, PLAIN);
  shell.knobs.awake.set(true);
  shell.wait_for_bar("HEADLESS-1");
  settle(&mut shell.event_loop);
  let said = watch_idle(&mut shell, Duration::from_millis(300));
  let until = Instant::now() + Duration::from_millis(1200);
  while Instant::now() < until {
    shell
      .event_loop
      .dispatch(Some(Duration::from_millis(100)))
      .expect("the connection works");
  }
  assert!(
    said.borrow().is_empty(),
    "idle while inhibited: {:?}",
    said.borrow()
  );

  shell.knobs.awake.set(false);
  run_until(&mut shell.event_loop, "idle once uninhibited", |_| {
    said.borrow().as_slice() == [true]
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn an_ended_idle_watch_says_nothing_more() {
  let sway = Sway::start();
  let mut shell = Shell::build(&sway, PLAIN);
  shell.wait_for_bar("HEADLESS-1");
  settle(&mut shell.event_loop);
  let said = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&said);
  let platform = shell.event_loop.platform();
  let watch = platform
    .watch_idle(Duration::from_millis(300), move |idle| {
      log.borrow_mut().push(idle)
    })
    .expect("a watch");
  platform.unwatch_idle(watch);
  platform.unwatch_idle(watch);
  let until = Instant::now() + Duration::from_millis(900);
  while Instant::now() < until {
    shell
      .event_loop
      .dispatch(Some(Duration::from_millis(100)))
      .expect("the connection works");
  }
  assert!(said.borrow().is_empty());
}

#[test]
#[ignore = "needs headless Sway"]
fn an_idle_watch_that_panics_is_logged_and_the_shell_goes_on() {
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let mut shell = Shell::build(&sway, PLAIN);
  let id = shell.wait_for_bar("HEADLESS-1");
  settle(&mut shell.event_loop);
  shell
    .event_loop
    .platform()
    .watch_idle(Duration::from_millis(200), |_| panic!("idle panic"))
    .expect("a watch");
  run_until(&mut shell.event_loop, "the panic logged", |_| {
    logs.text().contains("an idle watch panicked: idle panic")
  });
  // Still drawing.
  let before = shell
    .event_loop
    .platform()
    .window_info(id)
    .expect("held")
    .commits;
  shell.knobs.label.set(String::from("after"));
  run_until(&mut shell.event_loop, "a frame", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > before)
  });
}

/// What a toplevel watch heard: the last list, and how many lists came.
type Heard = Rc<RefCell<(Vec<Toplevel>, u32)>>;

fn watch_toplevels(shell: &mut Shell) -> Heard {
  let heard = Heard::default();
  let log = Rc::clone(&heard);
  shell.event_loop.platform().watch_toplevels(move |list| {
    let mut log = log.borrow_mut();
    *log = (list.to_vec(), log.1 + 1);
  });
  heard
}

/// The toplevel titled `title` in the last list heard.
fn titled(heard: &Heard, title: &str) -> Option<Toplevel> {
  let heard = heard.borrow();
  heard
    .0
    .iter()
    .find(|toplevel| toplevel.title == title)
    .cloned()
}

/// Shows a floating window titled `title`, which hides when asked to
/// close, and its title and visibility.
fn show_floating(shell: &mut Shell, title: &str) -> (Signal<String>, Signal<bool>) {
  let cx = Cx::new(shell.runtime.new_owner());
  let (title, open) = (cx.signal(title.to_owned()), cx.signal(true));
  let window = FloatingWindow::new()
    .title(move || title.get())
    .width(300)
    .height(200)
    .visible(move || open.get())
    .on_close(move |_| open.set(false))
    .child(Text::new("floating"));
  shell
    .event_loop
    .platform()
    .add_window(Window::floating(&cx, window));
  (title, open)
}

#[test]
#[ignore = "needs headless Sway"]
fn toplevels_are_listed_with_their_title_app_id_and_outputs() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let (title, open) = show_floating(&mut shell, "crabshell-floating");
  let heard = watch_toplevels(&mut shell);
  // Sway lists a window before it says which output it is on.
  run_until(&mut shell.event_loop, "the floating window", |_| {
    titled(&heard, "crabshell-floating").is_some_and(|toplevel| toplevel.outputs == ["HEADLESS-1"])
  });
  let ours = titled(&heard, "crabshell-floating").expect("listed");
  // A floating window sets no app id, and the bar is no toplevel.
  assert_eq!(ours.app_id, "");
  assert_eq!(heard.borrow().0.len(), 1);

  title.set(String::from("renamed"));
  run_until(&mut shell.event_loop, "the new title", |_| {
    titled(&heard, "renamed").is_some_and(|toplevel| toplevel.id == ours.id)
  });
  sway.msg(&["create_output"]);
  sway.msg(&[
    "[title=renamed]",
    "move",
    "container",
    "to",
    "output",
    "HEADLESS-2",
  ]);
  run_until(
    &mut shell.event_loop,
    "the window on the new output",
    |_| titled(&heard, "renamed").is_some_and(|toplevel| toplevel.outputs == ["HEADLESS-2"]),
  );

  let _app = AppWindow::new(&sway, "org.example.App", "another program");
  run_until(&mut shell.event_loop, "the other program's window", |_| {
    titled(&heard, "another program").is_some_and(|toplevel| toplevel.app_id == "org.example.App")
  });
  // Hidden, the floating window's toplevel goes.
  open.set(false);
  run_until(&mut shell.event_loop, "the floating window gone", |_| {
    titled(&heard, "renamed").is_none() && heard.borrow().0.len() == 1
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn toplevel_requests_reach_the_window() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let (_, open) = show_floating(&mut shell, "crabshell-floating");
  let heard = watch_toplevels(&mut shell);
  run_until(&mut shell.event_loop, "the floating window", |_| {
    titled(&heard, "crabshell-floating").is_some()
  });
  // Opened last, the other program's window takes the focus.
  let _app = AppWindow::new(&sway, "org.example.App", "another program");
  run_until(&mut shell.event_loop, "the other window focused", |_| {
    titled(&heard, "another program").is_some_and(|toplevel| toplevel.activated)
  });
  let ours = || titled(&heard, "crabshell-floating").expect("listed");
  let id = ours().id;
  assert!(!ours().activated);

  shell.event_loop.platform().activate_toplevel(id);
  run_until(&mut shell.event_loop, "activated", |_| ours().activated);
  shell
    .event_loop
    .platform()
    .set_toplevel_fullscreen(id, true);
  run_until(&mut shell.event_loop, "fullscreen", |_| {
    ours().fullscreen
      && fullscreen_mode(&sway_tree(&sway), "crabshell-floating").as_deref() == Some("1")
  });
  shell
    .event_loop
    .platform()
    .set_toplevel_fullscreen(id, false);
  run_until(&mut shell.event_loop, "not fullscreen", |_| {
    !ours().fullscreen
  });
  // Sway ignores maximize and minimize; labwc's test below has them.

  // An unknown id is ignored.
  shell.event_loop.platform().close_toplevel(u64::MAX);
  shell.event_loop.platform().close_toplevel(id);
  run_until(&mut shell.event_loop, "the window to close", |_| {
    !open.get_untracked()
  });
}

#[test]
#[ignore = "needs headless labwc"]
fn on_labwc_maximize_and_minimize_reach_the_window() {
  let labwc = Sway::start_labwc();
  let mut shell = Shell::new(&labwc);
  shell.wait_for_bar("HEADLESS-1");
  show_floating(&mut shell, "crabshell-floating");
  let heard = watch_toplevels(&mut shell);
  run_until(&mut shell.event_loop, "the floating window", |_| {
    titled(&heard, "crabshell-floating").is_some()
  });
  let ours = || titled(&heard, "crabshell-floating").expect("listed");
  let id = ours().id;
  for maximized in [true, false] {
    let platform = shell.event_loop.platform();
    platform.set_toplevel_maximized(id, maximized);
    run_until(&mut shell.event_loop, "maximized", |_| {
      ours().maximized == maximized
    });
  }
  for minimized in [true, false] {
    let platform = shell.event_loop.platform();
    platform.set_toplevel_minimized(id, minimized);
    run_until(&mut shell.event_loop, "minimized", |_| {
      ours().minimized == minimized
    });
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn nothing_hears_of_toplevels_until_a_watch_asks() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  settle(&mut shell.event_loop);
  let wakeups = shell.event_loop.platform().stats().wayland_wakeups;
  let mut app = AppWindow::new(&sway, "org.example.App", "before");
  app.set_title("after");
  let until = Instant::now() + Duration::from_millis(500);
  while Instant::now() < until {
    shell
      .event_loop
      .dispatch(Some(Duration::from_millis(100)))
      .expect("the connection works");
  }
  assert_eq!(shell.event_loop.platform().stats().wayland_wakeups, wakeups);

  let heard = watch_toplevels(&mut shell);
  run_until(&mut shell.event_loop, "the other program's window", |_| {
    titled(&heard, "after").is_some()
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn a_toplevel_watch_wakes_nothing_while_nothing_changes() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  show_floating(&mut shell, "crabshell-floating");
  let heard = watch_toplevels(&mut shell);
  run_until(&mut shell.event_loop, "the floating window", |_| {
    titled(&heard, "crabshell-floating").is_some()
  });
  settle(&mut shell.event_loop);
  let lists = heard.borrow().1;
  // One dispatch is one iteration however soon it wakes, so it is timed.
  let started = Instant::now();
  shell
    .event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  let slept = started.elapsed();
  assert!(slept >= Duration::from_millis(1900), "woke after {slept:?}");
  assert_eq!(heard.borrow().1, lists, "a list came with nothing changed");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_toplevel_watch_that_panics_is_logged_and_the_shell_goes_on() {
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  let id = shell.wait_for_bar("HEADLESS-1");
  shell
    .event_loop
    .platform()
    .watch_toplevels(|_| panic!("toplevel panic"));
  run_until(&mut shell.event_loop, "the panic logged", |_| {
    logs
      .text()
      .contains("a toplevel watch panicked: toplevel panic")
  });
  let before = shell
    .event_loop
    .platform()
    .window_info(id)
    .expect("held")
    .commits;
  shell.knobs.label.set(String::from("after"));
  run_until(&mut shell.event_loop, "a frame", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > before)
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn on_labwc_a_dialog_names_its_parent_until_the_parent_closes() {
  // Sway never says a window's parent; labwc does.
  let labwc = Sway::start_labwc();
  let mut shell = Shell::new(&labwc);
  shell.wait_for_bar("HEADLESS-1");
  let heard = watch_toplevels(&mut shell);
  let mut app = AppWindow::new(&labwc, "org.example.App", "main");
  app.open_child("dialog");
  run_until(&mut shell.event_loop, "the dialog and its parent", |_| {
    let main = titled(&heard, "main").map(|toplevel| toplevel.id);
    main.is_some() && titled(&heard, "dialog").is_some_and(|dialog| dialog.parent == main)
  });
  app.close_first();
  run_until(&mut shell.event_loop, "the dialog without a parent", |_| {
    titled(&heard, "main").is_none()
      && titled(&heard, "dialog").is_some_and(|dialog| dialog.parent.is_none())
  });
}

const RED: Color = Color::rgb(0xe0, 0x20, 0x20);
const BLUE: Color = Color::rgb(0x20, 0x40, 0xe0);

/// The test view's size, half of HEADLESS-2's 640 by 360.
const VIEW: (u32, u32) = (320, 180);

/// What the screencopy tests change: HEADLESS-2's color, and the view's
/// source, liveness and presence.
#[derive(Clone, Copy)]
struct Screens {
  color: Signal<Color>,
  source: Signal<CaptureSource>,
  live: Signal<bool>,
  shown: Signal<bool>,
}

/// A loop with HEADLESS-2, 640 by 360, filled with `RED` by a panel, and
/// once that shows, a panel in HEADLESS-1's corner with one view of
/// HEADLESS-2, `VIEW` in size. Returns once the view has its first frame.
fn screens(sway: &Sway, live: bool) -> (EventLoop, Screens, WindowId) {
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  sway.msg(&["create_output"]);
  sway.msg(&["output", "HEADLESS-2", "mode", "640x360", "scale", "1"]);
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let knobs = Screens {
    color: cx.signal(RED),
    source: cx.signal(CaptureSource::Output(String::from("HEADLESS-2"))),
    live: cx.signal(live),
    shown: cx.signal(true),
  };
  let mut event_loop = sway.connect();
  let fill = PanelWindow::new(&Output::new("HEADLESS-2"))
    .anchors(Edge::Top | Edge::Bottom | Edge::Left | Edge::Right)
    .style(move || Style::new().background(knobs.color.get()));
  event_loop.platform().add_window(Window::new(&cx, fill));
  wait_for_color(sway, &mut event_loop, "HEADLESS-2", (0, 0, 640, 360), RED);
  let view = move |_: &Cx, _| {
    ScreencopyView::new(move || knobs.source.get())
      .live(move || knobs.live.get())
      .style(Style::new().width(VIEW.0 as f32).height(VIEW.1 as f32))
  };
  let viewer = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Left)
    .width(VIEW.0)
    .height(VIEW.1)
    .child(Row::new().children_keyed(
      move || if knobs.shown.get() { vec![0] } else { vec![] },
      |key| *key,
      view,
    ));
  let viewer = event_loop.platform().add_window(Window::new(&cx, viewer));
  run_until(&mut event_loop, "the view's first frame", |platform| {
    frame_of(platform, viewer).is_some()
  });
  (event_loop, knobs, viewer)
}

/// The frame the test view shows, if any.
fn frame_of(platform: &Platform, viewer: WindowId) -> Option<CaptureFrame> {
  let (_, window) = platform.windows().find(|(id, _)| *id == viewer)?;
  let view = window.view();
  let row = view.root().children().next()?;
  let node = row.children().next()?;
  match node.content() {
    Content::Capture(capture) => Some(capture.frame.clone()),
    _ => None,
  }
}

/// Whether every pixel of `rgba` is `color`, opaque, within 2/255.
fn all_of(rgba: &[u8], color: Color) -> bool {
  let expected = [color.r, color.g, color.b, 255];
  rgba
    .as_chunks::<4>()
    .0
    .iter()
    .all(|pixel| pixel.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 2))
}

/// Whether the test view's frame is all `color`, at the size it needs.
fn shows(platform: &Platform, viewer: WindowId, color: Color) -> bool {
  frame_of(platform, viewer)
    .is_some_and(|frame| (frame.width, frame.height) == VIEW && all_of(&frame.rgba, color))
}

/// The pixels of `shot` in `x`, `y`, `width`, `height`.
fn area(shot: &Image, (x, y, width, height): (u32, u32, u32, u32)) -> Vec<u8> {
  (y..y + height)
    .flat_map(|row| {
      let start = (row * shot.width + x) as usize * 4;
      shot.rgba[start..start + width as usize * 4].to_vec()
    })
    .collect()
}

/// Dispatches until `output` shows `color` over `rect` in grim's
/// screenshots.
fn wait_for_color(
  sway: &Sway,
  event_loop: &mut EventLoop,
  output: &str,
  rect: (u32, u32, u32, u32),
  color: Color,
) {
  let deadline = Instant::now() + Duration::from_secs(10);
  while !all_of(&area(&sway.screenshot(output), rect), color) {
    assert!(Instant::now() < deadline, "{output} never showed {color:?}");
    event_loop
      .dispatch(Some(Duration::from_millis(50)))
      .expect("the connection works");
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn a_view_shows_an_outputs_pixels_once_and_ends_its_session() {
  let sway = Sway::start();
  let (mut event_loop, knobs, viewer) = screens(&sway, false);
  assert!(shows(event_loop.platform(), viewer, RED));
  assert_eq!(event_loop.platform().capture_sessions(), 0, "one frame");
  settle(&mut event_loop);
  wait_for_color(
    &sway,
    &mut event_loop,
    "HEADLESS-1",
    (0, 0, VIEW.0, VIEW.1),
    RED,
  );

  knobs.color.set(BLUE);
  wait_for_color(&sway, &mut event_loop, "HEADLESS-2", (0, 0, 640, 360), BLUE);
  settle(&mut event_loop);
  assert!(
    shows(event_loop.platform(), viewer, RED),
    "still the one frame"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn a_live_view_follows_its_output_and_sleeps_while_it_doesnt_change() {
  let sway = Sway::start();
  let (mut event_loop, knobs, viewer) = screens(&sway, true);
  assert_eq!(event_loop.platform().capture_sessions(), 1);
  knobs.color.set(BLUE);
  run_until(&mut event_loop, "a frame of the new color", |platform| {
    shows(platform, viewer, BLUE)
  });
  settle(&mut event_loop);
  wait_for_color(
    &sway,
    &mut event_loop,
    "HEADLESS-1",
    (0, 0, VIEW.0, VIEW.1),
    BLUE,
  );

  // Nothing changes, so the compositor sends nothing.
  let iterations = event_loop.platform().stats().iterations;
  let started = Instant::now();
  event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  assert!(started.elapsed() >= Duration::from_millis(1900));
  assert_eq!(event_loop.platform().stats().iterations, iterations + 1);
  assert_eq!(event_loop.platform().capture_sessions(), 1);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_view_gone_moved_or_waiting_has_no_session() {
  let sway = Sway::start();
  let (mut event_loop, knobs, viewer) = screens(&sway, true);
  let sessions = |event_loop: &mut EventLoop| event_loop.platform().capture_sessions();
  knobs.shown.set(false);
  run_until(&mut event_loop, "the view gone", |platform| {
    platform.capture_sessions() == 0
  });
  knobs.shown.set(true);
  run_until(&mut event_loop, "a new view", |platform| {
    platform.capture_sessions() == 1
  });

  // An output not there yet is waited for.
  knobs
    .source
    .set(CaptureSource::Output(String::from("HEADLESS-3")));
  run_until(&mut event_loop, "the new source", |platform| {
    platform.capture_sessions() == 0
  });
  sway.msg(&["create_output"]);
  run_until(&mut event_loop, "HEADLESS-3 captured", |platform| {
    platform.capture_sessions() == 1 && frame_of(platform, viewer).is_some()
  });

  event_loop.platform().remove_window(viewer);
  event_loop
    .dispatch(Some(Duration::ZERO))
    .expect("the connection works");
  assert_eq!(sessions(&mut event_loop), 0, "the window took it");
}

#[test]
#[ignore = "needs headless Sway"]
fn unplugging_the_output_ends_its_session_and_keeps_the_last_frame() {
  let sway = Sway::start();
  let (mut event_loop, _, viewer) = screens(&sway, true);
  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  run_until(&mut event_loop, "the session to end", |platform| {
    platform.capture_sessions() == 0
  });
  assert!(shows(event_loop.platform(), viewer, RED));
}

const GREEN: Color = Color::rgb(0x20, 0xc0, 0x40);

#[test]
#[ignore = "needs headless Sway"]
fn views_of_windows_show_their_pixels_and_keep_the_last_frame() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let mut event_loop = sway.connect();
  let heard: Rc<RefCell<Vec<Toplevel>>> = Rc::default();
  let log = Rc::clone(&heard);
  event_loop
    .platform()
    .watch_toplevels(move |list| *log.borrow_mut() = list.to_vec());
  // An id no window has yet is waited for.
  let source = cx.signal(CaptureSource::Toplevel(u64::MAX));
  let viewer = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Left)
    .width(VIEW.0)
    .height(VIEW.1)
    .child(
      Row::new().child(
        ScreencopyView::new(move || source.get())
          .live(true)
          .style(Style::new().width(VIEW.0 as f32).height(VIEW.1 as f32)),
      ),
    );
  let viewer = event_loop.platform().add_window(Window::new(&cx, viewer));
  let open = cx.signal(true);
  let floating = FloatingWindow::new()
    .title("captured")
    .width(300)
    .height(200)
    .visible(move || open.get())
    .style(Style::new().background(GREEN));
  event_loop
    .platform()
    .add_window(Window::floating(&cx, floating));
  run_until(&mut event_loop, "the window listed", |_| {
    heard
      .borrow()
      .iter()
      .any(|toplevel| toplevel.title == "captured")
  });
  settle(&mut event_loop);
  assert_eq!(event_loop.platform().capture_sessions(), 0);

  let id = heard.borrow()[0].id;
  source.set(CaptureSource::Toplevel(id));
  run_until(&mut event_loop, "the window's frame", |platform| {
    frame_of(platform, viewer).is_some_and(|frame| all_of(&frame.rgba, GREEN))
  });
  assert_eq!(event_loop.platform().capture_sessions(), 1);
  // A window that doesn't change sends nothing, live view or not.
  settle(&mut event_loop);
  let iterations = event_loop.platform().stats().iterations;
  event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  assert_eq!(event_loop.platform().stats().iterations, iterations + 1);

  open.set(false);
  run_until(&mut event_loop, "the session to end", |platform| {
    platform.capture_sessions() == 0
  });
  let frame = frame_of(event_loop.platform(), viewer).expect("the last frame stays");
  assert!(all_of(&frame.rgba, GREEN));

  // Another program's window, with an app id, pairs too. Its 100 pixel
  // square of zeros shows black, and isn't scaled up.
  let _app = AppWindow::new(&sway, "org.example.App", "another program");
  let other = |heard: &Rc<RefCell<Vec<Toplevel>>>| {
    let heard = heard.borrow();
    let found = heard
      .iter()
      .find(|toplevel| toplevel.app_id == "org.example.App");
    found.map(|toplevel| toplevel.id)
  };
  run_until(&mut event_loop, "the other window listed", |_| {
    other(&heard).is_some()
  });
  source.set(CaptureSource::Toplevel(other(&heard).expect("listed")));
  run_until(&mut event_loop, "the other window's frame", |platform| {
    frame_of(platform, viewer).is_some_and(|frame| {
      (frame.width, frame.height) == (100, 100) && all_of(&frame.rgba, Color::rgb(0, 0, 0))
    })
  });
}
