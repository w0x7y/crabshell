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

use crabshell_reactive::{ReadSignal, Runtime, Signal};
use crabshell_ui::{
  Adjust, Align, Button, Color, Column, Content, Cx, Edge, Key, KeyEvent, KeyboardFocus, Margins,
  Output, PanelWindow, PopupWindow, Rect, Region, Row, ScrollView, Slider, Spacer, Style, Text,
  TextArea, TextInput, Window,
};
use crabshell_ui::{CaptureFrame, CaptureSource, FloatingWindow, ScreencopyView, Transition};
use crabshell_ui::{Layer, Lock, SessionLock};
use crabshell_wayland::{
  Event, EventLoop, LockId, OutputInfo, Platform, Selection, Toplevel, WindowId, Windowset,
  WindowsetProjection,
};
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
  /// Whether that field is a text area instead.
  area: bool,
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
  area: false,
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
  /// Whether the bar asks for the compositor's shortcuts.
  inhibit: Signal<bool>,
  /// Whether the compositor says it gives them.
  inhibited: Signal<bool>,
  /// Whether every bar holds the focus, in one grab.
  grab: Signal<bool>,
  /// How every [`menu`] may move or shrink to stay on the output.
  adjust: Signal<Adjust>,
  /// The part of every [`menu`] that takes clicks.
  menu_mask: Signal<Option<Region>>,
  /// The opacity the bar and every [`menu`] ask the compositor for.
  opacity: Signal<f32>,
  /// The part of the bar and every [`menu`] said to hold anything to see.
  visible: Signal<Option<Region>>,
}

