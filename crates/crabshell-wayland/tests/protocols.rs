//! The platform against a fake compositor, for protocols no compositor
//! here offers or lets a test drive: blur behind windows, touch,
//! workspace requests Sway and labwc refuse, and what an activation
//! token is asked with. They run by default.

mod fake;

use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use crabshell_ui::{
  Button, Color, Cx, DragContent, DragPhase, Edge, Output, PanelWindow, Region, Row, ScrollView,
  Stack, Style, Text, TextInput, Window,
};
use crabshell_wayland::{EventLoop, WindowId, Windowset};
use fake::{Fake, Globals};
use smithay_client_toolkit::reexports::client::Connection;
use wayland_protocols::ext::workspace::v1::server::ext_workspace_handle_v1::WorkspaceCapabilities as Can;

/// A loop connected to a new fake with `globals`.
fn connect(globals: Globals) -> (Fake, EventLoop) {
  let (fake, stream) = Fake::start(globals);
  let connection = Connection::from_socket(stream).expect("a connection");
  let event_loop = EventLoop::with_connection(connection).expect("the platform starts");
  (fake, event_loop)
}

/// Dispatches until `done`, for at most 5 s.
fn wait(event_loop: &mut EventLoop, what: &str, mut done: impl FnMut(&mut EventLoop) -> bool) {
  let deadline = Instant::now() + Duration::from_secs(5);
  while !done(event_loop) {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    event_loop
      .dispatch(Some(Duration::from_millis(5)))
      .expect("the connection works");
  }
}

/// Dispatches for `time`, whatever comes.
fn run_for(event_loop: &mut EventLoop, time: Duration) {
  let until = Instant::now() + time;
  while Instant::now() < until {
    event_loop
      .dispatch(Some(Duration::from_millis(5)))
      .expect("the connection works");
  }
}

/// Dispatches until window `id` has committed `n` more frames than now.
fn frames(event_loop: &mut EventLoop, id: WindowId, n: u64) {
  let commits = |event_loop: &mut EventLoop| {
    let info = event_loop.platform().window_info(id);
    info.map_or(0, |info| info.commits)
  };
  let start = commits(event_loop);
  wait(event_loop, "the window's frames", |event_loop| {
    commits(event_loop) >= start + n
  });
}

/// The region the bars blur.
fn bar() -> Region {
  Region::rect(10, 0, 100, 30)
}

/// A bar along the top of `FAKE-1`, 30 high, blurred behind `region`,
/// whose color and mask `tint` changes, so each change sends its surface
/// settings again.
fn blurred_bar(
  cx: &Cx,
  region: crabshell_reactive::Signal<Option<Region>>,
  tint: crabshell_reactive::Signal<u8>,
) -> Window {
  let bar = PanelWindow::new(&Output::new("FAKE-1"))
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .blur(move || region.get())
    .mask(move || Some(Region::rect(0, 0, 640 - u32::from(tint.get()), 30)))
    .style(move || Style::new().background(Color::rgb(tint.get(), 0, 0)));
  Window::new(cx, bar)
}

/// Shared memory this process has in RAM, in KiB: window buffers.
fn shared_kib() -> u64 {
  let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
  status
    .lines()
    .find_map(|line| line.strip_prefix("RssShmem:"))
    .and_then(|rest| rest.trim().strip_suffix("kB")?.trim().parse().ok())
    .expect("an RssShmem line")
}

/// A window over the whole 640x480 output at scale 2, 4.7 MiB a buffer,
/// whose background `tint` sets, and how many buffers hold memory.
fn full_window(cx: &Cx, event_loop: &mut EventLoop) -> (crabshell_reactive::Signal<u8>, WindowId) {
  let tint = cx.signal(0_u8);
  let window = PanelWindow::new(&Output::new("FAKE-1"))
    .anchors(Edge::Top | Edge::Bottom | Edge::Left | Edge::Right)
    .style(move || Style::new().background(Color::rgb(tint.get(), 0, 0)));
  let window = event_loop.platform().add_window(Window::new(cx, window));
  frames(event_loop, window, 1);
  (tint, window)
}

