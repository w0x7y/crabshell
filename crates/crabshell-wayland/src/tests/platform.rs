//! What the platform refuses before it starts.

use crate::platform::{Error, require_version};

#[test]
fn a_compositor_without_damage_buffer_is_refused() {
  // wl_surface.damage_buffer came in wl_compositor version 4.
  assert_eq!(
    require_version("wl_compositor", 3, 4),
    Err(Error::TooOld {
      global: "wl_compositor",
      version: 3,
      needed: 4,
    })
  );
  assert_eq!(require_version("wl_compositor", 4, 4), Ok(()));
  assert_eq!(require_version("wl_compositor", 6, 4), Ok(()));
  assert_eq!(
    require_version("wl_compositor", 1, 4)
      .map_err(|error| error.to_string())
      .unwrap_err(),
    "the compositor offers wl_compositor version 1, but Crabshell needs 4"
  );
}

#[test]
fn animations_tick_once_a_frame_of_the_fastest_output() {
  use std::time::Duration;

  use crate::platform::frame_interval;

  assert_eq!(
    frame_interval([60_000, 144_000].into_iter()),
    Duration::from_micros(6_944)
  );
  assert_eq!(
    frame_interval([].into_iter()),
    Duration::from_micros(16_666)
  );
  // Outputs with no refresh rate count as 60 Hz.
  assert_eq!(
    frame_interval([0].into_iter()),
    Duration::from_micros(16_666)
  );
}

#[test]
fn a_panel_sized_from_its_content_settles_before_it_has_a_surface() {
  use crabshell_reactive::Runtime;
  use crabshell_ui::{Column, Cx, Edge, Rect, Size, Style, TextSystem, Window};

  use crate::platform::{ask_size, lay_out_sized};

  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let content = cx.signal(Rect::default());
  // The content doesn't shrink, so a window smaller than it can't squeeze it.
  let panel = super::panel("DP-1")
    .anchors(Edge::Top | Edge::Right)
    .width(300)
    .height(move || content.get().height as u32 + 8)
    .child(
      Column::new().child(
        Column::new()
          .geometry(content)
          .style(Style::new().height(40.0).shrink(0.0)),
      ),
    );
  let window = Window::new(&cx, panel);
  runtime.flush();
  // Before any layout the content is 0 tall, a height layer-shell refuses.
  assert_eq!(window.surface().height, 8);
  let output = (1920, 1080);
  ask_size(&window, output);
  let logs = super::capture_logs(|| {
    lay_out_sized(&window, &mut TextSystem::new(), |window| {
      ask_size(window, output)
    });
  });
  assert_eq!(window.surface().height, 48);
  assert_eq!(window.size(), Size::new(300.0, 48.0));
  assert!(!window.needs_layout());
  assert!(logs.is_empty(), "{logs}");
  cx.owner().dispose();
}

#[test]
fn a_size_that_never_settles_stops_after_a_few_layouts_and_is_logged() {
  use crabshell_reactive::Runtime;
  use crabshell_ui::{Column, Cx, Edge, Rect, TextSystem, Window};

  use crate::platform::{SIZE_ROUNDS, ask_size, lay_out_sized};

  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  // The child fills the window, so each size asks for one pixel more.
  let filled = cx.signal(Rect::default());
  let panel = super::panel("DP-1")
    .anchors(Edge::Top | Edge::Right)
    .width(300)
    .height(move || filled.get().height as u32 + 1)
    .child(Column::new().geometry(filled));
  let window = Window::new(&cx, panel);
  runtime.flush();
  let output = (1920, 1080);
  ask_size(&window, output);
  let logs = super::capture_logs(|| {
    lay_out_sized(&window, &mut TextSystem::new(), |window| {
      ask_size(window, output)
    });
  });
  assert!(
    logs.contains("PanelWindow[DP-1]")
      && logs.contains("a window's size still changed after 4 layouts"),
    "{logs}"
  );
  // One pixel a layout, and the last layout is at the last size given.
  assert_eq!(window.size().height, 1.0 + SIZE_ROUNDS as f32);
  assert!(!window.needs_layout());
  cx.owner().dispose();
}