/// A popup with two entries: the first opens another menu beside it, and
/// the second closes them all.
fn menu(knobs: Knobs) -> PopupWindow {
  PopupWindow::new()
    .adjustment(move || knobs.adjust.get())
    .mask(move || knobs.menu_mask.get())
    .compositor_opacity(move || knobs.opacity.get())
    .visible_mask(move || knobs.visible.get())
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
    .inhibit_shortcuts(move || knobs.inhibit.get())
    .shortcuts_inhibited(knobs.inhibited)
    .focus_grab(knobs.grab)
    .compositor_opacity(move || knobs.opacity.get())
    .visible_mask(move || knobs.visible.get())
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
            if knobs.setup.area {
              return Row::new().child(TextArea::new(knobs.typed).focus(true));
            }
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
  /// Each output change the listener heard, in order.
  changed: Rc<RefCell<Vec<OutputInfo>>>,
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
      inhibit: cx.signal(false),
      inhibited: cx.signal(false),
      grab: cx.signal(false),
      adjust: cx.signal(Adjust::FLIP | Adjust::SLIDE),
      menu_mask: cx.signal(None),
      opacity: cx.signal(1.0),
      visible: cx.signal(None),
    };
    let mut event_loop = sway.connect();
    let built = Rc::new(RefCell::new(Vec::new()));
    let removed = Rc::new(RefCell::new(Vec::new()));
    let changed = Rc::new(RefCell::new(Vec::new()));
    let (on_built, on_removed) = (Rc::clone(&built), Rc::clone(&removed));
    let on_changed = Rc::clone(&changed);
    event_loop
      .platform()
      .on_event(move |platform: &mut Platform, event| match event {
        Event::OutputAdded(output) => {
          let id = platform.add_window(bar(&cx, &output.name, knobs));
          on_built.borrow_mut().push((output.name.clone(), id));
        }
        Event::OutputChanged(output) => on_changed.borrow_mut().push(output.clone()),
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
      changed,
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

/// What the platform knows of `name`, if it is announced.
fn output_named(platform: &Platform, name: &str) -> Option<OutputInfo> {
  platform
    .outputs()
    .find(|output| output.name == name)
    .cloned()
}

#[test]
#[ignore = "needs headless Sway"]
fn outputs_tell_their_place_size_scale_and_turn_and_each_change() {
  let sway = Sway::start();
  sway.msg(&[
    "output",
    "HEADLESS-1",
    "mode",
    "1920x1080",
    "position",
    "0",
    "0",
    "scale",
    "1",
  ]);
  let mut shell = Shell::new(&sway);
  let first = shell.wait_for_bar("HEADLESS-1");
  let one = output_named(shell.event_loop.platform(), "HEADLESS-1").expect("announced");
  assert_eq!(one.description, "Headless output 1", "{one:?}");
  // What wlroots says for an output with no EDID.
  assert_eq!(
    (one.make.as_str(), one.model.as_str()),
    ("Unknown", "Unknown")
  );
  assert_eq!((one.x, one.y, one.width, one.height), (0, 0, 1920, 1080));
  assert_eq!((one.scale, one.fractional_scale), (1, 1.0));
  // A virtual output has no physical size.
  assert_eq!((one.width_mm, one.height_mm), (0, 0));
  assert_eq!(one.transform, crabshell_render::Turn::default());

  // A new scale is told as a change, and it is the scale the bar draws
  // at, past the whole one wl_output gives.
  sway.msg(&["output", "HEADLESS-1", "scale", "1.5"]);
  run_until(&mut shell.event_loop, "HEADLESS-1 at 1.5", |platform| {
    let drawn = platform.window_info(first).map(|info| info.scale);
    let told = output_named(platform, "HEADLESS-1").map(|output| output.fractional_scale);
    drawn == Some(1.5) && told == Some(1.5)
  });
  let one = output_named(shell.event_loop.platform(), "HEADLESS-1").expect("announced");
  assert_eq!((one.scale, one.width, one.height), (2, 1280, 720));
  assert_eq!(
    shell.changed.borrow().last(),
    Some(&one),
    "the change was told"
  );

  // Another, beside it and turned a quarter: its sides swap, and its
  // scale comes from the mode's height.
  sway.msg(&["create_output"]);
  let second = shell.wait_for_bar("HEADLESS-2");
  sway.msg(&[
    "output",
    "HEADLESS-2",
    "mode",
    "1280x720",
    "position",
    "1280",
    "100",
    "scale",
    "1.5",
    "transform",
    "90",
  ]);
  run_until(&mut shell.event_loop, "HEADLESS-2 turned", |platform| {
    let drawn = platform.window_info(second).map(|info| info.scale);
    // Sway turns 90 clockwise, which wl_output calls 270.
    let told = output_named(platform, "HEADLESS-2");
    drawn == Some(1.5) && told.is_some_and(|output| output.transform.quarters == 3)
  });
  let two = output_named(shell.event_loop.platform(), "HEADLESS-2").expect("announced");
  assert_eq!((two.x, two.y, two.width, two.height), (1280, 100, 480, 853));
  assert_eq!(
    two.mode.map(|mode| (mode.width, mode.height)),
    Some((1280, 720))
  );
  assert_eq!((two.scale, two.fractional_scale), (2, 1.5));
  assert!(!two.transform.flipped);
  // Each change told says something new.
  let changed = shell.changed.borrow().clone();
  for (name, told) in [("HEADLESS-1", &one), ("HEADLESS-2", &two)] {
    let of: Vec<&OutputInfo> = changed
      .iter()
      .filter(|output| output.name == name)
      .collect();
    assert_eq!(of.last(), Some(&told), "{name}'s last change");
    assert!(of.windows(2).all(|pair| pair[0] != pair[1]), "{of:#?}");
  }

  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  run_until(&mut shell.event_loop, "HEADLESS-2 to go", |platform| {
    output_named(platform, "HEADLESS-2").is_none()
  });
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

/// How many frames window `id` committed.
fn commits(shell: &mut Shell, id: WindowId) -> u64 {
  let info = shell.event_loop.platform().window_info(id);
  info.expect("the window is held").commits
}

/// Runs `change`, which changes only a hint of window `id`, and checks it
/// took exactly `frames` frames to commit.
fn hint_frames(shell: &mut Shell, id: WindowId, frames: u64, change: impl FnOnce()) {
  let before = commits(shell, id);
  change();
  if frames > 0 {
    run_until(&mut shell.event_loop, "a frame with the hint", |platform| {
      platform
        .window_info(id)
        .is_some_and(|info| info.commits > before)
    });
  }
  settle(&mut shell.event_loop);
  assert_eq!(commits(shell, id), before + frames);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_hint_change_rides_the_next_frame_and_an_opacity_sway_lacks_sends_nothing() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  let bar = shell.wait_for_bar("HEADLESS-1");
  let knobs = shell.knobs;
  // Not a layer-shell reconfigure, which has the compositor arrange the
  // output again, and Sway answer with a configure.
  hint_frames(&mut shell, bar, 1, || knobs.masked.set(true));
  // Sway lacks hyprland-surface-v1, so there is nothing to commit.
  hint_frames(&mut shell, bar, 0, || knobs.opacity.set(0.5));

  // A floating window's hints don't commit its toplevel either.
  let cx = Cx::new(shell.runtime.new_owner());
  let (masked, faded) = (cx.signal(false), cx.signal(1.0_f32));
  let floating = FloatingWindow::new()
    .title("crabshell-hinted")
    .mask(move || masked.get().then(|| Region::rect(0, 0, 10, 10)))
    .compositor_opacity(move || faded.get())
    .child(Text::new("hinted"));
  let platform = shell.event_loop.platform();
  let id = platform.add_window(Window::floating(&cx, floating));
  run_until(&mut shell.event_loop, "the floating window", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > 0)
  });
  settle(&mut shell.event_loop);
  hint_frames(&mut shell, id, 1, || masked.set(true));
  hint_frames(&mut shell, id, 0, || faded.set(0.5));
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

#[test]
#[ignore = "needs headless Sway"]
fn global_shortcuts_without_hyprland_are_logged_once_and_the_bar_still_draws() {
  // Sway lacks hyprland-global-shortcuts-v1.
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  let id = shell.wait_for_bar("HEADLESS-1");
  let platform = shell.event_loop.platform();
  for name in ["launcher", "lock"] {
    let watch = platform.watch_shortcut("crabshell", name, "a test shortcut", |_| {
      panic!("pressed without Hyprland")
    });
    assert_eq!(watch, None);
  }
  assert_eq!(platform.shortcut_watches(), 0);
  let before = platform.window_info(id).expect("the bar is held").commits;
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
    text.matches("lacks hyprland-global-shortcuts-v1").count(),
    1,
    "{text}"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn a_focus_grab_without_hyprland_is_logged_once_and_left_alone() {
  // Sway lacks hyprland-focus-grab-v1.
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  sway.msg(&["create_output"]);
  let first = shell.wait_for_bar("HEADLESS-1");
  shell.wait_for_bar("HEADLESS-2");
  let grab = shell.knobs.grab;
  grab.set(true);
  settle(&mut shell.event_loop);
  // Asking wakes nothing: one dispatch sleeps the whole time.
  let started = Instant::now();
  shell
    .event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  let slept = started.elapsed();
  assert!(slept >= Duration::from_millis(1900), "woke after {slept:?}");

  // One of the bars goes with its output, and the other still draws.
  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  let platform = shell.event_loop.platform();
  let before = platform.window_info(first).expect("held").commits;
  shell.knobs.label.set(String::from("still drawing"));
  run_until(&mut shell.event_loop, "a frame and one bar", |platform| {
    platform.windows().count() == 1
      && platform
        .window_info(first)
        .is_some_and(|info| info.commits > before)
  });
  assert!(grab.get_untracked(), "nothing cleared it");
  let text = logs.text();
  assert_eq!(
    text.matches("lacks hyprland-focus-grab-v1").count(),
    1,
    "{text}"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn a_compositor_opacity_and_visible_mask_without_hyprland_log_once_and_change_nothing() {
  // Sway lacks hyprland-surface-v1.
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let mut menus = Menus::start(&sway, "1.5");
  menus.shell.runtime.flush();
  let nodes = menus.shell.runtime.node_count();
  let knobs = menus.shell.knobs;
  knobs.opacity.set(0.5);
  knobs.visible.set(Some(Region::rect(0, 0, 100, 30)));
  let cx = Cx::new(menus.shell.runtime.new_owner());
  let floating = FloatingWindow::new()
    .title("crabshell-faded")
    .compositor_opacity(0.25)
    .visible_mask(Some(Region::rect(0, 0, 50, 50)))
    .child(Text::new("faded"));
  let platform = menus.shell.event_loop.platform();
  let id = platform.add_window(Window::floating(&cx, floating));
  run_until(
    &mut menus.shell.event_loop,
    "the floating window",
    |platform| {
      platform
        .window_info(id)
        .is_some_and(|info| info.commits > 0)
    },
  );
  settle(&mut menus.shell.event_loop);
  assert!(logs.text().contains("lacks hyprland-surface-v1"));
  // Sway shows the bar as drawn.
  let platform = menus.shell.event_loop.platform();
  let snapshot = platform.snapshot(menus.bar).expect("the bar draws");
  assert_matches("faded-bar@1.5", &sway.screenshot("HEADLESS-1"), &snapshot);
  let menu = menus.open_from_bar();

  // Changes, a new scale and the defaults again log nothing more, and
  // every window still draws.
  knobs.opacity.set(0.75);
  sway.msg(&["output", "HEADLESS-1", "scale", "2"]);
  settle(&mut menus.shell.event_loop);
  knobs.opacity.set(1.0);
  knobs.visible.set(None);
  let before = menus.info(menus.bar).commits;
  knobs.label.set(String::from("still drawing"));
  run_until(
    &mut menus.shell.event_loop,
    "a frame with the new label",
    |platform| {
      platform
        .window_info(menus.bar)
        .is_some_and(|info| info.commits > before)
    },
  );
  let text = logs.text();
  assert_eq!(
    text.matches("lacks hyprland-surface-v1").count(),
    1,
    "{text}"
  );

  // The menu and the floating window go, and so does all they made.
  sway.msg(&["output", "HEADLESS-1", "scale", "1.5"]);
  settle(&mut menus.shell.event_loop);
  menus.click_entry(menu, 1);
  menus.wait_for("the menu to close", <[_]>::is_empty);
  menus.shell.event_loop.platform().remove_window(id);
  cx.owner().dispose();
  settle(&mut menus.shell.event_loop);
  menus.shell.runtime.flush();
  assert_eq!(menus.shell.runtime.node_count(), nodes);
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

/// Whether an x reached the bar's `on_key`.
fn got_x(knobs: Knobs) -> bool {
  knobs
    .keys
    .with_untracked(|keys| keys.iter().any(|key| key.key == Key::Character('x')))
}

#[test]
#[ignore = "needs headless Sway"]
fn a_bar_that_inhibits_shortcuts_gets_the_keys_sway_binds() {
  let sway = Sway::start();
  sway.msg(&["bindsym", "x", "workspace", "number", "7"]);
  let (mut shell, mut keyboard) = typing(&sway);
  let knobs = shell.knobs;
  keyboard.tap(keys::X);
  settle(&mut shell.event_loop);
  assert!(!got_x(knobs), "Sway's binding let x through");
  assert!(
    sway.msg(&["-r", "-t", "get_workspaces"]).contains("\"7\""),
    "Sway's binding didn't run"
  );

  knobs.inhibit.set(true);
  run_until(&mut shell.event_loop, "Sway to honor it", |_| {
    knobs.inhibited.get_untracked()
  });
  settle(&mut shell.event_loop);
  // Inhibiting, nothing wakes the shell: one dispatch sleeps the whole
  // time.
  let started = Instant::now();
  shell
    .event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  let slept = started.elapsed();
  assert!(slept >= Duration::from_millis(1900), "woke after {slept:?}");
  keyboard.tap(keys::X);
  run_until(&mut shell.event_loop, "x at the bar", |_| got_x(knobs));

  // Asked no more, Sway takes x again.
  knobs.inhibit.set(false);
  run_until(&mut shell.event_loop, "the inhibitor gone", |_| {
    !knobs.inhibited.get_untracked()
  });
  let typed = knobs.keys.with_untracked(Vec::len);
  keyboard.tap(keys::X);
  settle(&mut shell.event_loop);
  assert_eq!(knobs.keys.with_untracked(Vec::len), typed);
}

#[test]
#[ignore = "needs headless Sway"]
fn shortcuts_inhibited_follows_sway_and_turns_false_as_the_bar_goes() {
  let sway = Sway::start();
  let (mut shell, _keyboard) = typing(&sway);
  let knobs = shell.knobs;
  knobs.inhibit.set(true);
  run_until(&mut shell.event_loop, "Sway to honor it", |_| {
    knobs.inhibited.get_untracked()
  });
  sway.msg(&["seat", "-", "shortcuts_inhibitor", "deactivate"]);
  run_until(&mut shell.event_loop, "Sway taking them back", |_| {
    !knobs.inhibited.get_untracked()
  });
  sway.msg(&["seat", "-", "shortcuts_inhibitor", "activate"]);
  run_until(&mut shell.event_loop, "Sway giving them again", |_| {
    knobs.inhibited.get_untracked()
  });

  // The output goes, and the bar's inhibitor with it.
  sway.msg(&["output", "HEADLESS-1", "unplug"]);
  run_until(&mut shell.event_loop, "the bar gone", |platform| {
    platform.windows().next().is_none()
  });
  assert!(!knobs.inhibited.get_untracked());
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
#[ignore = "needs headless Sway"]
fn a_text_area_takes_keys_and_input_method_text_and_then_sleeps() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut keyboard = VirtualKeyboard::new(&sway);
  let mut method = InputMethod::new(&sway);
  let setup = Setup {
    keyboard: KeyboardFocus::Exclusive,
    field: true,
    area: true,
    ..PLAIN
  };
  let mut shell = Shell::build(&sway, setup);
  shell.wait_for_bar("HEADLESS-1");
  let typed = shell.knobs.typed;
  for key in [keys::H, keys::ENTER, keys::I] {
    keyboard.tap(key);
  }
  run_until(&mut shell.event_loop, "two typed lines", |_| {
    typed.get_untracked() == "h\ni"
  });
  run_until(
    &mut shell.event_loop,
    "the input method to activate",
    |_| {
      method.poll();
      method.active()
    },
  );
  method.commit("你");
  run_until(&mut shell.event_loop, "the committed text", |_| {
    typed.get_untracked() == "h\ni你"
  });
  // The input method is told the text with its line break.
  run_until(&mut shell.event_loop, "the surrounding text", |_| {
    method.poll();
    method.surrounding() == "h\ni你"
  });
  // A focused area wakes nothing while nothing happens. One event comes
  // after the input method's last change, as it does for a one-line
  // field, and then the loop sleeps.
  settle(&mut shell.event_loop);
  shell
    .event_loop
    .dispatch(Some(Duration::from_secs(1)))
    .expect("the connection works");
  sleeps_through(&mut shell.event_loop, Duration::from_secs(2));
}

/// The rectangle of the first text field in the bar.
fn field_rect(node: crabshell_ui::NodeView<'_>) -> Option<crabshell_ui::Rect> {
  match node.content() {
    Content::Input(_) => Some(node.rect()),
    _ => node.children().find_map(field_rect),
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn dragging_selects_in_a_field_past_the_bar_and_shift_click_extends() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut keyboard = VirtualKeyboard::new(&sway);
  let mut pointer = VirtualPointer::new(&sway);
  let setup = Setup {
    keyboard: KeyboardFocus::Exclusive,
    field: true,
    ..PLAIN
  };
  let mut shell = Shell::build(&sway, setup);
  let id = shell.wait_for_bar("HEADLESS-1");
  shell.knobs.typed.set(String::from("hello"));
  settle(&mut shell.event_loop);
  let window = |platform: &Platform| {
    platform
      .windows()
      .find(|(held, _)| *held == id)
      .map(|(_, window)| (field_rect(window.view().root()), window.field()))
  };
  let (rect, _) = window(shell.event_loop.platform()).expect("the bar");
  let rect = rect.expect("a field");
  let (x, y) = (rect.x as u32 + 1, (rect.y + rect.height / 2.0) as u32);
  pointer.move_to(x, y, (1920, 1080));
  settle(&mut shell.event_loop);
  pointer.press(VirtualPointer::LEFT);
  // Far below the bar: the press holds the pointer, so the bar still
  // hears where it goes.
  pointer.move_to(1900, 900, (1920, 1080));
  pointer.release(VirtualPointer::LEFT);
  run_until(
    &mut shell.event_loop,
    "the whole text selected",
    |platform| {
      window(platform)
        .and_then(|(_, field)| field)
        .is_some_and(|field| (field.anchor, field.cursor) == (0, 5))
    },
  );

  // A click inside places the caret; Shift and a click at the end
  // extend from there. Slower than a double click, which picks a word.
  std::thread::sleep(Duration::from_millis(450));
  pointer.move_to(x, y, (1920, 1080));
  pointer.press(VirtualPointer::LEFT);
  pointer.release(VirtualPointer::LEFT);
  run_until(
    &mut shell.event_loop,
    "the caret at the start",
    |platform| {
      window(platform)
        .and_then(|(_, field)| field)
        .is_some_and(|field| (field.anchor, field.cursor) == (0, 0))
    },
  );
  keyboard.shift(true);
  pointer.move_to(rect.right() as u32 - 1, y, (1920, 1080));
  pointer.press(VirtualPointer::LEFT);
  pointer.release(VirtualPointer::LEFT);
  keyboard.shift(false);
  run_until(
    &mut shell.event_loop,
    "the selection extended",
    |platform| {
      window(platform)
        .and_then(|(_, field)| field)
        .is_some_and(|field| (field.anchor, field.cursor) == (0, 5))
    },
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn shift_released_while_the_keyboard_is_elsewhere_doesnt_extend_a_click() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut keyboard = VirtualKeyboard::new(&sway);
  let mut pointer = VirtualPointer::new(&sway);
  let setup = Setup {
    keyboard: KeyboardFocus::Exclusive,
    field: true,
    ..PLAIN
  };
  let mut shell = Shell::build(&sway, setup);
  let id = shell.wait_for_bar("HEADLESS-1");
  let knobs = shell.knobs;
  knobs.typed.set(String::from("hello"));
  settle(&mut shell.event_loop);
  let field = |platform: &Platform| {
    platform
      .windows()
      .find(|(held, _)| *held == id)
      .and_then(|(_, window)| window.field())
  };
  let rect = shell
    .event_loop
    .platform()
    .windows()
    .find(|(held, _)| *held == id)
    .and_then(|(_, window)| field_rect(window.view().root()))
    .expect("a field");
  let y = (rect.y + rect.height / 2.0) as u32;
  let mut click = |x: u32| {
    // Slower than a double click, which picks a word.
    std::thread::sleep(Duration::from_millis(450));
    pointer.move_to(x, y, (1920, 1080));
    pointer.press(VirtualPointer::LEFT);
    pointer.release(VirtualPointer::LEFT);
  };
  click(rect.x as u32 + 1);
  run_until(
    &mut shell.event_loop,
    "the caret at the start",
    |platform| field(platform).is_some_and(|field| (field.anchor, field.cursor) == (0, 0)),
  );
  keyboard.shift(true);
  settle(&mut shell.event_loop);
  // The keyboard goes to another program, and Shift comes up there.
  knobs.keyboard.set(KeyboardFocus::None);
  settle(&mut shell.event_loop);
  let app = AppWindow::new(&sway, "org.example.App", "another program");
  run_until(&mut shell.event_loop, "the keyboard to leave", |platform| {
    field(platform).is_none()
  });
  keyboard.shift(false);
  click(rect.right() as u32 - 1);
  settle(&mut shell.event_loop);
  drop(app);
  knobs.keyboard.set(KeyboardFocus::Exclusive);
  run_until(&mut shell.event_loop, "the keyboard back", |platform| {
    field(platform).is_some()
  });
  let state = field(shell.event_loop.platform()).expect("the field");
  assert_eq!((state.anchor, state.cursor), (5, 5), "a plain click");
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

/// What `command` prints once it exits, the loop running meanwhile, since
/// a reader waits for the shell to write.
fn output_of(shell: &mut Shell, mut command: Command) -> String {
  let child = command
    .stdout(std::process::Stdio::piped())
    .spawn()
    .expect("the client runs");
  let child = Rc::new(RefCell::new(Some(child)));
  let output = Rc::new(RefCell::new(None));
  let (held, out) = (Rc::clone(&child), Rc::clone(&output));
  run_until(&mut shell.event_loop, "the client to finish", move |_| {
    let mut held = held.borrow_mut();
    match held.as_mut().map(std::process::Child::try_wait) {
      Some(Ok(Some(_))) => {
        let done = held.take().expect("still held");
        *out.borrow_mut() = Some(done.wait_with_output().expect("its output"));
        true
      }
      _ => false,
    }
  });
  let output = output.borrow_mut().take().expect("the client finished");
  String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
#[ignore = "needs headless Sway and wl-clipboard"]
fn a_drag_selection_becomes_the_primary_selection_and_a_middle_click_pastes_it() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let _keyboard = VirtualKeyboard::new(&sway);
  let mut pointer = VirtualPointer::new(&sway);
  let setup = Setup {
    keyboard: KeyboardFocus::Exclusive,
    field: true,
    ..PLAIN
  };
  let mut shell = Shell::build(&sway, setup);
  let id = shell.wait_for_bar("HEADLESS-1");
  let typed = shell.knobs.typed;
  typed.set(String::from("hello"));
  settle(&mut shell.event_loop);
  let rect = shell
    .event_loop
    .platform()
    .windows()
    .find(|(held, _)| *held == id)
    .and_then(|(_, window)| field_rect(window.view().root()))
    .expect("a field");
  let (x, y) = (rect.x as u32 + 1, (rect.y + rect.height / 2.0) as u32);
  pointer.move_to(x, y, (1920, 1080));
  settle(&mut shell.event_loop);
  pointer.press(VirtualPointer::LEFT);
  pointer.move_to(1900, y, (1920, 1080));
  pointer.release(VirtualPointer::LEFT);
  settle(&mut shell.event_loop);
  let mut paste = sway.client("wl-paste");
  paste.args(["--primary", "--no-newline"]);
  assert_eq!(output_of(&mut shell, paste), "hello");

  let mut copy = sway
    .client("wl-copy")
    .args(["--primary", "--foreground", "XY"])
    .spawn()
    .expect("wl-copy runs");
  // The shell hears of wl-copy's offer in its own time: until then, a
  // middle click pastes what the field offered, so the text is reset.
  let mut last = Instant::now() - Duration::from_secs(1);
  run_until(&mut shell.event_loop, "the middle-click paste", |_| {
    if typed.get_untracked() != "XYhello" && last.elapsed() > Duration::from_millis(300) {
      last = Instant::now();
      typed.set(String::from("hello"));
      pointer.move_to(x, y, (1920, 1080));
      pointer.press(VirtualPointer::MIDDLE);
      pointer.release(VirtualPointer::MIDDLE);
    }
    typed.get_untracked() == "XYhello"
  });
  let _ = copy.kill();
  let _ = copy.wait();
}

/// Follows the clipboard, keeping each selection heard.
fn watch_clipboard(shell: &mut Shell) -> Rc<RefCell<Vec<Selection>>> {
  let heard = Rc::new(RefCell::new(Vec::new()));
  let keep = Rc::clone(&heard);
  let platform = shell.event_loop.platform();
  platform.watch_clipboard(move |selection| keep.borrow_mut().push(selection.clone()));
  heard
}

/// `wl-copy` offering `bytes` as `kind` until killed.
fn wl_copy(sway: &Sway, kind: &str, bytes: &[u8]) -> std::process::Child {
  use std::io::Write;
  let mut child = sway
    .client("wl-copy")
    .args(["--foreground", "--type", kind])
    .stdin(std::process::Stdio::piped())
    .spawn()
    .expect("wl-copy runs");
  let mut stdin = child.stdin.take().expect("its stdin");
  stdin.write_all(bytes).expect("wl-copy reads its stdin");
  child
}

fn stop(mut child: std::process::Child) {
  let _ = child.kill();
  let _ = child.wait();
}

#[test]
#[ignore = "needs headless Sway and wl-clipboard"]
fn the_clipboard_watch_follows_wl_copy_without_the_keyboard_and_sets_what_wl_paste_reads() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let heard = watch_clipboard(&mut shell);
  let last = move || heard.borrow().last().cloned().unwrap_or_default();
  let copy = wl_copy(&sway, "text/plain;charset=utf-8", b"first");
  run_until(&mut shell.event_loop, "the copied text", |_| {
    last().text.as_deref() == Some("first")
  });
  stop(copy);

  shell
    .event_loop
    .platform()
    .set_clipboard_text(String::from("from the shell"));
  let mut paste = sway.client("wl-paste");
  paste.arg("--no-newline");
  assert_eq!(output_of(&mut shell, paste), "from the shell");
  run_until(&mut shell.event_loop, "its own text heard", |_| {
    last().text.as_deref() == Some("from the shell")
  });
}

#[test]
#[ignore = "needs headless Sway and wl-clipboard"]
fn the_clipboard_watch_reads_images_when_asked_and_refuses_what_is_too_large() {
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let heard = watch_clipboard(&mut shell);
  let last = || heard.borrow().last().cloned().unwrap_or_default();
  // Each copy takes the clipboard from the one before, so the clipboard
  // is never empty between them.
  let mut copies = Vec::new();

  copies.push(wl_copy(&sway, "text/plain", b"before"));
  run_until(&mut shell.event_loop, "the text", |_| {
    last().text.as_deref() == Some("before")
  });
  let png = b"\x89PNG not really".to_vec();
  copies.push(wl_copy(&sway, "image/png", &png));
  run_until(
    &mut shell.event_loop,
    "an image while images are off",
    |_| last() == Selection::default(),
  );

  shell.event_loop.platform().read_clipboard_images(true);
  copies.push(wl_copy(&sway, "image/png", &png));
  run_until(&mut shell.event_loop, "the image", |_| {
    last().image == Some((String::from("image/png"), png.clone()))
  });

  copies.push(wl_copy(&sway, "image/png", &vec![7; 8 * 1024 * 1024 + 1]));
  run_until(&mut shell.event_loop, "an image too large", |_| {
    logs.text().contains("an image over 8 MiB")
  });
  copies.push(wl_copy(
    &sway,
    "text/plain",
    &vec![b'a'; 4 * 1024 * 1024 + 1],
  ));
  run_until(&mut shell.event_loop, "text too large", |_| {
    logs.text().contains("over 4 MiB of text")
  });
  let largest = heard.borrow().iter().fold(0, |most, selection| {
    let text = selection.text.as_ref().map_or(0, String::len);
    let image = selection.image.as_ref().map_or(0, |(_, bytes)| bytes.len());
    most.max(text).max(image)
  });
  assert_eq!(largest, png.len(), "nothing too large was heard");
  copies.into_iter().for_each(stop);
}

#[test]
#[ignore = "needs headless Sway"]
fn the_clipboard_watch_hears_a_password_managers_hint() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let heard = watch_clipboard(&mut shell);
  // Another client offers a secret, as a password manager does.
  let mut manager = sway.connect();
  manager.platform().watch_clipboard(|_| {});
  manager.platform().set_clipboard(
    &["text/plain;charset=utf-8", "x-kde-passwordManagerHint"],
    b"hunter2".to_vec(),
  );
  let expected = Selection {
    text: Some(String::from("hunter2")),
    image: None,
    secret: true,
  };
  run_until(&mut shell.event_loop, "the secret", |_| {
    manager
      .dispatch(Some(Duration::ZERO))
      .expect("the connection works");
    heard.borrow().last() == Some(&expected)
  });
}

#[test]
#[ignore = "needs headless Sway and wl-clipboard"]
fn a_clipboard_watch_that_panics_is_logged_and_kept() {
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let heard = Rc::new(RefCell::new(Vec::new()));
  let keep = Rc::clone(&heard);
  shell
    .event_loop
    .platform()
    .watch_clipboard(move |selection| {
      keep.borrow_mut().push(selection.text.clone());
      panic!("clipboard panic");
    });
  let first = wl_copy(&sway, "text/plain", b"one");
  run_until(&mut shell.event_loop, "the panic logged", |_| {
    logs
      .text()
      .contains("a clipboard watch panicked: clipboard panic")
  });
  let second = wl_copy(&sway, "text/plain", b"two");
  run_until(&mut shell.event_loop, "the watch kept", |_| {
    heard.borrow().last() == Some(&Some(String::from("two")))
  });
  stop(first);
  stop(second);
}

#[test]
#[ignore = "needs headless Sway and wl-clipboard"]
fn the_clipboard_wakes_nothing_while_it_doesnt_change_or_while_nothing_follows_it() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  settle(&mut shell.event_loop);
  // Nothing follows the clipboard, so a copy wakes nothing.
  let copy = wl_copy(&sway, "text/plain", b"unheard");
  let asleep = |shell: &mut Shell| {
    let started = Instant::now();
    shell
      .event_loop
      .dispatch(Some(Duration::from_secs(2)))
      .expect("the connection works");
    started.elapsed()
  };
  let slept = asleep(&mut shell);
  assert!(slept >= Duration::from_millis(1900), "woke after {slept:?}");
  stop(copy);

  let heard = watch_clipboard(&mut shell);
  let copy = wl_copy(&sway, "text/plain", b"heard");
  run_until(&mut shell.event_loop, "the copy", |_| {
    heard.borrow().last().and_then(|last| last.text.as_deref()) == Some("heard")
  });
  settle(&mut shell.event_loop);
  let count = heard.borrow().len();
  let slept = asleep(&mut shell);
  assert!(slept >= Duration::from_millis(1900), "woke after {slept:?}");
  assert_eq!(heard.borrow().len(), count);
  stop(copy);
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

/// Adds a panel covering HEADLESS-1 in the bottom layer, below every
/// other window, and waits for it. Returns how many clicks reached it.
fn add_floor(shell: &mut Shell, cx: &Cx) -> Signal<u32> {
  let clicks = cx.signal(0);
  let floor = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Bottom | Edge::Left | Edge::Right)
    .layer(Layer::Bottom)
    .exclusive_zone(-1)
    .child(Button::new(Text::new("floor")).on_click(move |_| clicks.update(|count| *count += 1)));
  let id = shell
    .event_loop
    .platform()
    .add_window(Window::new(cx, floor));
  run_until(&mut shell.event_loop, "the floor", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > 0)
  });
  clicks
}

#[test]
#[ignore = "needs headless Sway"]
fn a_floating_windows_mask_lets_clicks_through_to_what_is_below() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut pointer = VirtualPointer::new(&sway);
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let cx = Cx::new(shell.runtime.new_owner());
  let below = add_floor(&mut shell, &cx);
  let (masked, clicks) = (cx.signal(true), cx.signal(0));
  let floating = FloatingWindow::new()
    .title("crabshell-masked")
    // The window fills the workspace; only its left 400 pixels take
    // clicks.
    .mask(move || masked.get().then(|| Region::rect(0, 0, 400, 100_000)))
    .child(Button::new(Text::new("window")).on_click(move |_| clicks.update(|count| *count += 1)));
  let id = shell
    .event_loop
    .platform()
    .add_window(Window::floating(&cx, floating));
  run_until(&mut shell.event_loop, "the floating window", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > 0)
  });
  pointer.move_to(960, 600, (1920, 1080));
  settle(&mut shell.event_loop);
  // A virtual pointer's clicks never reached a toplevel on Sway.
  let click = |x: u32| {
    sway.msg(&["seat", "-", "cursor", "set", &x.to_string(), "600"]);
    sway.msg(&["seat", "-", "cursor", "press", "button1"]);
    sway.msg(&["seat", "-", "cursor", "release", "button1"]);
  };
  click(1500);
  run_until(&mut shell.event_loop, "a click outside the mask", |_| {
    below.get_untracked() == 1
  });
  click(100);
  run_until(&mut shell.event_loop, "a click inside the mask", |_| {
    clicks.get_untracked() == 1
  });

  // Without a mask the whole window takes clicks, and with one again
  // they pass.
  masked.set(false);
  settle(&mut shell.event_loop);
  click(1500);
  run_until(&mut shell.event_loop, "a click once unmasked", |_| {
    clicks.get_untracked() == 2
  });
  masked.set(true);
  settle(&mut shell.event_loop);
  click(1500);
  run_until(&mut shell.event_loop, "a click once masked again", |_| {
    below.get_untracked() == 2
  });
  assert_eq!(clicks.get_untracked(), 2);
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

/// The menu the bar's button opens, wider than the output, as `adjust`
/// lets it fit, and the size its content wants.
fn wide_menu(menus: &mut Menus, adjust: Adjust) -> (WindowId, (u32, u32)) {
  menus.shell.knobs.adjust.set(adjust);
  // About 3300 logical pixels: wider than the output at scale 1, and
  // inside the largest buffer at scale 2.
  menus.shell.knobs.entry.set("wide ".repeat(100));
  let popup = menus.open_from_bar();
  let (_, window) = menus
    .shell
    .event_loop
    .platform()
    .windows()
    .find(|(id, _)| *id == popup)
    .expect("held");
  let wanted = window.wanted_size();
  (popup, (wanted.width as u32, wanted.height as u32))
}

#[test]
#[ignore = "needs headless Sway"]
fn a_menu_too_wide_for_the_output_is_drawn_at_the_width_sway_resizes_it_to() {
  for scale in ["1", "1.25", "1.5", "1.75", "2"] {
    let sway = Sway::start();
    let mut menus = Menus::start(&sway, scale);
    let (popup, wanted) = wide_menu(&mut menus, Adjust::RESIZE);
    let info = menus.info(popup);
    let (x, y) = info.position.expect("placed");
    // Not slid: cut at the output's right edge, at its first frame.
    let width = menus.layout.0 - x as u32;
    assert!(wanted.0 > width, "{wanted:?} at {scale}");
    assert_eq!(info.size, Some((width, wanted.1)), "at {scale}");
    assert_eq!(info.commits, 1, "at {scale}");
    let factor: f64 = scale.parse().expect("a scale");
    let pixels = |side: u32| (f64::from(side) * factor).round() as u32;
    assert_eq!(
      info.buffer_size,
      Some((pixels(width), pixels(wanted.1))),
      "at {scale}"
    );
    // At fractional scales the compositor rounds the place to pixels.
    if scale == "1" || scale == "2" {
      let snapshot = menus
        .shell
        .event_loop
        .platform()
        .snapshot(popup)
        .expect("the menu draws offscreen");
      assert_matches_at(
        &format!("resized menu@{scale}"),
        &sway.screenshot("HEADLESS-1"),
        &snapshot,
        (pixels(x as u32), pixels(y as u32)),
      );
    }
  }

  // Without resizing it slides to the output's left edge and keeps its
  // size, most of it off the output.
  let sway = Sway::start();
  let mut menus = Menus::start(&sway, "1");
  let (popup, wanted) = wide_menu(&mut menus, Adjust::FLIP | Adjust::SLIDE);
  let info = menus.info(popup);
  assert_eq!(info.position.map(|(x, _)| x), Some(0));
  assert_eq!(info.size, Some(wanted));
  assert_eq!(info.buffer_size, Some(wanted));
}

#[test]
#[ignore = "needs headless Sway"]
fn a_menus_mask_lets_clicks_through_to_the_shells_own_window_below() {
  // The mask is in logical pixels, whatever the scale.
  for scale in ["1", "1.5", "2"] {
    let sway = Sway::start();
    let mut menus = Menus::start(&sway, scale);
    let cx = Cx::new(menus.shell.runtime.new_owner());
    let below = add_floor(&mut menus.shell, &cx);
    let menu = menus.open_from_bar();
    // Only the first entry's rows take clicks, as wide as the menu gets.
    let first = entry_rect(menus.shell.event_loop.platform(), menu, 0);
    let rows = (first.y + first.height).ceil() as u32;
    menus
      .shell
      .knobs
      .menu_mask
      .set(Some(Region::rect(0, 0, 100_000, rows)));
    settle(&mut menus.shell.event_loop);
    // `close` is outside the mask: the floor gets the click, and the menu
    // stays.
    menus.click_entry(menu, 1);
    run_until(
      &mut menus.shell.event_loop,
      "a click outside the mask",
      |_| below.get_untracked() == 1,
    );
    settle(&mut menus.shell.event_loop);
    assert_eq!(
      popups(menus.shell.event_loop.platform()),
      [menu],
      "at {scale}"
    );

    // Grown wider, the menu's new part takes clicks too: the first entry's
    // far end opens a submenu.
    let narrow = menus.info(menu).size.expect("sized").0;
    menus
      .shell
      .knobs
      .entry
      .set(String::from("a much longer first entry"));
    run_until(&mut menus.shell.event_loop, "a wider menu", |platform| {
      platform
        .window_info(menu)
        .and_then(|info| info.size)
        .is_some_and(|(width, _)| width > narrow + 20)
    });
    settle(&mut menus.shell.event_loop);
    let platform = menus.shell.event_loop.platform();
    let first = entry_rect(platform, menu, 0);
    let (x, y) = origin(platform, menu);
    menus.click((
      x + first.x + first.width - 5.0,
      y + first.y + first.height / 2.0,
    ));
    menus.wait_for("a submenu", |open| open.len() == 2);

    // Without a mask, `close` takes the click.
    menus.shell.knobs.menu_mask.set(None);
    settle(&mut menus.shell.event_loop);
    menus.click_entry(menu, 1);
    menus.wait_for("every menu to close", <[_]>::is_empty);
    assert_eq!(below.get_untracked(), 1, "at {scale}");
  }
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

/// What a windowset watch heard: the last lists, and how many came.
type HeardSets = Rc<RefCell<(Vec<Windowset>, Vec<WindowsetProjection>, u32)>>;

fn watch_windowsets(shell: &mut Shell) -> HeardSets {
  let heard = HeardSets::default();
  let log = Rc::clone(&heard);
  shell
    .event_loop
    .platform()
    .watch_windowsets(move |sets, projections| {
      let mut log = log.borrow_mut();
      *log = (sets.to_vec(), projections.to_vec(), log.2 + 1);
    });
  heard
}

/// The windowset named `name` in the last list heard.
fn named(heard: &HeardSets, name: &str) -> Option<Windowset> {
  let heard = heard.borrow();
  heard.0.iter().find(|set| set.name == name).cloned()
}

/// The projection shown on `output` in the last list heard.
fn projection_on(heard: &HeardSets, output: &str) -> Option<WindowsetProjection> {
  let heard = heard.borrow();
  let mut found = heard
    .1
    .iter()
    .filter(|projection| projection.outputs == [output]);
  found.next().cloned()
}

#[test]
#[ignore = "needs headless Sway"]
fn sway_lists_each_outputs_windowsets_and_follows_its_outputs() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let heard = watch_windowsets(&mut shell);
  run_until(&mut shell.event_loop, "HEADLESS-1's windowset", |_| {
    let first = projection_on(&heard, "HEADLESS-1").map(|projection| projection.id);
    first.is_some() && named(&heard, "1").is_some_and(|set| set.active && set.projection == first)
  });
  // Sway creates, activates and moves workspaces, and can't deactivate
  // or remove them.
  let first = projection_on(&heard, "HEADLESS-1").expect("listed");
  let one = named(&heard, "1").expect("listed");
  assert!(first.can_create);
  assert!(one.can_activate && one.can_assign && !one.can_deactivate && !one.can_remove);
  assert_ne!(one.id, first.id, "ids name one thing each");

  sway.msg(&["create_output"]);
  run_until(&mut shell.event_loop, "HEADLESS-2's windowset", |_| {
    let second = projection_on(&heard, "HEADLESS-2").map(|projection| projection.id);
    second.is_some() && named(&heard, "2").is_some_and(|set| set.projection == second)
  });
  let second = projection_on(&heard, "HEADLESS-2").expect("listed");
  // Announced again under the same name, an output gets a new projection.
  sway.msg(&["output", "HEADLESS-2", "disable"]);
  run_until(
    &mut shell.event_loop,
    "HEADLESS-2's projection gone",
    |_| projection_on(&heard, "HEADLESS-2").is_none() && named(&heard, "2").is_none(),
  );
  sway.msg(&["output", "HEADLESS-2", "enable"]);
  run_until(
    &mut shell.event_loop,
    "HEADLESS-2's projection back",
    |_| {
      let again = projection_on(&heard, "HEADLESS-2").filter(|again| again.id != second.id);
      let again = again.map(|again| again.id);
      again.is_some() && named(&heard, "2").is_some_and(|set| set.projection == again)
    },
  );
  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  run_until(
    &mut shell.event_loop,
    "only HEADLESS-1's projection",
    |_| heard.borrow().1 == [first.clone()],
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn windowset_requests_reach_sway() {
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let heard = watch_windowsets(&mut shell);
  // A window keeps workspace 1 once another shows.
  let _app = AppWindow::new(&sway, "org.example.App", "another program");
  sway.msg(&["workspace", "two"]);
  run_until(&mut shell.event_loop, "workspace two", |_| {
    named(&heard, "two").is_some_and(|set| set.active)
      && named(&heard, "1").is_some_and(|set| !set.active)
  });
  let one = named(&heard, "1").expect("listed").id;
  let first = projection_on(&heard, "HEADLESS-1").expect("listed").id;

  shell.event_loop.platform().activate_windowset(one);
  // Sway deletes the empty workspace it left.
  run_until(&mut shell.event_loop, "workspace 1 shown", |_| {
    named(&heard, "1").is_some_and(|set| set.active) && named(&heard, "two").is_none()
  });
  shell
    .event_loop
    .platform()
    .create_windowset(first, String::from("made"));
  run_until(&mut shell.event_loop, "a new workspace", |_| {
    named(&heard, "made").is_some_and(|set| set.active && set.projection == Some(first))
  });
  sway.msg(&["create_output"]);
  run_until(&mut shell.event_loop, "HEADLESS-2's projection", |_| {
    projection_on(&heard, "HEADLESS-2").is_some()
  });
  let second = projection_on(&heard, "HEADLESS-2").expect("listed").id;
  shell.event_loop.platform().assign_windowset(one, second);
  run_until(&mut shell.event_loop, "workspace 1 on HEADLESS-2", |_| {
    named(&heard, "1").is_some_and(|set| set.projection == Some(second))
  });

  // Unknown ids, and a windowset's id where a projection's goes, are
  // ignored; names the protocol can't carry are refused and logged.
  let lists = heard.borrow().2;
  let platform = shell.event_loop.platform();
  platform.activate_windowset(u64::MAX);
  platform.create_windowset(one, String::from("nowhere"));
  platform.assign_windowset(one, one);
  platform.create_windowset(first, String::from("nul\0inside"));
  platform.create_windowset(first, "long".repeat(1001));
  settle(&mut shell.event_loop);
  assert_eq!(
    heard.borrow().2,
    lists,
    "an ignored request changed something"
  );
  let refused = "a windowset's name must be at most 4000 bytes and hold no NUL";
  assert_eq!(logs.text().matches(refused).count(), 2, "{}", logs.text());
}

#[test]
#[ignore = "needs headless Sway"]
fn nothing_hears_of_windowsets_until_a_watch_asks() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  settle(&mut shell.event_loop);
  let wakeups = shell.event_loop.platform().stats().wayland_wakeups;
  sway.msg(&["workspace", "two"]);
  let until = Instant::now() + Duration::from_millis(500);
  while Instant::now() < until {
    shell
      .event_loop
      .dispatch(Some(Duration::from_millis(100)))
      .expect("the connection works");
  }
  assert_eq!(shell.event_loop.platform().stats().wayland_wakeups, wakeups);

  let heard = watch_windowsets(&mut shell);
  run_until(&mut shell.event_loop, "workspace two", |_| {
    named(&heard, "two").is_some_and(|set| set.active)
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn a_windowset_watch_wakes_nothing_while_nothing_changes() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let heard = watch_windowsets(&mut shell);
  run_until(&mut shell.event_loop, "workspace 1", |_| {
    named(&heard, "1").is_some()
  });
  settle(&mut shell.event_loop);
  let lists = heard.borrow().2;
  // One dispatch is one iteration however soon it wakes, so it is timed.
  let started = Instant::now();
  shell
    .event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  let slept = started.elapsed();
  assert!(slept >= Duration::from_millis(1900), "woke after {slept:?}");
  assert_eq!(heard.borrow().2, lists, "a list came with nothing changed");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_windowset_watch_that_panics_is_logged_and_the_shell_goes_on() {
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  let id = shell.wait_for_bar("HEADLESS-1");
  shell
    .event_loop
    .platform()
    .watch_windowsets(|_, _| panic!("windowset panic"));
  run_until(&mut shell.event_loop, "the panic logged", |_| {
    logs
      .text()
      .contains("a windowset watch panicked: windowset panic")
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
  // blue square shows as it is, not scaled up.
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
      (frame.width, frame.height) == (100, 100) && all_of(&frame.rgba, Color::rgb(0x2e, 0x6b, 0xc8))
    })
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn a_view_of_a_turned_output_shows_it_upright() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  sway.msg(&["create_output"]);
  sway.msg(&[
    "output",
    "HEADLESS-2",
    "mode",
    "640x360",
    "scale",
    "1",
    "transform",
    "90",
  ]);
  let cx = Cx::new(Runtime::current().new_owner());
  let mut event_loop = sway.connect();
  // Turned, the output is 360 wide and 640 tall: red left, blue right.
  let half = |color: Color| Row::new().style(Style::new().background(color).width(180.0));
  let fill = PanelWindow::new(&Output::new("HEADLESS-2"))
    .anchors(Edge::Top | Edge::Bottom | Edge::Left | Edge::Right)
    .child(Row::new().child(half(RED)).child(half(BLUE)));
  event_loop.platform().add_window(Window::new(&cx, fill));
  wait_for_color(&sway, &mut event_loop, "HEADLESS-2", (0, 0, 180, 640), RED);
  let viewer = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Left)
    .width(180)
    .height(320)
    .child(
      Row::new().child(
        ScreencopyView::new(CaptureSource::Output(String::from("HEADLESS-2")))
          .style(Style::new().width(180.0).height(320.0)),
      ),
    );
  let viewer = event_loop.platform().add_window(Window::new(&cx, viewer));
  run_until(&mut event_loop, "the view's frame", |platform| {
    frame_of(platform, viewer).is_some()
  });
  let frame = frame_of(event_loop.platform(), viewer).expect("a frame");
  assert!(
    frame.height > frame.width,
    "upright, {}x{}",
    frame.width,
    frame.height
  );
  let at = |x: u32, y: u32| {
    let from = ((y * frame.width + x) * 4) as usize;
    frame.rgba[from..from + 4].to_vec()
  };
  let (middle, last) = (frame.height / 2, frame.width - 1);
  assert_eq!(at(0, middle)[..3], [RED.r, RED.g, RED.b], "red at the left");
  assert_eq!(
    at(last, middle)[..3],
    [BLUE.r, BLUE.g, BLUE.b],
    "blue at the right"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn a_one_shot_view_that_grows_captures_again_at_its_new_size() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  sway.msg(&["create_output"]);
  sway.msg(&["output", "HEADLESS-2", "mode", "640x360", "scale", "1"]);
  let cx = Cx::new(Runtime::current().new_owner());
  let mut event_loop = sway.connect();
  let fill = PanelWindow::new(&Output::new("HEADLESS-2"))
    .anchors(Edge::Top | Edge::Bottom | Edge::Left | Edge::Right)
    .style(Style::new().background(RED));
  event_loop.platform().add_window(Window::new(&cx, fill));
  wait_for_color(&sway, &mut event_loop, "HEADLESS-2", (0, 0, 640, 360), RED);
  let wide = cx.signal(32.0_f32);
  let viewer = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Left)
    .width(VIEW.0)
    .height(VIEW.1)
    .child(Row::new().child(
      ScreencopyView::new(CaptureSource::Output(String::from("HEADLESS-2"))).style(move || {
        Style::new()
          .width(wide.get())
          .height(wide.get() * 9.0 / 16.0)
      }),
    ));
  let viewer = event_loop.platform().add_window(Window::new(&cx, viewer));
  run_until(&mut event_loop, "the small frame", |platform| {
    frame_of(platform, viewer).is_some()
  });
  let small = frame_of(event_loop.platform(), viewer).expect("a frame");
  assert_eq!((small.width, small.height), (32, 18));
  wide.set(320.0);
  run_until(
    &mut event_loop,
    "a frame cut for the larger box",
    |platform| frame_of(platform, viewer).is_some_and(|frame| frame.width == 320),
  );
}

/// Every frame the views in `window` show, in the order they are drawn.
fn frames_in(platform: &Platform, window: WindowId) -> Vec<CaptureFrame> {
  fn walk(node: crabshell_ui::NodeView<'_>, out: &mut Vec<CaptureFrame>) {
    if let Content::Capture(capture) = node.content() {
      out.push(capture.frame.clone());
    }
    node.children().for_each(|child| walk(child, out));
  }
  let mut frames = Vec::new();
  if let Some((_, found)) = platform.windows().find(|(id, _)| *id == window) {
    walk(found.view().root(), &mut frames);
  }
  frames
}

#[test]
#[ignore = "needs headless Sway"]
fn views_of_one_output_share_a_session_and_one_scrolled_away_sleeps() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  sway.msg(&["create_output"]);
  sway.msg(&["output", "HEADLESS-2", "mode", "640x360", "scale", "1"]);
  let cx = Cx::new(Runtime::current().new_owner());
  let mut event_loop = sway.connect();
  let color = cx.signal(RED);
  let fill = PanelWindow::new(&Output::new("HEADLESS-2"))
    .anchors(Edge::Top | Edge::Bottom | Edge::Left | Edge::Right)
    .style(move || Style::new().background(color.get()));
  event_loop.platform().add_window(Window::new(&cx, fill));
  wait_for_color(&sway, &mut event_loop, "HEADLESS-2", (0, 0, 640, 360), RED);
  let both = cx.signal(true);
  let offset = cx.signal(0.0_f32);
  let view = || {
    ScreencopyView::new(CaptureSource::Output(String::from("HEADLESS-2")))
      .live(true)
      .style(Style::new().width(160.0).height(90.0))
  };
  // The second view sits at the top of a scroll view as tall as it, over
  // a spacer it scrolls to.
  let scrolled = ScrollView::new(
    Column::new()
      .child(view())
      .child(Spacer::new().style(Style::new().height(90.0))),
  )
  .offset(offset)
  .style(Style::new().width(160.0).height(90.0));
  let viewer = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Left)
    .width(320)
    .height(90)
    .child(
      Row::new()
        .children_keyed(
          move || if both.get() { vec![0] } else { vec![] },
          |key| *key,
          move |_, _| view(),
        )
        .child(scrolled),
    );
  let viewer = event_loop.platform().add_window(Window::new(&cx, viewer));
  run_until(&mut event_loop, "both views' frames", |platform| {
    frames_in(platform, viewer).len() == 2
  });
  assert_eq!(event_loop.platform().capture_sessions(), 1, "one shared");
  let frames = frames_in(event_loop.platform(), viewer);
  assert!(Rc::ptr_eq(&frames[0].rgba, &frames[1].rgba), "one frame");
  assert!(all_of(&frames[0].rgba, RED));
  assert_eq!((frames[0].width, frames[0].height), (160, 90));

  both.set(false);
  run_until(&mut event_loop, "the first view gone", |platform| {
    frames_in(platform, viewer).len() == 1
  });
  assert_eq!(event_loop.platform().capture_sessions(), 1, "still shown");
  offset.set(90.0);
  run_until(&mut event_loop, "the session to end", |platform| {
    platform.capture_sessions() == 0
  });
  // Out of sight, a change to its source wakes nothing.
  color.set(BLUE);
  wait_for_color(&sway, &mut event_loop, "HEADLESS-2", (0, 0, 640, 360), BLUE);
  settle(&mut event_loop);
  let iterations = event_loop.platform().stats().iterations;
  event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  assert_eq!(event_loop.platform().stats().iterations, iterations + 1);
  assert!(all_of(
    &frames_in(event_loop.platform(), viewer)[0].rgba,
    RED
  ));

  offset.set(0.0);
  run_until(&mut event_loop, "a frame of the new color", |platform| {
    let frames = frames_in(platform, viewer);
    platform.capture_sessions() == 1 && all_of(&frames[0].rgba, BLUE)
  });
  // A view that joins a session of a still source gets its last frame.
  settle(&mut event_loop);
  both.set(true);
  run_until(&mut event_loop, "the joining view's frame", |platform| {
    let frames = frames_in(platform, viewer);
    frames.len() == 2 && Rc::ptr_eq(&frames[0].rgba, &frames[1].rgba)
  });
  assert_eq!(event_loop.platform().capture_sessions(), 1);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_view_joining_a_session_stalled_on_a_hidden_window_still_follows_its_source() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  sway.msg(&["create_output"]);
  sway.msg(&["output", "HEADLESS-2", "mode", "640x360", "scale", "1"]);
  let cx = Cx::new(Runtime::current().new_owner());
  let mut event_loop = sway.connect();
  let color = cx.signal(RED);
  let fill = PanelWindow::new(&Output::new("HEADLESS-2"))
    .anchors(Edge::Top | Edge::Bottom | Edge::Left | Edge::Right)
    .style(move || Style::new().background(color.get()));
  event_loop.platform().add_window(Window::new(&cx, fill));
  wait_for_color(&sway, &mut event_loop, "HEADLESS-2", (0, 0, 640, 360), RED);
  let viewer = |layer: Layer, edge: Edge| {
    let view = ScreencopyView::new(CaptureSource::Output(String::from("HEADLESS-2")))
      .live(true)
      .style(Style::new().width(160.0).height(90.0));
    let panel = PanelWindow::new(&Output::new("HEADLESS-1"))
      .anchors(Edge::Top | edge)
      .layer(layer)
      .width(160)
      .height(90)
      .child(Row::new().child(view));
    Window::new(&cx, panel)
  };
  let shows = |platform: &Platform, window: WindowId, color: Color| {
    let frames = frames_in(platform, window);
    frames.len() == 1 && all_of(&frames[0].rgba, color)
  };
  let first = event_loop
    .platform()
    .add_window(viewer(Layer::Bottom, Edge::Left));
  run_until(&mut event_loop, "the first view's frame", |platform| {
    shows(platform, first, RED)
  });

  // A fullscreen window hides the first viewer, which then gets no frame
  // callbacks: it draws the next frame, and the one after waits for it.
  sway.msg(&["focus", "output", "HEADLESS-1"]);
  let _cover = AppWindow::new(&sway, "org.example.Cover", "cover");
  sway.msg(&["[app_id=org.example.Cover]", "fullscreen", "enable"]);
  settle(&mut event_loop);
  for next in [BLUE, GREEN] {
    color.set(next);
    run_until(&mut event_loop, "the hidden view's frame", |platform| {
      shows(platform, first, next)
    });
  }
  settle(&mut event_loop);

  // A view that joins shows the last frame, and the source's next change.
  let second = event_loop
    .platform()
    .add_window(viewer(Layer::Overlay, Edge::Right));
  run_until(&mut event_loop, "the joining view's frame", |platform| {
    shows(platform, second, GREEN)
  });
  assert_eq!(event_loop.platform().capture_sessions(), 1);
  color.set(RED);
  run_until(
    &mut event_loop,
    "the joining view's next frame",
    |platform| shows(platform, second, RED),
  );

  // With the first viewer closed, the second still follows.
  settle(&mut event_loop);
  event_loop.platform().remove_window(first);
  color.set(BLUE);
  run_until(
    &mut event_loop,
    "a frame after the first closed",
    |platform| shows(platform, second, BLUE),
  );
}

/// A window whose view moves from one source to another, while it can't
/// draw, is no longer waited for: the first source's session goes on
/// with the views left. The shown viewer turns on painting the cursor,
/// which starts a session of its own with no frame, and its view keeps
/// the frame it had, the same pixels as the new session's first; so
/// only the hidden viewer, new to it, is waited for.
#[test]
#[ignore = "needs headless Sway"]
fn a_session_stops_waiting_for_a_window_whose_view_moved_to_another_source() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  for name in ["HEADLESS-2", "HEADLESS-3"] {
    sway.msg(&["create_output"]);
    sway.msg(&["output", name, "mode", "640x360", "scale", "1"]);
  }
  let cx = Cx::new(Runtime::current().new_owner());
  let mut event_loop = sway.connect();
  let color = cx.signal(RED);
  for (name, color) in [("HEADLESS-2", color), ("HEADLESS-3", cx.signal(GREEN))] {
    let fill = PanelWindow::new(&Output::new(name))
      .anchors(Edge::Top | Edge::Bottom | Edge::Left | Edge::Right)
      .style(move || Style::new().background(color.get()));
    event_loop.platform().add_window(Window::new(&cx, fill));
    let now = color.get_untracked();
    wait_for_color(&sway, &mut event_loop, name, (0, 0, 640, 360), now);
  }
  let source = |name: &str| CaptureSource::Output(String::from(name));
  // 0: only the shown viewer shows the first source; 1: the hidden one
  // shows it too, and both paint the cursor; 2: the hidden one shows the
  // second source.
  let phase = cx.signal(0_u8);
  let tint = cx.signal(BAR);
  let hidden = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Left)
    .layer(Layer::Bottom)
    .width(160)
    .height(90)
    .style(move || Style::new().background(tint.get()))
    .child(Row::new().children_keyed(
      move || if phase.get() > 0 { vec![0] } else { vec![] },
      |key| *key,
      move |_, _| {
        let name = move || {
          if phase.get() == 1 {
            "HEADLESS-2"
          } else {
            "HEADLESS-3"
          }
        };
        ScreencopyView::new(move || source(name()))
          .live(true)
          .paint_cursors(true)
          .style(Style::new().width(80.0).height(45.0))
      },
    ));
  let hidden = event_loop.platform().add_window(Window::new(&cx, hidden));
  let shown = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Right)
    .layer(Layer::Overlay)
    .width(160)
    .height(90)
    .child(
      Row::new().child(
        ScreencopyView::new(source("HEADLESS-2"))
          .live(true)
          .paint_cursors(move || phase.get() > 0)
          .style(Style::new().width(160.0).height(90.0)),
      ),
    );
  let shown = event_loop.platform().add_window(Window::new(&cx, shown));
  let shows = |platform: &Platform, window: WindowId, color: Color| {
    let frames = frames_in(platform, window);
    frames.len() == 1 && all_of(&frames[0].rgba, color)
  };
  run_until(&mut event_loop, "the first source's frame", |platform| {
    shows(platform, shown, RED)
  });

  // A fullscreen window hides the bottom viewer, which draws once more,
  // and then gets no frame callbacks.
  sway.msg(&["focus", "output", "HEADLESS-1"]);
  let _cover = AppWindow::new(&sway, "org.example.Cover", "cover");
  sway.msg(&["[app_id=org.example.Cover]", "fullscreen", "enable"]);
  settle(&mut event_loop);
  let commits = |platform: &Platform| platform.window_info(hidden).map_or(0, |info| info.commits);
  let before = commits(event_loop.platform());
  tint.set(MAGENTA);
  run_until(
    &mut event_loop,
    "the hidden viewer's last draw",
    |platform| commits(platform) > before,
  );
  settle(&mut event_loop);

  // The new session's first frame is new only to the hidden viewer, so
  // the session waits for that one alone.
  phase.set(1);
  run_until(&mut event_loop, "the hidden viewer's frame", |platform| {
    shows(platform, hidden, RED)
  });
  settle(&mut event_loop);
  phase.set(2);
  color.set(BLUE);
  run_until(
    &mut event_loop,
    "the first source's next frame",
    |platform| shows(platform, shown, BLUE),
  );
}