/// How many of window `id`'s buffers hold memory.
fn buffers(event_loop: &mut EventLoop, id: WindowId) -> usize {
  let info = event_loop.platform().window_info(id);
  info.expect("the window").buffers
}

/// The fake reads a buffer until the next commit, as compositors that
/// keep shm buffers do, so each new frame lands in the buffer it read
/// before. The spare is never emptied there, or every frame would be
/// repainted whole.
#[test]
fn a_compositor_that_holds_the_buffer_shown_leaves_the_spare() {
  let (_fake, mut event_loop) = connect(Globals {
    scale: 2,
    ..Globals::default()
  });
  let cx = Cx::new(Runtime::current().new_owner());
  let (tint, window) = full_window(&cx, &mut event_loop);
  for value in 1..=3 {
    tint.set(value);
    frames(&mut event_loop, window, 1);
    run_for(&mut event_loop, Duration::from_millis(50));
    assert_eq!(buffers(&mut event_loop, window), 2, "after frame {value}");
  }
  cx.owner().dispose();
}

/// Once the compositor releases the buffer shown too, as one that copies
/// buffers does, the memory of the spare is given back, keeping the
/// buffer shown, and nothing wakes the loop for it.
#[test]
fn a_spare_is_given_back_once_the_buffer_shown_is_released() {
  let (fake, mut event_loop) = connect(Globals {
    scale: 2,
    ..Globals::default()
  });
  let cx = Cx::new(Runtime::current().new_owner());
  let (tint, window) = full_window(&cx, &mut event_loop);
  tint.set(1);
  frames(&mut event_loop, window, 1);
  assert_eq!(
    buffers(&mut event_loop, window),
    2,
    "a frame while the last is read"
  );
  let both = shared_kib();
  fake.run(fake::State::copy_buffers);
  wait(&mut event_loop, "the spare given back", |event_loop| {
    buffers(event_loop, window) == 1
  });
  let one = shared_kib();
  assert!(one + 4096 <= both, "{both} KiB with both, {one} KiB after");
  // Copied, each frame goes back into the one buffer.
  for value in 2..=4 {
    tint.set(value);
    frames(&mut event_loop, window, 1);
    run_for(&mut event_loop, Duration::from_millis(20));
    assert_eq!(buffers(&mut event_loop, window), 1, "after frame {value}");
  }
  let wakeups = event_loop.platform().stats().wayland_wakeups;
  run_for(&mut event_loop, Duration::from_millis(200));
  assert_eq!(event_loop.platform().stats().wayland_wakeups, wakeups);
  cx.owner().dispose();
}

#[test]
fn blur_goes_out_in_surface_pixels_once_and_clears_at_scales_1_and_2() {
  for scale in [1, 2] {
    let (fake, mut event_loop) = connect(Globals {
      scale,
      effects: Some(true),
      ..Globals::default()
    });
    let cx = Cx::new(Runtime::current().new_owner());
    let region = cx.signal(Some(bar()));
    let tint = cx.signal(0_u8);
    let bar = event_loop
      .platform()
      .add_window(blurred_bar(&cx, region, tint));
    wait(&mut event_loop, "the blur", |_| !fake.log().is_empty());
    assert_eq!(fake.log(), ["blur 10,0,100,30"], "at scale {scale}");
    let info = event_loop.platform().window_info(bar).expect("the bar");
    let side = |logical: u32| logical * scale as u32;
    assert_eq!(info.buffer_size, Some((side(640), side(30))));

    // New settings with the same region send it no more.
    tint.set(1);
    frames(&mut event_loop, bar, 1);
    tint.set(2);
    frames(&mut event_loop, bar, 1);
    assert_eq!(fake.log(), ["blur 10,0,100,30"], "at scale {scale}");

    region.set(Some(
      Region::rect(0, 0, 50, 30).union(Region::rect(600, 0, 100, 30)),
    ));
    wait(&mut event_loop, "the new region", |_| fake.log().len() == 2);
    // Cut to the bar's 640 logical pixels.
    assert_eq!(fake.log()[1], "blur 0,0,50,30 600,0,40,30");
    region.set(None);
    wait(&mut event_loop, "the cleared blur", |_| {
      fake.log().len() == 3
    });
    assert_eq!(fake.log()[2], "blur none");
    cx.owner().dispose();
  }
}

