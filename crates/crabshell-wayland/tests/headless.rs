//! The platform on a real compositor: headless Sway, one per test.
//!
//! They need `sway`, `swaymsg` and `grim`, so they are ignored by
//! default. Run them without the session's `SWAYSOCK`, which
//! `Sway::start()` refuses:
//!
//! ```text
//! env -u SWAYSOCK cargo test -p crabshell-wayland --test headless -- --ignored --test-threads=1
//! ```

mod sway;

use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use crabshell_reactive::{Runtime, Signal};
use crabshell_ui::{
  Align, Button, Color, Column, Cx, Edge, Output, PanelWindow, PopupWindow, Rect, Row, Spacer,
  Style, Text, Window,
};
use crabshell_wayland::{Event, EventLoop, Platform, WindowId};
use sway::{Image, Sway, VirtualPointer, run_until, settle};

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

/// What the test bar's bindings read and write.
#[derive(Clone, Copy)]
struct Knobs {
  label: Signal<String>,
  clicks: Signal<u32>,
  /// The first entry of every [`menu`].
  entry: Signal<String>,
  /// Whether the bar's button opens a [`menu`].
  popups: bool,
  /// Whether it opens it from an effect, which runs in a flush, as a
  /// menu waiting on its app's reply would. The effect opens another each
  /// time `reopen` changes.
  from_effect: Signal<bool>,
  reopen: Signal<u32>,
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

/// A 30-pixel bar across the top of `output`: a button at the left and a
/// label at the right, on an opaque background.
fn bar(cx: &Cx, output: &str, knobs: Knobs) -> Window {
  let Knobs { label, clicks, .. } = knobs;
  let panel = PanelWindow::new(&Output::new(output))
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
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
              if !knobs.popups {
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
        .child(Spacer::new())
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
    Shell::build(sway, false)
  }

  /// A shell whose bars' buttons open a [`menu`].
  fn with_popups(sway: &Sway) -> Shell {
    Shell::build(sway, true)
  }

  fn build(sway: &Sway, popups: bool) -> Shell {
    let runtime = Runtime::current();
    let cx = Cx::new(runtime.new_owner());
    let knobs = Knobs {
      label: cx.signal(String::from("12:34")),
      clicks: cx.signal(0),
      entry: cx.signal(String::from("more")),
      popups,
      from_effect: cx.signal(false),
      reopen: cx.signal(0),
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
  /// HEADLESS-1, 1920 by 1080 pixels at `scale`.
  fn start(sway: &Sway, scale: &str) -> Menus {
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let factor: f32 = scale.parse().expect("a scale");
    let mut shell = Shell::with_popups(sway);
    let pointer = VirtualPointer::new(sway);
    let bar = shell.wait_for_bar("HEADLESS-1");
    Menus {
      shell,
      pointer,
      bar,
      offset: (0.0, 0.0),
      layout: ((1920.0 / factor) as u32, (1080.0 / factor) as u32),
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