/// The lock screen's background.
const MAGENTA: Color = Color::rgb(0xff, 0x00, 0xff);

/// The bar's background.
const BAR: Color = Color::rgb(0x1e, 0x1e, 0x2e);

/// What a test lock screen reads and writes.
#[derive(Clone, Copy)]
struct Locker {
  /// Its `locked`.
  on: Signal<bool>,
  /// What its text fields hold.
  typed: Signal<String>,
  /// What the compositor says.
  locked: ReadSignal<bool>,
  id: LockId,
}

/// Adds a lock screen to `event_loop`'s platform, under `cx`: magenta,
/// with each output's name, a text field that takes the focus and a
/// button that unlocks. It locks once `on` turns true.
fn add_locker(event_loop: &mut EventLoop, cx: &Cx) -> Locker {
  let on = cx.signal(false);
  let typed = cx.signal(String::new());
  let lock = Lock::new(
    cx,
    SessionLock::new()
      .locked(move || on.get())
      .style(Style::new().background(MAGENTA).color(Color::rgb(0, 0, 0)))
      .surface(move |_, output| {
        Column::new()
          .child(Text::new(output.name()))
          .child(TextInput::new(typed).focus(true))
          .child(Button::new(Text::new("unlock")).on_click(move |_| on.set(false)))
      }),
  );
  let locked = lock.locked();
  let id = event_loop.platform().add_session_lock(lock);
  Locker {
    on,
    typed,
    locked,
    id,
  }
}