#[test]
fn blur_the_compositor_turns_on_late_is_sent_then() {
  let (fake, mut event_loop) = connect(Globals {
    effects: Some(false),
    ..Globals::default()
  });
  let cx = Cx::new(Runtime::current().new_owner());
  let tint = cx.signal(0_u8);
  let bar = blurred_bar(&cx, cx.signal(Some(bar())), tint);
  let bar = event_loop.platform().add_window(bar);
  frames(&mut event_loop, bar, 1);
  run_for(&mut event_loop, Duration::from_millis(50));
  assert!(fake.log().is_empty(), "{:?}", fake.log());

  fake.run(|state| state.blur_on());
  // The next frame carries it.
  wait(&mut event_loop, "the late blur", |event_loop| {
    tint.update(|tint| *tint = tint.wrapping_add(1));
    event_loop
      .dispatch(Some(Duration::from_millis(5)))
      .expect("the connection works");
    !fake.log().is_empty()
  });
  assert_eq!(fake.log(), ["blur 10,0,100,30"]);
  cx.owner().dispose();
}

/// A bar with one button, 100 by 30 at its left, that counts its clicks.
fn tapped() -> (Fake, EventLoop, crabshell_reactive::Signal<u32>, Cx) {
  let (fake, mut event_loop) = connect(Globals {
    touch: true,
    pointer: true,
    activation: true,
    ..Globals::default()
  });
  let cx = Cx::new(Runtime::current().new_owner());
  let clicks = cx.signal(0_u32);
  let bar = PanelWindow::new(&Output::new("FAKE-1"))
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .child(
      Button::new(Text::new("tap"))
        .style(Style::new().width(100.0).height(30.0))
        .on_click(move |_| clicks.update(|n| *n += 1)),
    );
  let bar = event_loop.platform().add_window(Window::new(&cx, bar));
  frames(&mut event_loop, bar, 1);
  (fake, event_loop, clicks, cx)
}

#[test]
fn a_finger_tapping_a_button_clicks_it_once() {
  let (fake, mut event_loop, clicks, cx) = tapped();
  fake.run(|state| state.tap((10.0, 10.0), 2.0, false));
  wait(&mut event_loop, "the click", |_| clicks.get() == 1);
  run_for(&mut event_loop, Duration::from_millis(50));
  assert_eq!(clicks.get(), 1);
  cx.owner().dispose();
}

#[test]
fn a_cancelled_touch_doesnt_click() {
  let (fake, mut event_loop, clicks, cx) = tapped();
  fake.run(|state| state.tap((10.0, 10.0), 2.0, true));
  // A tap after it is told after it, so once that clicks, the cancel
  // was handled.
  fake.run(|state| state.tap((20.0, 10.0), 0.0, false));
  wait(&mut event_loop, "the second tap's click", |_| {
    clicks.get() > 0
  });
  run_for(&mut event_loop, Duration::from_millis(50));
  assert_eq!(clicks.get(), 1, "only the tap that lifted");
  cx.owner().dispose();
}

/// Asks for an activation token for `app_id`, and dispatches until it
/// comes.
fn token(event_loop: &mut EventLoop, app_id: Option<&str>) -> String {
  let got: Rc<RefCell<Option<String>>> = Rc::default();
  let into = Rc::clone(&got);
  let asked = event_loop
    .platform()
    .request_activation_token(app_id, move |token| *into.borrow_mut() = Some(token));
  assert!(asked, "the compositor hands out tokens");
  wait(event_loop, "the token", |_| got.borrow().is_some());
  got.take().expect("the token")
}

