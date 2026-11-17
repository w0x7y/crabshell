//! The platform against a fake compositor, for protocols no compositor
//! here offers or lets a test drive: blur behind windows and touch. They
//! run by default.

mod fake;

use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use crabshell_ui::{Button, Color, Cx, Edge, Output, PanelWindow, Region, Style, Text, Window};
use crabshell_wayland::{EventLoop, WindowId};
use fake::{Fake, Globals};
use smithay_client_toolkit::reexports::client::Connection;

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