/// The lock surfaces held, by output, in the order they came.
fn lock_surfaces(platform: &Platform) -> Vec<(String, WindowId)> {
  platform
    .windows()
    .filter(|(_, window)| window.view().root().path().starts_with("SessionLock["))
    .map(|(id, window)| (window.output().name(), id))
    .collect()
}

/// Dispatches until the compositor says the session is `locked`, as
/// `locker` hears it, and returns how long that took.
fn wait_locked(event_loop: &mut EventLoop, locker: Locker, locked: bool) -> Duration {
  let started = Instant::now();
  run_until(event_loop, &format!("locked to be {locked}"), |_| {
    locker.locked.get_untracked() == locked
  });
  started.elapsed()
}

/// Two outputs, HEADLESS-1 at 1280 by 720 and HEADLESS-2 at 640 by 360
/// and scale 2, each with the test bar.
fn two_outputs(sway: &Sway) -> Shell {
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  sway.msg(&["create_output"]);
  sway.msg(&["output", "HEADLESS-2", "mode", "640x360", "scale", "2"]);
  let mut shell = Shell::new(sway);
  shell.wait_for_bar("HEADLESS-1");
  shell.wait_for_bar("HEADLESS-2");
  shell
}

#[test]
#[ignore = "needs headless Sway"]
fn locking_covers_every_output_and_unlocking_shows_the_bars_again() {
  let sway = Sway::start();
  let mut shell = two_outputs(&sway);
  shell.runtime.flush();
  let before = shell.runtime.node_count();
  let cx = Cx::new(shell.runtime.new_owner());
  let locker = add_locker(&mut shell.event_loop, &cx);
  shell.runtime.flush();
  let nodes = shell.runtime.node_count();

  locker.on.set(true);
  let took = wait_locked(&mut shell.event_loop, locker, true);
  eprintln!("the compositor locked {took:?} after the shell asked");
  let platform = shell.event_loop.platform();
  let surfaces = lock_surfaces(platform);
  let names: Vec<&str> = surfaces.iter().map(|(name, _)| name.as_str()).collect();
  assert_eq!(names, ["HEADLESS-1", "HEADLESS-2"]);
  let info = |id| platform.window_info(id).expect("held");
  assert_eq!(info(surfaces[0].1).size, Some((1280, 720)));
  assert_eq!(info(surfaces[1].1).size, Some((320, 180)));
  assert_eq!(info(surfaces[1].1).buffer_size, Some((640, 360)));
  // Right of the centered button there is no text: all magenta, the
  // bars' tops too.
  wait_for_color(
    &sway,
    &mut shell.event_loop,
    "HEADLESS-1",
    (700, 0, 580, 720),
    MAGENTA,
  );
  wait_for_color(
    &sway,
    &mut shell.event_loop,
    "HEADLESS-2",
    (400, 0, 240, 360),
    MAGENTA,
  );

  locker.on.set(false);
  wait_locked(&mut shell.event_loop, locker, false);
  assert!(lock_surfaces(shell.event_loop.platform()).is_empty());
  wait_for_color(
    &sway,
    &mut shell.event_loop,
    "HEADLESS-1",
    (600, 0, 300, 30),
    BAR,
  );
  shell.runtime.flush();
  assert_eq!(shell.runtime.node_count(), nodes, "the surfaces are freed");

  shell.event_loop.platform().remove_session_lock(locker.id);
  assert_eq!(shell.event_loop.platform().session_locks(), 0);
  cx.owner().dispose();
  shell.runtime.flush();
  assert_eq!(shell.runtime.node_count(), before, "the lock is freed");
}