#[test]
fn an_activation_token_names_the_last_tap_and_its_window() {
  let (fake, mut event_loop, clicks, cx) = tapped();
  assert!(event_loop.platform().offers_activation());
  // Before any input, there is no serial or surface to give.
  assert_eq!(token(&mut event_loop, Some("org.example.App")), "token-1");
  wait(&mut event_loop, "the token's end", |_| {
    fake.log().len() == 2
  });
  assert_eq!(
    fake.log(),
    [
      "token org.example.App serial none on none",
      "token destroyed"
    ]
  );

  fake.clear();
  fake.run(|state| state.tap((10.0, 10.0), 0.0, false));
  wait(&mut event_loop, "the click", |_| clicks.get() == 1);
  assert_eq!(token(&mut event_loop, None), "token-2");
  wait(&mut event_loop, "the token's end", |_| {
    fake.log().len() == 3
  });
  let down = fake.log()[0].clone();
  let serial = down.strip_prefix("down ").expect("the tap's serial");
  assert_eq!(
    fake.log()[1..],
    [
      format!("token none serial {serial} on panel"),
      String::from("token destroyed")
    ]
  );
  cx.owner().dispose();
}

#[test]
fn an_activation_token_names_the_last_clicks_press_and_its_window() {
  let (fake, mut event_loop, clicks, cx) = tapped();
  fake.run(|state| state.click((10.0, 10.0)));
  wait(&mut event_loop, "the click", |_| clicks.get() == 1);
  assert_eq!(token(&mut event_loop, Some("org.example.App")), "token-1");
  wait(&mut event_loop, "the token's end", |_| {
    fake.log().len() == 3
  });
  let press = fake.log()[0].clone();
  let serial = press.strip_prefix("press ").expect("the press's serial");
  assert_eq!(
    fake.log()[1..],
    [
      format!("token org.example.App serial {serial} on panel"),
      String::from("token destroyed")
    ]
  );
  cx.owner().dispose();
}

#[test]
fn without_xdg_activation_no_token_is_asked_for() {
  let (_fake, mut event_loop) = connect(Globals::default());
  let platform = event_loop.platform();
  assert!(!platform.offers_activation());
  assert!(
    !platform.request_activation_token(Some("org.example.App"), |_| { panic!("no token comes") })
  );
}

#[test]
fn a_finger_drags_a_view_whose_mouse_drag_is_off() {
  let (fake, mut event_loop) = connect(Globals {
    touch: true,
    ..Globals::default()
  });
  let cx = Cx::new(Runtime::current().new_owner());
  let offset = cx.signal(0.0_f32);
  let view = ScrollView::new(Stack::new().style(Style::new().width(400.0).height(30.0)))
    .horizontal(true)
    .mouse_drag(false)
    .offset(offset)
    .style(Style::new().width(100.0).height(30.0));
  let bar = PanelWindow::new(&Output::new("FAKE-1"))
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .child(Row::new().child(view));
  let bar = event_loop.platform().add_window(Window::new(&cx, bar));
  frames(&mut event_loop, bar, 1);
  // 30 pixels left, past the threshold: the content follows the finger.
  fake.run(|state| state.tap((50.0, 10.0), -30.0, false));
  wait(&mut event_loop, "the drag", |_| offset.get() == 30.0);
  cx.owner().dispose();
}

#[test]
fn a_finger_drag_selection_is_offered_as_the_primary_selection_at_once() {
  let (fake, mut event_loop) = connect(Globals {
    touch: true,
    ..Globals::default()
  });
  let cx = Cx::new(Runtime::current().new_owner());
  let text = cx.signal(String::from("abcdef"));
  let field = TextInput::new(text).style(Style::new().width(100.0));
  let bar = PanelWindow::new(&Output::new("FAKE-1"))
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .child(Row::new().child(field));
  let bar = event_loop.platform().add_window(Window::new(&cx, bar));
  frames(&mut event_loop, bar, 1);
  let logs = Logs::default();
  let writer = logs.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_writer(move || writer.clone())
    .finish();
  let guard = tracing::subscriber::set_default(subscriber);
  // The fake has no primary selection, so offering it is logged.
  fake.run(|state| state.tap((2.0, 10.0), 90.0, false));
  wait(&mut event_loop, "the offer", |_| {
    logs.text().contains("lacks the primary selection")
  });
  drop(guard);
  let window = event_loop.platform().windows().find(|(id, _)| *id == bar);
  let (_, window) = window.expect("the bar");
  assert_eq!(
    window.take_primary(),
    None,
    "nothing left for a later event"
  );
  cx.owner().dispose();
}

