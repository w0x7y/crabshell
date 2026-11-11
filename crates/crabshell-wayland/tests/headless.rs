//! The platform on a real compositor: headless Sway, one per test.
//!
//! They need `sway`, `swaymsg` and `grim`, so they are ignored by
//! default. Run them with
//!
//! ```text
//! cargo test -p crabshell-wayland --test headless -- --ignored --test-threads=1
//! ```

mod sway;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crabshell_reactive::{Runtime, Signal};
use crabshell_ui::{
    Align, Button, Color, Cx, Edge, Output, PanelWindow, Rect, Row, Spacer, Style, Text, Window,
};
use crabshell_wayland::{Event, EventLoop, Platform, WindowId};
use sway::{Image, Sway, VirtualPointer, run_until, settle};

/// What the test bar's bindings read and write.
#[derive(Clone, Copy)]
struct Knobs {
    label: Signal<String>,
    clicks: Signal<u32>,
}

/// A 30-pixel bar across the top of `output`: a button at the left and a
/// label at the right, on an opaque background.
fn bar(cx: &Cx, output: &str, knobs: Knobs) -> Window {
    let Knobs { label, clicks } = knobs;
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
                        .on_click(move |_| clicks.update(|count| *count += 1)),
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
        let runtime = Runtime::current();
        let cx = Cx::new(runtime.new_owner());
        let knobs = Knobs {
            label: cx.signal(String::from("12:34")),
            clicks: cx.signal(0),
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
        self.built
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
    assert!(
        shot.width == snapshot.width && shot.height >= snapshot.height,
        "{name}: a {}x{} screenshot can't hold a {}x{} bar",
        shot.width,
        shot.height,
        snapshot.width,
        snapshot.height
    );
    let row = shot.width as usize * 4;
    let top = &shot.rgba[..row * snapshot.height as usize];
    let differing = top
        .as_chunks::<4>()
        .0
        .iter()
        .zip(snapshot.rgba.as_chunks::<4>().0)
        .filter(|(shot, drawn)| {
            shot.iter()
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
        platform.windows().count() == 1
            && platform.outputs().all(|output| output.name != "HEADLESS-2")
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