#[test]
#[ignore = "needs headless Sway"]
fn an_output_added_while_locked_gets_a_surface_and_one_unplugged_drops_it() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let cx = Cx::new(shell.runtime.new_owner());
  let locker = add_locker(&mut shell.event_loop, &cx);
  locker.on.set(true);
  wait_locked(&mut shell.event_loop, locker, true);

  sway.msg(&["create_output"]);
  sway.msg(&["output", "HEADLESS-2", "mode", "640x360", "scale", "1"]);
  run_until(
    &mut shell.event_loop,
    "HEADLESS-2's lock surface",
    |platform| {
      lock_surfaces(platform).iter().any(|(name, id)| {
        name == "HEADLESS-2"
          && platform
            .window_info(*id)
            .is_some_and(|info| info.commits > 0)
      })
    },
  );
  wait_for_color(
    &sway,
    &mut shell.event_loop,
    "HEADLESS-2",
    (400, 0, 240, 360),
    MAGENTA,
  );

  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  run_until(
    &mut shell.event_loop,
    "HEADLESS-2's surface to go",
    |platform| {
      lock_surfaces(platform)
        .iter()
        .all(|(name, _)| name == "HEADLESS-1")
    },
  );
  assert!(locker.locked.get_untracked(), "still locked");
}

#[test]
#[ignore = "needs headless Sway"]
fn typing_reaches_a_field_on_the_lock_surface_and_a_click_unlocks() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  // As on a real seat, the keyboard is there when the surface maps.
  let mut keyboard = VirtualKeyboard::new(&sway);
  let mut pointer = VirtualPointer::new(&sway);
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let cx = Cx::new(shell.runtime.new_owner());
  let locker = add_locker(&mut shell.event_loop, &cx);
  locker.on.set(true);
  wait_locked(&mut shell.event_loop, locker, true);
  settle(&mut shell.event_loop);

  keyboard.tap(keys::H);
  keyboard.tap(keys::I);
  run_until(&mut shell.event_loop, "the typing", |_| {
    locker.typed.get_untracked() == "hi"
  });

  let platform = shell.event_loop.platform();
  let (_, id) = lock_surfaces(platform)[0];
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("held");
  let view = window.view();
  let column = view.root().children().next().expect("the column");
  let button = column.children().nth(2).expect("the unlock button").rect();
  drop(view);
  let (x, y) = center(button, (0.0, 0.0));
  pointer.move_to(x as u32, y as u32, (1280, 720));
  pointer.click();
  wait_locked(&mut shell.event_loop, locker, false);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_second_lock_after_the_first_ended_locks_again() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let cx = Cx::new(shell.runtime.new_owner());
  let locker = add_locker(&mut shell.event_loop, &cx);
  for _ in 0..2 {
    locker.on.set(true);
    wait_locked(&mut shell.event_loop, locker, true);
    assert_eq!(lock_surfaces(shell.event_loop.platform()).len(), 1);
    locker.on.set(false);
    wait_locked(&mut shell.event_loop, locker, false);
    assert!(lock_surfaces(shell.event_loop.platform()).is_empty());
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn a_new_locker_takes_over_from_one_that_died_while_locked() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let mut first = sway.connect();
  let locker = add_locker(&mut first, &cx);
  locker.on.set(true);
  wait_locked(&mut first, locker, true);
  // Its connection closes, as when the shell crashes or exits for a
  // reload.
  drop(first);
  // Sway keeps the session locked and shows red in its place.
  let deadline = Instant::now() + Duration::from_secs(10);
  let red = Color::rgb(0xff, 0x00, 0x00);
  while !all_of(
    &area(&sway.screenshot("HEADLESS-1"), (0, 0, 1280, 720)),
    red,
  ) {
    assert!(Instant::now() < deadline, "Sway never showed red");
    std::thread::sleep(Duration::from_millis(50));
  }

  let mut second = sway.connect();
  let locker = add_locker(&mut second, &cx);
  locker.on.set(true);
  let took = wait_locked(&mut second, locker, true);
  eprintln!("a new locker locked {took:?} after asking");
  wait_for_color(
    &sway,
    &mut second,
    "HEADLESS-1",
    (700, 0, 580, 720),
    MAGENTA,
  );
  locker.on.set(false);
  wait_locked(&mut second, locker, false);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_lock_another_client_holds_is_refused_and_asked_again_later() {
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let mut holder = sway.connect();
  let held = add_locker(&mut holder, &cx);
  held.on.set(true);
  wait_locked(&mut holder, held, true);

  let mut second = sway.connect();
  let refused = add_locker(&mut second, &cx);
  refused.on.set(true);
  run_until(&mut second, "the refusal", |_| {
    logs
      .text()
      .contains("the compositor refused or ended the session lock")
  });
  assert!(!refused.locked.get_untracked());
  assert!(lock_surfaces(second.platform()).is_empty());

  held.on.set(false);
  wait_locked(&mut holder, held, false);
  // Still true: asked again only once it turns false and true.
  settle(&mut second);
  assert!(!refused.locked.get_untracked());
  refused.on.set(false);
  settle(&mut second);
  refused.on.set(true);
  wait_locked(&mut second, refused, true);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_locked_session_wakes_nothing_while_nothing_changes() {
  let sway = Sway::start();
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let cx = Cx::new(shell.runtime.new_owner());
  let locker = add_locker(&mut shell.event_loop, &cx);
  locker.on.set(true);
  wait_locked(&mut shell.event_loop, locker, true);
  settle(&mut shell.event_loop);
  let before = shell.event_loop.platform().stats();
  let started = Instant::now();
  shell
    .event_loop
    .dispatch(Some(Duration::from_secs(2)))
    .expect("the connection works");
  let slept = started.elapsed();
  assert!(slept >= Duration::from_millis(1900), "woke after {slept:?}");
  let after = shell.event_loop.platform().stats();
  assert_eq!(after.commits, before.commits, "frames");
  assert_eq!(after.wayland_wakeups, before.wayland_wakeups, "events");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_lock_dropped_while_locked_leaves_the_session_locked() {
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  let mut shell = Shell::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let cx = Cx::new(shell.runtime.new_owner());
  let locker = add_locker(&mut shell.event_loop, &cx);
  locker.on.set(true);
  wait_locked(&mut shell.event_loop, locker, true);

  shell.event_loop.platform().remove_session_lock(locker.id);
  cx.owner().dispose();
  settle(&mut shell.event_loop);
  assert!(lock_surfaces(shell.event_loop.platform()).is_empty());
  assert!(
    logs.text().contains("the session stays locked"),
    "{}",
    logs.text()
  );
  // Sway blanks the output in black, the bar included.
  let black = Color::rgb(0, 0, 0);
  wait_for_color(
    &sway,
    &mut shell.event_loop,
    "HEADLESS-1",
    (0, 0, 1280, 720),
    black,
  );
  assert_eq!(shell.event_loop.platform().session_locks(), 1, "kept");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_popup_from_a_lock_surface_is_logged_once_and_never_shown() {
  let (logs, _logging) = capture_logs();
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1280x720", "scale", "1"]);
  let mut pointer = VirtualPointer::new(&sway);
  let mut event_loop = sway.connect();
  let cx = Cx::new(Runtime::current().new_owner());
  let lock = Lock::new(
    &cx,
    SessionLock::new().locked(true).surface(|_, _| {
      Row::new().child(
        Button::new(Text::new("menu"))
          .on_click(|cx| cx.open_popup(|_| PopupWindow::new().child(Text::new("entry")))),
      )
    }),
  );
  let locked = lock.locked();
  event_loop.platform().add_session_lock(lock);
  run_until(&mut event_loop, "the lock", |_| locked.get_untracked());
  settle(&mut event_loop);
  pointer.move_to(10, 8, (1280, 720));
  pointer.click();
  settle(&mut event_loop);
  let platform = event_loop.platform();
  let popups = popups(platform);
  assert_eq!(popups.len(), 1, "the click opened a popup");
  assert!(!platform.window_info(popups[0]).expect("held").shown);
  // Once, not at every turn of the loop since.
  assert_eq!(
    logs
      .text()
      .matches("a lock surface can't open popups")
      .count(),
    1,
    "{}",
    logs.text()
  );
}

/// Runs one dispatch of `time` and checks nothing woke it: it waited the
/// whole time, in one iteration.
fn sleeps_through(event_loop: &mut EventLoop, time: Duration) {
  let iterations = event_loop.platform().stats().iterations;
  let started = Instant::now();
  event_loop
    .dispatch(Some(time))
    .expect("the connection works");
  let waited = started.elapsed();
  assert!(
    waited >= time - Duration::from_millis(50),
    "woken after {waited:?}"
  );
  assert_eq!(event_loop.platform().stats().iterations, iterations + 1);
}

#[test]
#[ignore = "needs headless Sway"]
fn timers_wake_the_loop_only_while_one_runs() {
  let sway = Sway::start();
  let mut event_loop = sway.connect();
  let cx = Cx::new(Runtime::current().new_owner());
  settle(&mut event_loop);
  let fires = Rc::new(std::cell::Cell::new(0_u32));
  let count = |fires: &Rc<std::cell::Cell<u32>>| {
    let fires = Rc::clone(fires);
    move || fires.set(fires.get() + 1)
  };
  // Timers made outside the loop are armed at its next turn, as `run`'s
  // first one does for the root closure's.
  let arm = |event_loop: &mut EventLoop| {
    event_loop
      .dispatch(Some(Duration::ZERO))
      .expect("the connection works");
  };

  // A one-shot wakes the loop once, at its time, then nothing does.
  let started = Instant::now();
  cx.after(Duration::from_millis(300), count(&fires));
  arm(&mut event_loop);
  event_loop
    .dispatch(Some(Duration::from_secs(3)))
    .expect("the connection works");
  let woke = started.elapsed();
  assert_eq!(fires.get(), 1);
  assert!(
    woke >= Duration::from_millis(290) && woke < Duration::from_secs(1),
    "fired after {woke:?}"
  );
  sleeps_through(&mut event_loop, Duration::from_secs(2));

  // An interval of 100 ms wakes it about ten times a second, once a fire.
  fires.set(0);
  let ticking = cx.interval(Duration::from_millis(100), count(&fires));
  arm(&mut event_loop);
  let iterations = event_loop.platform().stats().iterations;
  let started = Instant::now();
  while started.elapsed() < Duration::from_secs(1) {
    event_loop
      .dispatch(Some(Duration::from_secs(5)))
      .expect("the connection works");
  }
  let woke = event_loop.platform().stats().iterations - iterations;
  assert!(
    (8..=11).contains(&fires.get()),
    "{} fires in a second",
    fires.get()
  );
  assert!(
    woke <= u64::from(fires.get()) + 2,
    "{woke} wakeups for {} fires",
    fires.get()
  );

  // Stopped, it wakes nothing.
  ticking.stop();
  arm(&mut event_loop);
  sleeps_through(&mut event_loop, Duration::from_secs(2));

  // Nor does one whose context is gone.
  let child = cx.child();
  child.interval(Duration::from_millis(50), count(&fires));
  child.owner().dispose();
  let before = fires.get();
  arm(&mut event_loop);
  sleeps_through(&mut event_loop, Duration::from_secs(2));
  assert_eq!(fires.get(), before);

  // When the earliest stops, the loop sleeps until the next one instead.
  fires.set(0);
  let early = cx.interval(Duration::from_millis(100), count(&fires));
  let started = Instant::now();
  cx.after(Duration::from_millis(1500), count(&fires));
  arm(&mut event_loop);
  early.stop();
  arm(&mut event_loop);
  event_loop
    .dispatch(Some(Duration::from_secs(3)))
    .expect("the connection works");
  let woke = started.elapsed();
  assert_eq!(fires.get(), 1, "only the later one fired");
  assert!(woke >= Duration::from_millis(1450), "woken after {woke:?}");
  sleeps_through(&mut event_loop, Duration::from_secs(1));
}

/// The width of the box an overlay sizes from the bar's geometry, and
/// whether the overlay waits for a layout.
fn follower(platform: &Platform, id: WindowId) -> (f32, bool) {
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the overlay is held");
  let view = window.view();
  let row = view
    .root()
    .children()
    .next()
    .expect("the overlay has a row");
  let follower = row.children().next().expect("the row holds the box");
  (follower.rect().width, window.needs_layout())
}

/// An overlay along the top of `output`, 10 pixels tall.
fn overlay(output: &str) -> PanelWindow {
  PanelWindow::new(&Output::new(output))
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(10)
    .exclusive_zone(0)
    .layer(Layer::Overlay)
}

#[test]
#[ignore = "needs headless Sway"]
fn a_window_laid_out_from_another_windows_geometry_catches_up_in_the_same_iteration() {
  follow_the_bar(true);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_window_whose_margins_follow_another_windows_geometry_sends_them_in_the_same_iteration() {
  follow_the_bar(false);
}

/// A bar whose icon's rectangle an overlay before it follows: with
/// `sized`, by laying out a box that wide, and always by its margins.
/// Nothing else wakes the loop, so the iteration whose flush moves the
/// icon must also catch the overlays up.
fn follow_the_bar(sized: bool) {
  let sway = Sway::start();
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let rect = cx.signal(Rect::default());
  let size = cx.signal(10);
  let mut event_loop = sway.connect();
  let ids = Rc::new(RefCell::new(None));
  let built = Rc::clone(&ids);
  event_loop
    .platform()
    .on_event(move |platform: &mut Platform, event| {
      if let Event::OutputAdded(output) = event {
        // The overlays come first, so the loop has passed them before the
        // bar's layout writes the rectangle they follow.
        let boxed = sized.then(|| {
          overlay(&output.name).child(Row::new().align(Align::Start).child(
            crabshell_ui::Stack::new().style(move || {
              Style::new()
                .background(Color::rgb(200, 0, 0))
                .width(rect.get().right())
                .height(4.0)
            }),
          ))
        });
        let moved = overlay(&output.name)
          .margins(move || Margins {
            left: rect.get().right() as i32,
            ..Margins::all(0)
          })
          .style(Style::new().background(Color::rgb(0, 0, 200)));
        let bar = PanelWindow::new(&Output::new(&output.name))
          .anchors(Edge::Top | Edge::Left | Edge::Right)
          .height(30)
          .child(
            Row::new().align(Align::Start).child(
              crabshell_ui::Icon::new("x")
                .size(move || size.get())
                .geometry(rect),
            ),
          );
        let boxed = boxed.map(|boxed| platform.add_window(Window::new(&cx, boxed)));
        let moved = platform.add_window(Window::new(&cx, moved));
        let bar = platform.add_window(Window::new(&cx, bar));
        *built.borrow_mut() = Some((boxed, moved, bar));
      }
    });
  run_until(&mut event_loop, "every window's first frame", |platform| {
    ids.borrow().is_some_and(|(boxed, moved, bar)| {
      boxed.iter().chain([&moved, &bar]).all(|id| {
        platform
          .window_info(*id)
          .is_some_and(|info| info.commits > 0)
      })
    })
  });
  settle(&mut event_loop);
  let (boxed, moved, _) = ids.borrow().expect("every window was built");
  if let Some(boxed) = boxed {
    assert_eq!(follower(event_loop.platform(), boxed), (10.0, false));
  }
  let width = |platform: &Platform| {
    let info = platform.window_info(moved).expect("the overlay is held");
    info.size.expect("the overlay was configured").0
  };
  let before = width(event_loop.platform());

  size.set(30);
  event_loop
    .dispatch(Some(Duration::ZERO))
    .expect("the connection works");
  let platform = event_loop.platform();
  if let Some(boxed) = boxed {
    assert_eq!(follower(platform, boxed), (30.0, false));
  }
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == moved)
    .expect("the overlay is held");
  assert!(!window.surface_changed(), "its margins were sent");
  // Sway answers them with a size 20 pixels narrower.
  run_until(&mut event_loop, "the overlay's new size", |platform| {
    width(platform) == before - 20
  });
}

/// A bar along the bottom of `output`, 40 pixels tall, with four
/// 100-pixel buttons 10 apart that write what they get to `log`: one
/// that drags, one held for a long press, one that double-clicks, and
/// one for the side buttons and scrolling.
fn pointer_bar(cx: &Cx, output: &str, log: &Rc<RefCell<Vec<String>>>) -> Window {
  let note = |name: &'static str| {
    let log = Rc::clone(log);
    move |_: &Cx| log.borrow_mut().push(name.to_owned())
  };
  let (drags, wheels) = (Rc::clone(log), Rc::clone(log));
  let size = Style::new()
    .width(100.0)
    .height(30.0)
    .background(Color::hex("#89b4fa"));
  let panel = PanelWindow::new(&Output::new(output))
    .anchors(Edge::Bottom | Edge::Left | Edge::Right)
    .height(40)
    .style(Style::new().background(Color::hex("#1e1e2e")))
    .child(
      Row::new()
        .gap(10)
        .align(Align::Start)
        .child(
          Button::new(Spacer::new())
            .style(size)
            .on_click(note("drag click"))
            .on_drag(move |_, drag| {
              let (start, at) = (drag.start, drag.position);
              drags.borrow_mut().push(format!(
                "{:?} {},{} {},{}",
                drag.phase, start.x, start.y, at.x, at.y
              ));
            }),
        )
        .child(
          Button::new(Spacer::new())
            .style(size)
            .on_click(note("long click"))
            .on_long_press(note("long")),
        )
        .child(
          Button::new(Spacer::new())
            .style(size)
            .on_click(note("click"))
            .on_double_click(note("double")),
        )
        .child(
          Button::new(Spacer::new())
            .style(size)
            .on_back(note("back"))
            .on_forward(note("forward"))
            .on_wheel(move |_, wheel| {
              wheels.borrow_mut().push(format!(
                "wheel {:?} {} {} {} {}",
                wheel.source, wheel.x.pixels, wheel.y.pixels, wheel.x.stop, wheel.y.stop
              ));
            }),
        ),
    );
  Window::new(cx, panel)
}

/// The test bar on top and a [`pointer_bar`] below on a 1920×1080 output
/// at scale 1, with a virtual pointer.
struct Pointing {
  shell: Shell,
  pointer: VirtualPointer,
  log: Rc<RefCell<Vec<String>>>,
  /// The middle of each of the bottom bar's buttons, on the output.
  spots: Vec<(u32, u32)>,
  _cx: Cx,
}

fn pointing(sway: &Sway) -> Pointing {
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut shell = Shell::new(sway);
  let pointer = VirtualPointer::new(sway);
  shell.wait_for_bar("HEADLESS-1");
  let cx = Cx::new(shell.runtime.new_owner());
  let log = Rc::new(RefCell::new(Vec::new()));
  let id = shell
    .event_loop
    .platform()
    .add_window(pointer_bar(&cx, "HEADLESS-1", &log));
  run_until(
    &mut shell.event_loop,
    "the bottom bar's frame",
    |platform| {
      platform
        .window_info(id)
        .is_some_and(|info| info.commits > 0)
    },
  );
  settle(&mut shell.event_loop);
  let platform = shell.event_loop.platform();
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the bottom bar is held");
  let view = window.view();
  let row = view.root().children().next().expect("the bar has a row");
  let spots = row
    .children()
    .map(|button| {
      let (x, y) = center(button.rect(), (0.0, 1040.0));
      (x as u32, y as u32)
    })
    .collect();
  drop(view);
  Pointing {
    shell,
    pointer,
    log,
    spots,
    _cx: cx,
  }
}

const OUTPUT: (u32, u32) = (1920, 1080);

#[test]
#[ignore = "needs headless Sway"]
fn a_virtual_pointer_drags_a_button_out_of_the_window_and_back() {
  let sway = Sway::start();
  let mut p = pointing(&sway);
  let (x, y) = p.spots[0];
  assert_eq!((x, y), (50, 1055));
  p.pointer.move_to(x, y, OUTPUT);
  p.pointer.press(VirtualPointer::LEFT);
  p.pointer.move_to(x + 30, y, OUTPUT);
  // Far outside the bar: the compositor keeps sending the motion here
  // while the button is held.
  p.pointer.move_to(400, 800, OUTPUT);
  p.pointer.move_to(60, y, OUTPUT);
  p.pointer.release(VirtualPointer::LEFT);
  let log = Rc::clone(&p.log);
  run_until(&mut p.shell.event_loop, "the drag's end", |_| {
    log.borrow().iter().any(|line| line.starts_with("End"))
  });
  settle(&mut p.shell.event_loop);
  assert_eq!(
    *log.borrow(),
    [
      "Start 50,15 80,15",
      "Move 50,15 400,-240",
      "Move 50,15 60,15",
      "End 50,15 60,15",
    ]
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn holding_the_button_runs_the_long_press_and_then_nothing_wakes() {
  let sway = Sway::start();
  let mut p = pointing(&sway);
  let (x, y) = p.spots[1];
  p.pointer.move_to(x, y, OUTPUT);
  settle(&mut p.shell.event_loop);
  p.pointer.press(VirtualPointer::LEFT);
  let pressed = Instant::now();
  let log = Rc::clone(&p.log);
  run_until(&mut p.shell.event_loop, "the long press", |_| {
    !log.borrow().is_empty()
  });
  let held = pressed.elapsed();
  assert!(held >= Duration::from_millis(790), "after {held:?}");
  p.pointer.release(VirtualPointer::LEFT);
  settle(&mut p.shell.event_loop);
  assert_eq!(*log.borrow(), ["long"], "the release doesn't click");
  // Its timer isn't left armed: one dispatch sleeps the whole time.
  sleeps_through(&mut p.shell.event_loop, Duration::from_secs(2));
}

#[test]
#[ignore = "needs headless Sway"]
fn a_double_click_side_buttons_and_a_sideways_swipe_reach_their_buttons() {
  let sway = Sway::start();
  let mut p = pointing(&sway);
  let (x, y) = p.spots[2];
  p.pointer.move_to(x, y, OUTPUT);
  p.pointer.click();
  p.pointer.click();
  let log = Rc::clone(&p.log);
  run_until(&mut p.shell.event_loop, "the double click", |_| {
    log.borrow().len() == 2
  });
  assert_eq!(*log.borrow(), ["click", "double"]);
  log.borrow_mut().clear();

  let (x, y) = p.spots[3];
  p.pointer.move_to(x, y, OUTPUT);
  for button in [VirtualPointer::SIDE, VirtualPointer::EXTRA] {
    p.pointer.press(button);
    p.pointer.release(button);
  }
  p.pointer.swipe(30.0, 0.0, false);
  p.pointer.swipe(0.0, 0.0, true);
  run_until(&mut p.shell.event_loop, "the swipe's end", |_| {
    log.borrow().len() == 4
  });
  assert_eq!(
    *log.borrow(),
    [
      "back",
      "forward",
      "wheel Finger 30 0 false false",
      "wheel Finger 0 0 true true",
    ]
  );
}

/// A bottom bar of three 100-pixel chips 10 apart, keyed by number, that
/// slide aside as one goes, with a trash target after them that takes a
/// chip dropped on it, and whether a drop waits over the trash.
fn chips_bar(cx: &Cx, output: &str, chips: Signal<Vec<u32>>, over: Signal<bool>) -> Window {
  let size = Style::new()
    .width(100.0)
    .height(30.0)
    .background(Color::hex("#89b4fa"));
  let panel = PanelWindow::new(&Output::new(output))
    .anchors(Edge::Bottom | Edge::Left | Edge::Right)
    .height(40)
    .style(Style::new().background(Color::hex("#1e1e2e")))
    .child(
      Row::new()
        .gap(10)
        .align(Align::Start)
        .children_keyed(
          move || chips.get(),
          |n| *n,
          move |cx, n| {
            // The chip follows the pointer while it is dragged.
            let moved = cx.signal(crabshell_ui::Point::default());
            Button::new(Spacer::new())
              .style(move || {
                let at = moved.get();
                size.translate(at.x, at.y)
              })
              .drag_data(move || Some(n.get()))
              .on_drag(move |_, drag| {
                let gone = drag.phase == crabshell_ui::DragPhase::End;
                moved.set(if gone {
                  crabshell_ui::Point::default()
                } else {
                  crabshell_ui::Point::new(
                    drag.position.x - drag.start.x,
                    drag.position.y - drag.start.y,
                  )
                });
              })
          },
        )
        .displaced(Transition::new(150))
        .child(
          Button::new(Spacer::new())
            .style(size.background(Color::hex("#f38ba8")))
            .drop_hovered(over)
            .on_drop(move |_, n: u32| chips.update(|chips| chips.retain(|kept| *kept != n))),
        ),
    );
  Window::new(cx, panel)
}

#[test]
#[ignore = "needs headless Sway"]
fn a_virtual_pointer_drops_a_chip_on_the_trash_and_the_rest_slide_then_sleep() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut shell = Shell::new(&sway);
  let mut pointer = VirtualPointer::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let cx = Cx::new(shell.runtime.new_owner());
  let chips = cx.signal(vec![1, 2, 3]);
  let over = cx.signal(false);
  let id = shell
    .event_loop
    .platform()
    .add_window(chips_bar(&cx, "HEADLESS-1", chips, over));
  run_until(&mut shell.event_loop, "the chips' frame", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > 0)
  });
  settle(&mut shell.event_loop);

  // Chip 1 is at 0 to 100 and the trash at 330 to 430, 1040 down.
  pointer.move_to(50, 1055, OUTPUT);
  pointer.press(VirtualPointer::LEFT);
  pointer.move_to(200, 1055, OUTPUT);
  pointer.move_to(380, 1055, OUTPUT);
  run_until(&mut shell.event_loop, "the drag over the trash", |_| {
    over.get_untracked()
  });
  let commits = shell
    .event_loop
    .platform()
    .window_info(id)
    .map_or(0, |info| info.commits);
  pointer.release(VirtualPointer::LEFT);
  run_until(&mut shell.event_loop, "the drop", |_| {
    chips.get_untracked() == [2, 3]
  });
  assert!(!over.get_untracked(), "the drag ended");
  run_until(&mut shell.event_loop, "the slide's end", |_| {
    !crabshell_ui::animating()
  });
  settle(&mut shell.event_loop);
  let platform = shell.event_loop.platform();
  let slid = platform.window_info(id).map_or(0, |info| info.commits) - commits;
  assert!(slid > 2, "the slide drew frames as it went: {slid}");
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the chips are held");
  let view = window.view();
  let row = view.root().children().next().expect("the bar has a row");
  let placed: Vec<_> = row
    .children()
    .map(|child| (child.rect().x, child.transform()))
    .collect();
  assert_eq!(placed, [(0.0, None), (110.0, None), (220.0, None)]);
  drop(view);
  sleeps_through(&mut shell.event_loop, Duration::from_secs(2));
}

/// A bar along the bottom with a 120-pixel slider over 0 to 104, so its
/// value is where its handle's left edge is, and a 40-pixel square view of
/// 30 rows 20 tall.
fn slider_bar(cx: &Cx, value: Signal<f32>, dragging: Signal<bool>, offset: Signal<f32>) -> Window {
  let rows = (0..30).fold(Column::new(), |column, n| {
    let shade = if n % 2 == 0 { "#45475a" } else { "#585b70" };
    column.child(Row::new().style(Style::new().height(20.0).background(Color::hex(shade))))
  });
  let panel = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Bottom | Edge::Left | Edge::Right)
    .height(40)
    .style(Style::new().background(Color::hex("#1e1e2e")))
    .child(
      Row::new()
        .gap(10)
        .align(Align::Start)
        .child(
          Slider::new(value)
            .range(0.0, 104.0)
            .dragging(dragging)
            .style(Style::new().background(Color::hex("#45475a")))
            .fill_style(Style::new().background(Color::hex("#89b4fa")))
            .handle_style(Style::new().radius(8.0).background(Color::hex("#cdd6f4"))),
        )
        .child(
          ScrollView::new(rows)
            .offset(offset)
            .style(Style::new().width(40.0).height(40.0)),
        ),
    );
  Window::new(cx, panel)
}

#[test]
#[ignore = "needs headless Sway"]
fn a_virtual_pointer_drags_a_slider_and_a_touchpad_flings_a_view_to_rest() {
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let mut shell = Shell::new(&sway);
  let mut pointer = VirtualPointer::new(&sway);
  shell.wait_for_bar("HEADLESS-1");
  let cx = Cx::new(shell.runtime.new_owner());
  let (value, dragging, offset) = (cx.signal(0.0), cx.signal(false), cx.signal(0.0));
  let id = shell
    .event_loop
    .platform()
    .add_window(slider_bar(&cx, value, dragging, offset));
  run_until(&mut shell.event_loop, "the bar's frame", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > 0)
  });
  settle(&mut shell.event_loop);

  // The handle is 16 tall at the bar's top; its middle follows the
  // pointer from the press, with no threshold.
  pointer.move_to(60, 1048, OUTPUT);
  pointer.press(VirtualPointer::LEFT);
  run_until(&mut shell.event_loop, "the press", |_| value.get() == 52.0);
  assert!(dragging.get());
  pointer.move_to(400, 900, OUTPUT);
  run_until(&mut shell.event_loop, "the drag to the end", |_| {
    value.get() == 104.0
  });
  pointer.move_to(30, 1048, OUTPUT);
  pointer.release(VirtualPointer::LEFT);
  run_until(&mut shell.event_loop, "the release", |_| !dragging.get());
  assert_eq!(value.get(), 22.0);

  // Five 10-pixel moves 10 ms apart lift at 1000 pixels a second.
  pointer.move_to(150, 1060, OUTPUT);
  for _ in 0..5 {
    pointer.swipe(0.0, 10.0, false);
    std::thread::sleep(Duration::from_millis(10));
  }
  pointer.swipe(0.0, 0.0, true);
  run_until(&mut shell.event_loop, "the fling's rest", |_| {
    offset.get() > 50.0 && !crabshell_ui::animating()
  });
  assert!(offset.get() > 150.0, "it flung on to {}", offset.get());
  settle(&mut shell.event_loop);
  sleeps_through(&mut shell.event_loop, Duration::from_secs(2));
}