#[test]
fn deactivate_and_remove_go_out_with_a_commit_only_where_the_compositor_takes_them() {
  let (fake, mut event_loop) = connect(Globals {
    workspaces: Some(vec![
      ("one", Can::Activate | Can::Deactivate | Can::Remove),
      ("two", Can::Activate),
    ]),
    ..Globals::default()
  });
  let heard: Rc<RefCell<Vec<Windowset>>> = Rc::default();
  let log = Rc::clone(&heard);
  event_loop
    .platform()
    .watch_windowsets(move |sets, _| *log.borrow_mut() = sets.to_vec());
  wait(&mut event_loop, "the workspaces", |_| {
    heard.borrow().len() == 2
  });
  let (one, two) = {
    let heard = heard.borrow();
    assert!(heard[0].can_deactivate && heard[0].can_remove);
    assert!(!heard[1].can_deactivate && !heard[1].can_remove);
    (heard[0].id, heard[1].id)
  };

  let platform = event_loop.platform();
  platform.deactivate_windowset(one);
  platform.remove_windowset(one);
  platform.deactivate_windowset(two);
  platform.remove_windowset(two);
  wait(&mut event_loop, "the requests", |_| {
    fake.log().last().map(String::as_str) == Some("commit")
  });
  assert_eq!(fake.log(), ["deactivate one", "remove one", "commit"]);

  // Neither request goes out for a windowset that refuses both, and
  // each is logged.
  fake.clear();
  let logs = Logs::default();
  let writer = logs.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_max_level(tracing::Level::DEBUG)
    .with_writer(move || writer.clone())
    .finish();
  let guard = tracing::subscriber::set_default(subscriber);
  let platform = event_loop.platform();
  platform.deactivate_windowset(two);
  platform.remove_windowset(two);
  drop(guard);
  let logged = logs.text();
  for what in ["deactivate", "remove"] {
    let line =
      format!("the compositor doesn't take {what} for this windowset, so it isn't sent id={two}");
    assert!(logged.contains(&line), "{logged}");
  }
  let platform = event_loop.platform();
  platform.activate_windowset(two);
  wait(&mut event_loop, "the activation", |_| {
    fake.log().last().map(String::as_str) == Some("commit")
  });
  assert_eq!(fake.log(), ["activate two", "commit"]);
}

/// What a test logs, as text.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Logs {
  fn text(&self) -> String {
    String::from_utf8_lossy(&self.0.lock().expect("the logs")).into_owned()
  }
}