/// A 1 by 1 GIF that loops for ever between red and green, 100 ms each.
fn blinking_gif() -> Vec<u8> {
  let mut gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00".to_vec();
  gif.extend_from_slice(&[255, 0, 0, 0, 255, 0]);
  gif.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\x00\x00\x00");
  // Each frame: a 100 ms delay, then index 0 or 1 in two bytes of LZW.
  for data in [[0x44, 0x01], [0x4c, 0x01]] {
    gif.extend_from_slice(&[0x21, 0xf9, 4, 0, 10, 0, 0, 0]);
    gif.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2]);
    gif.extend_from_slice(&data);
    gif.push(0);
  }
  gif.push(0x3b);
  gif
}

/// Applies what the test set, then runs the loop for `time` and returns
/// how many iterations and commits that took.
fn run_for(event_loop: &mut EventLoop, time: Duration) -> (u64, u64) {
  event_loop
    .dispatch(Some(Duration::ZERO))
    .expect("the connection works");
  let stats = event_loop.platform().stats();
  let end = Instant::now() + time;
  while let Some(left) = end.checked_duration_since(Instant::now()) {
    event_loop
      .dispatch(Some(left))
      .expect("the connection works");
  }
  let after = event_loop.platform().stats();
  (
    after.iterations - stats.iterations,
    after.commits - stats.commits,
  )
}

#[test]
#[ignore = "needs headless Sway"]
fn a_shown_gif_wakes_only_at_its_frames_and_a_still_paused_or_hidden_one_wakes_nothing() {
  let sway = Sway::start();
  let dir = std::env::temp_dir().join(format!("crabshell-gif-{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("a directory for the GIF");
  let path = dir.join("blink.gif");
  std::fs::write(&path, blinking_gif()).expect("write the GIF");
  let still = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../crabshell-render/tests/fixtures/stripes.png");
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let playing = cx.signal(true);
  let shown = cx.signal(true);
  let bar = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .child(
      Row::new()
        .child(crabshell_ui::Image::new(still).width(40).height(20))
        .child(
          crabshell_ui::Image::new(path.clone())
            .width(20)
            .height(20)
            .playing(move || playing.get())
            .visible(move || shown.get()),
        ),
    );
  let mut event_loop = sway.connect();
  let id = event_loop.platform().add_window(Window::new(&cx, bar));
  run_until(&mut event_loop, "the bar's first frame", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > 0)
  });

  // Shown, it draws a frame each 100 ms and wakes for little else.
  let (iterations, commits) = run_for(&mut event_loop, Duration::from_secs(2));
  assert!((14..=24).contains(&commits), "{commits} frames in 2 s");
  assert!(iterations <= commits * 2 + 4, "{iterations} wakeups");

  // Paused, it stays on its frame and nothing wakes.
  playing.set(false);
  settle(&mut event_loop);
  assert_eq!(run_for(&mut event_loop, Duration::from_secs(2)), (1, 0));

  // Hidden while it plays, it may wake once for the frame it waited for,
  // and then nothing.
  playing.set(true);
  let (_, commits) = run_for(&mut event_loop, Duration::from_millis(500));
  assert!(commits >= 2, "{commits} frames in 500 ms");
  shown.set(false);
  settle(&mut event_loop);
  assert_eq!(run_for(&mut event_loop, Duration::from_secs(2)), (1, 0));

  // Shown again, it plays again.
  shown.set(true);
  let (_, commits) = run_for(&mut event_loop, Duration::from_secs(1));
  assert!(commits >= 5, "{commits} frames in 1 s");
  let _ = std::fs::remove_dir_all(&dir);
}