impl io::Write for Logs {
  fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
    self.0.lock().expect("the logs").extend_from_slice(bytes);
    Ok(bytes.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

/// An output described a read after the first is listed by the time a
/// sync sent after the first is done: Wayland sends the answer after
/// every event before it.
#[test]
fn a_sync_is_done_after_every_event_sent_before_it() {
  let (_fake, mut event_loop) = connect(Globals {
    late_output: true,
    ..Globals::default()
  });
  let names = |event_loop: &mut EventLoop| -> Vec<String> {
    let outputs = event_loop.platform().outputs();
    outputs.map(|output| output.name.clone()).collect()
  };
  wait(&mut event_loop, "the first output", |event_loop| {
    !names(event_loop).is_empty()
  });
  assert_eq!(names(&mut event_loop), ["FAKE-1"], "FAKE-2 comes later");
  let ticket = event_loop.platform().sync();
  assert!(!event_loop.platform().synced(ticket));
  wait(&mut event_loop, "the sync", |event_loop| {
    event_loop.platform().synced(ticket)
  });
  assert_eq!(names(&mut event_loop), ["FAKE-1", "FAKE-2"]);
  assert!(event_loop.platform().synced(ticket - 1), "an older ticket");
}

/// Two bars on `FAKE-1`, along the top and the bottom, 30 high, each
/// holding a 100 by 30 button `build` makes.
fn dnd_bars(build: impl FnOnce(&Cx) -> (Button, Button)) -> (Fake, EventLoop, Cx) {
  let (fake, mut event_loop) = connect(Globals {
    pointer: true,
    data_device: true,
    ..Globals::default()
  });
  let cx = Cx::new(Runtime::current().new_owner());
  let (top, bottom) = build(&cx);
  let size = Style::new().width(100.0).height(30.0);
  for (edge, button) in [(Edge::Top, top), (Edge::Bottom, bottom)] {
    let bar = PanelWindow::new(&Output::new("FAKE-1"))
      .anchors(edge | Edge::Left | Edge::Right)
      .height(30)
      .child(Row::new().child(button.style(size)));
    let bar = event_loop.platform().add_window(Window::new(&cx, bar));
    frames(&mut event_loop, bar, 1);
  }
  (fake, event_loop, cx)
}

/// Waits for the fake to log `line`.
fn logged(fake: &Fake, event_loop: &mut EventLoop, line: &str) {
  wait(event_loop, line, |_| fake.log().iter().any(|l| l == line));
}

/// The serial the fake's last `press_out` pressed with, once it has.
fn press_serial(fake: &Fake, event_loop: &mut EventLoop) -> String {
  let press = || {
    let log = fake.log();
    let press = log
      .iter()
      .rev()
      .find_map(|line| line.strip_prefix("press "));
    press.map(String::from)
  };
  wait(event_loop, "the press", |_| press().is_some());
  press().expect("a press")
}

/// After a drag, once its frames are drawn, nothing wakes the loop: no
/// timer, pipe or callback is left in it.
fn asleep_after(event_loop: &mut EventLoop) {
  run_for(event_loop, Duration::from_millis(50));
  let (start, iterations) = (Instant::now(), event_loop.platform().stats().iterations);
  event_loop
    .dispatch(Some(Duration::from_millis(300)))
    .expect("the connection works");
  assert!(start.elapsed() >= Duration::from_millis(300), "woken early");
  assert_eq!(event_loop.platform().stats().iterations, iterations + 1);
}

type Dropped = Rc<RefCell<Vec<DragContent>>>;

fn content_target(dropped: &Dropped, hovered: crabshell_reactive::Signal<bool>) -> Button {
  let dropped = Rc::clone(dropped);
  Button::new(Text::new("in"))
    .on_drop_content(move |_, content| dropped.borrow_mut().push(content))
    .drop_hovered(hovered)
}

#[test]
fn another_programs_drag_lights_a_button_and_drops_its_uris_there() {
  let dropped = Dropped::default();
  let mut hovered = None;
  let (fake, mut event_loop, cx) = dnd_bars(|cx| {
    let lit = cx.signal(false);
    hovered = Some(lit);
    (
      content_target(&dropped, lit),
      Button::new(Text::new("other")),
    )
  });
  let hovered = hovered.expect("built");
  let list = b"# a comment\r\nfile:///a\r\n\r\nfile:///b c\r\n".to_vec();
  fake.run(move |state| {
    let types: &[&str] = &["text/plain", "text/uri-list"];
    state.drag_enter(0, (150.0, 10.0), Some((types, list)));
  });
  run_for(&mut event_loop, Duration::from_millis(30));
  assert!(!hovered.get(), "beside the button");
  fake.run(|state| state.drag_motion((50.0, 10.0)));
  logged(&fake, &mut event_loop, "accept text/uri-list");
  assert!(hovered.get());
  fake.run(|state| state.drag_drop());
  wait(&mut event_loop, "the drop", |_| {
    !dropped.borrow().is_empty()
  });
  let uris = vec![String::from("file:///a"), String::from("file:///b c")];
  assert_eq!(*dropped.borrow(), [DragContent::Uris(uris)]);
  assert!(!hovered.get());
  logged(&fake, &mut event_loop, "finish");
  let log = fake.log();
  assert!(
    log.contains(&String::from("receive text/uri-list")),
    "{log:?}"
  );
  asleep_after(&mut event_loop);
  cx.owner().dispose();
}

#[test]
fn a_drop_past_its_cap_drops_nothing_and_text_drops_as_text() {
  let dropped = Dropped::default();
  let (fake, mut event_loop, cx) = dnd_bars(|cx| {
    let target = content_target(&dropped, cx.signal(false));
    (target, Button::new(Text::new("other")))
  });
  let mut list = b"file:///".to_vec();
  list.resize(1024 * 1024 + 1, b'a');
  fake.run(move |state| {
    let types: &[&str] = &["text/uri-list"];
    state.drag_enter(0, (50.0, 10.0), Some((types, list)));
  });
  logged(&fake, &mut event_loop, "accept text/uri-list");
  fake.run(|state| state.drag_drop());
  logged(&fake, &mut event_loop, "finish");
  run_for(&mut event_loop, Duration::from_millis(30));
  assert!(dropped.borrow().is_empty(), "over 1 MiB of URIs");

  fake.clear();
  fake.run(|state| {
    let types: &[&str] = &["text/plain;charset=utf-8"];
    state.drag_enter(
      0,
      (50.0, 10.0),
      Some((types, vec![b'a'; 4 * 1024 * 1024 + 1])),
    );
  });
  logged(&fake, &mut event_loop, "accept text/plain;charset=utf-8");
  fake.run(|state| state.drag_drop());
  logged(&fake, &mut event_loop, "finish");
  run_for(&mut event_loop, Duration::from_millis(30));
  assert!(dropped.borrow().is_empty(), "over 4 MiB of text");

  fake.clear();
  fake.run(|state| {
    let types: &[&str] = &["UTF8_STRING"];
    state.drag_enter(0, (50.0, 10.0), Some((types, b"hi".to_vec())));
  });
  logged(&fake, &mut event_loop, "accept UTF8_STRING");
  fake.run(|state| state.drag_drop());
  wait(&mut event_loop, "the text", |_| {
    !dropped.borrow().is_empty()
  });
  assert_eq!(*dropped.borrow(), [DragContent::Text(String::from("hi"))]);
  assert!(fake.log().contains(&String::from("receive UTF8_STRING")));
  cx.owner().dispose();
}

/// Reads everything `stream` gets while the loop runs, until it closes.
fn read_all(event_loop: &mut EventLoop, mut stream: std::os::unix::net::UnixStream) -> Vec<u8> {
  use std::io::Read;
  stream.set_nonblocking(true).expect("non-blocking");
  let (mut bytes, mut chunk) = (Vec::new(), [0; 64]);
  wait(event_loop, "the bytes", |_| match stream.read(&mut chunk) {
    Ok(0) => true,
    Ok(n) => {
      bytes.extend_from_slice(&chunk[..n]);
      false
    }
    Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
    Err(error) => panic!("{error}"),
  });
  bytes
}

/// Asks the drag the shell started for `kind`, as another program would.
fn read_started(fake: &Fake, event_loop: &mut EventLoop, kind: &'static str) -> Vec<u8> {
  let (sent, received) = std::sync::mpsc::channel();
  fake.run(move |state| sent.send(state.read_started(kind)).expect("the test waits"));
  read_all(event_loop, received.recv().expect("a stream"))
}

#[test]
fn a_drag_leaving_its_window_offers_its_text_and_ends_when_taken() {
  let ends: Rc<RefCell<Vec<bool>>> = Rc::default();
  let (fake, mut event_loop, cx) = dnd_bars(|_| {
    let ends = Rc::clone(&ends);
    let source = Button::new(Text::new("drag"))
      .drag_content(Some(DragContent::Text(String::from("hello"))))
      .on_drag(move |_, drag| {
        if drag.phase == DragPhase::End {
          ends.borrow_mut().push(drag.dropped);
        }
      });
    (source, Button::new(Text::new("other")))
  });
  fake.run(|state| state.press_out(0, (10.0, 10.0), (10.0, -20.0)));
  let started = format!(
    "start_drag {} application/x-crabshell-drag,text/plain;charset=utf-8,UTF8_STRING",
    press_serial(&fake, &mut event_loop)
  );
  logged(&fake, &mut event_loop, &started);
  fake.run(|state| state.pointer_leave(0));
  run_for(&mut event_loop, Duration::from_millis(30));
  assert!(ends.borrow().is_empty(), "the leave doesn't end it");

  assert_eq!(
    read_started(&fake, &mut event_loop, "UTF8_STRING"),
    b"hello"
  );
  let private = read_started(&fake, &mut event_loop, "application/x-crabshell-drag");
  assert!(private.is_empty(), "the private type sends nothing");
  fake.run(|state| state.take_started());
  wait(&mut event_loop, "the end", |_| !ends.borrow().is_empty());
  assert_eq!(*ends.borrow(), [true]);
  logged(&fake, &mut event_loop, "source destroyed");
  cx.owner().dispose();
}

#[test]
fn a_typed_drag_crosses_from_one_window_to_a_button_in_another() {
  let ends: Rc<RefCell<Vec<bool>>> = Rc::default();
  let drops: Rc<RefCell<Vec<u32>>> = Rc::default();
  let mut hovered = None;
  let (fake, mut event_loop, cx) = dnd_bars(|cx| {
    let (ends, drops) = (Rc::clone(&ends), Rc::clone(&drops));
    let lit = cx.signal(false);
    hovered = Some(lit);
    let source = Button::new(Text::new("drag"))
      .drag_data(Some(7_u32))
      .on_drag(move |_, drag| {
        if drag.phase == DragPhase::End {
          ends.borrow_mut().push(drag.dropped);
        }
      });
    let target = Button::new(Text::new("in"))
      .on_drop(move |_, value: u32| drops.borrow_mut().push(value))
      .drop_hovered(lit);
    (source, target)
  });
  let hovered = hovered.expect("built");
  fake.run(|state| state.press_out(0, (10.0, 10.0), (10.0, 40.0)));
  let started = format!(
    "start_drag {} application/x-crabshell-drag",
    press_serial(&fake, &mut event_loop)
  );
  logged(&fake, &mut event_loop, &started);
  fake.run(|state| {
    state.pointer_leave(0);
    state.drag_enter(1, (50.0, 10.0), None);
  });
  logged(
    &fake,
    &mut event_loop,
    "accept application/x-crabshell-drag",
  );
  assert!(hovered.get());
  fake.run(|state| state.drag_drop());
  wait(&mut event_loop, "the drop", |_| !drops.borrow().is_empty());
  assert_eq!(*drops.borrow(), [7]);
  assert_eq!(*ends.borrow(), [true]);
  assert!(!hovered.get());
  logged(&fake, &mut event_loop, "finish");
  asleep_after(&mut event_loop);
  cx.owner().dispose();
}

#[test]
fn a_source_removed_mid_drag_cancels_the_drag() {
  let mut shown = None;
  let (fake, mut event_loop, cx) = dnd_bars(|cx| {
    let on = cx.signal(true);
    shown = Some(on);
    let source = Button::new(Text::new("drag"))
      .drag_data(Some(7_u32))
      .visible(move || on.get());
    (source, Button::new(Text::new("other")))
  });
  fake.run(|state| state.press_out(0, (10.0, 10.0), (10.0, 40.0)));
  let started = format!(
    "start_drag {} application/x-crabshell-drag",
    press_serial(&fake, &mut event_loop)
  );
  logged(&fake, &mut event_loop, &started);
  shown.expect("built").set(false);
  logged(&fake, &mut event_loop, "source destroyed");
  cx.owner().dispose();
}