/// The threads of this process named `name`.
fn threads_named(name: &str) -> usize {
  std::fs::read_dir("/proc/self/task")
    .expect("this process's threads")
    .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("comm")).ok())
    .filter(|comm| comm.trim_end() == name)
    .count()
}

#[test]
#[ignore = "needs headless Sway"]
fn a_picture_decoded_off_the_loop_repaints_its_box_and_then_nothing_wakes() {
  let sway = Sway::start();
  let fixtures =
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crabshell-render/tests/fixtures");
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let name = cx.signal("stripes.png");
  // Blurred, so it is decoded on the worker thread.
  let bar = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .child(
      Row::new().child(
        crabshell_ui::Image::new(move || fixtures.join(name.get()))
          .width(40)
          .height(20)
          .blur(2.0),
      ),
    );
  let mut event_loop = sway.connect();
  let id = event_loop.platform().add_window(Window::new(&cx, bar));
  run_until(&mut event_loop, "the bar's first frame", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > 0)
  });
  settle(&mut event_loop);
  // A new source shows the old picture at once, then the new one when it
  // comes in: two frames, each repainting the image's box alone.
  let before = event_loop
    .platform()
    .window_info(id)
    .expect("the bar")
    .commits;
  name.set("bands.png");
  run_until(&mut event_loop, "the new picture's frame", |platform| {
    platform.window_info(id).is_some_and(|info| {
      let damage = info.last_damage;
      info.commits >= before + 2
        && !damage.is_empty()
        && damage.iter().all(|rect| rect.x + rect.width <= 48)
    })
  });
  settle(&mut event_loop);
  sleeps_through(&mut event_loop, Duration::from_secs(2));
  assert_eq!(threads_named("decode"), 0, "the decoding thread ended");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_windows_first_commit_waits_for_the_pictures_its_first_draw_asked_for() {
  let sway = Sway::start();
  let dir = std::env::temp_dir().join(format!("crabshell-first-{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("a directory for the picture");
  let path = dir.join("red.png");
  let file = std::fs::File::create(&path).expect("create the picture");
  let mut encoder = png::Encoder::new(file, 64, 64);
  encoder.set_color(png::ColorType::Rgba);
  let mut writer = encoder.write_header().expect("a PNG header");
  writer
    .write_image_data(&[255, 0, 0, 255].repeat(64 * 64))
    .expect("the pixels");
  writer.finish().expect("the PNG");
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  // Blurred, so it is decoded on the worker thread.
  let bar = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .child(
      Row::new().child(
        crabshell_ui::Image::new(path)
          .width(40)
          .height(30)
          .blur(1.0),
      ),
    );
  let mut event_loop = sway.connect();
  let id = event_loop.platform().add_window(Window::new(&cx, bar));
  run_until(&mut event_loop, "the bar's first frame", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > 0)
  });
  // Nothing more is dispatched, so the screen shows the first commit.
  let shot = sway.screenshot("HEADLESS-1");
  let at = ((15 * shot.width + 20) * 4) as usize;
  assert_eq!(
    &shot.rgba[at..at + 4],
    &[255, 0, 0, 255],
    "the first commit shows the picture"
  );
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "needs headless Sway"]
fn an_image_transition_draws_while_it_runs_and_then_nothing_wakes() {
  let sway = Sway::start();
  let fixtures =
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crabshell-render/tests/fixtures");
  let (stripes, bands) = (fixtures.join("stripes.png"), fixtures.join("bands.png"));
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let next = cx.signal(false);
  let bar = PanelWindow::new(&Output::new("HEADLESS-1"))
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .child(
      Row::new().child(
        crabshell_ui::Image::new(move || {
          if next.get() {
            bands.clone()
          } else {
            stripes.clone()
          }
        })
        .width(60)
        .height(30)
        .transition(crabshell_ui::ImageTransition::crossfade(Transition::new(
          500,
        ))),
      ),
    );
  let mut event_loop = sway.connect();
  let id = event_loop.platform().add_window(Window::new(&cx, bar));
  run_until(&mut event_loop, "the bar's first frame", |platform| {
    platform
      .window_info(id)
      .is_some_and(|info| info.commits > 0)
  });
  settle(&mut event_loop);
  assert_eq!(run_for(&mut event_loop, Duration::from_secs(1)), (1, 0));

  // While it runs, it draws on the animation clock; once it ends,
  // nothing wakes.
  next.set(true);
  let (_, commits) = run_for(&mut event_loop, Duration::from_millis(500));
  assert!(commits >= 10, "{commits} frames in the 500 ms it runs");
  settle(&mut event_loop);
  assert_eq!(run_for(&mut event_loop, Duration::from_secs(2)), (1, 0));
}
