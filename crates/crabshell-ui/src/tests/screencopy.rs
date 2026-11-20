//! Screencopy views: what the platform captures for, and the frames it
//! shows.

use std::rc::Rc;

use super::{Harness, capture_logs, node_at};
use crate::{
  CaptureFrame, CaptureRequest, CaptureSource, Content, Rect, Row, ScreencopyView, Size, Style,
  deliver_capture, wanted_captures,
};

const BOX: Style = Style::new().width(100.0).height(50.0);

fn output(name: &str) -> CaptureSource {
  CaptureSource::Output(name.to_owned())
}

/// A frame of one gray.
fn frame(gray: u8) -> CaptureFrame {
  CaptureFrame {
    width: 2,
    height: 1,
    logical_width: 2,
    logical_height: 1,
    rgba: Rc::from([gray, gray, gray, 255].repeat(2)),
  }
}

/// The frame the view at `path` shows, if any.
fn shown(window: &crate::Window, path: &[usize]) -> Option<CaptureFrame> {
  match node_at(window.view().root(), path).content() {
    Content::Capture(capture) => Some(capture.frame.clone()),
    _ => None,
  }
}

#[test]
fn a_view_asks_for_its_source_and_shows_each_new_frame_alone() {
  let mut h = Harness::new();
  let source = h.cx.signal(output("DP-2"));
  let live = h.cx.signal(false);
  let row = Row::new()
    .child(Row::new().style(Style::new().width(40.0)))
    .child(
      ScreencopyView::new(move || source.get())
        .live(move || live.get())
        .style(BOX),
    );
  let window = h.window(h.panel().child(row), 200.0, 50.0);
  let requests = window.captures();
  let [request] = &requests[..] else {
    panic!("{requests:?}");
  };
  assert_eq!(
    *request,
    CaptureRequest {
      id: request.id,
      source: output("DP-2"),
      live: false,
      paint_cursors: false,
      size: Size::new(100.0, 50.0),
    }
  );
  // Nothing shows before the first frame.
  assert_eq!(shown(&window, &[0, 1]), None);
  window.take_damage();

  window.show_capture(request.id, frame(10));
  assert_eq!(shown(&window, &[0, 1]), Some(frame(10)));
  let view = node_at(window.view().root(), &[0, 1]).bounds();
  assert_eq!(window.take_damage(), [view], "only the view repaints");
  // The same pixels again repaint nothing.
  window.show_capture(request.id, frame(10));
  assert!(window.take_damage().is_empty());
  // An id no view has is ignored.
  window.show_capture(request.id + 1000, frame(20));
  assert_eq!(shown(&window, &[0, 1]), Some(frame(10)));

  live.set(true);
  source.set(output("DP-3"));
  h.frame(&window);
  assert_eq!(shown(&window, &[0, 1]), None, "the old source's frame goes");
  assert_eq!(window.take_damage(), [view]);
  let requests = window.captures();
  assert_eq!(requests[0].id, request.id, "still the same view");
  assert_eq!(requests[0].source, output("DP-3"));
  assert!(requests[0].live);
}

#[test]
fn views_without_a_box_or_gone_from_the_window_arent_captured() {
  let mut h = Harness::new();
  let shown_views = h.cx.signal(vec![1, 2]);
  let row = Row::new()
    // No width and no height: nothing to show in.
    .child(ScreencopyView::new(CaptureSource::Toplevel(7)))
    .children_keyed(
      move || shown_views.get(),
      |n| *n,
      |_, _| ScreencopyView::new(CaptureSource::Toplevel(9)).style(BOX),
    );
  let window = h.window(h.panel().child(row), 300.0, 50.0);
  let ids: Vec<u64> = window.captures().iter().map(|request| request.id).collect();
  assert_eq!(ids.len(), 2, "{ids:?}");
  assert!(ids[0] != ids[1], "each view has its own id");

  shown_views.set(vec![2]);
  h.frame(&window);
  let left: Vec<u64> = window.captures().iter().map(|request| request.id).collect();
  assert_eq!(left, [ids[1]]);
  shown_views.set(Vec::new());
  h.frame(&window);
  assert!(window.captures().is_empty());
  // Only the view without a box is still kept.
  assert_eq!(window.tree.borrow().captures.len(), 1);
}

#[test]
fn a_panicking_source_is_logged_with_its_path_and_the_rest_goes_on() {
  let mut h = Harness::new();
  let fail = h.cx.signal(false);
  let failing = ScreencopyView::new(move || {
    assert!(!fail.get(), "source failure");
    output("DP-2")
  });
  let row = Row::new()
    .child(failing.style(BOX))
    .child(ScreencopyView::new(output("DP-3")).style(BOX));
  let window = h.window(h.panel().child(row), 300.0, 50.0);
  let logs = capture_logs(|| {
    fail.set(true);
    h.frame(&window);
  });
  assert!(
    logs.contains("effect PanelWindow[DP-1]/Row/ScreencopyView[0].source panicked and is disabled"),
    "{logs}"
  );
  // The view keeps its last source, and the other is untouched.
  let sources: Vec<CaptureSource> = window
    .captures()
    .into_iter()
    .map(|request| request.source)
    .collect();
  assert_eq!(sources, [output("DP-2"), output("DP-3")]);
}

#[test]
fn a_live_view_out_of_sight_isnt_captured_until_it_comes_back() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0_f32);
  let cut = h.cx.signal(true);
  let column = crate::Column::new()
    .child(ScreencopyView::new(output("DP-2")).live(true).style(BOX))
    .child(ScreencopyView::new(output("DP-3")).style(BOX));
  let scrolled = crate::ScrollView::new(column)
    .offset(offset)
    .style(Style::new().width(100.0).height(50.0));
  // A clipping box 10 pixels wide, whose live view lies past its edge.
  let clipped = Row::new()
    .style(move || Style::new().width(10.0).height(50.0).clip(cut.get()))
    .child(Row::new().style(Style::new().width(20.0).shrink(0.0)))
    .child(
      ScreencopyView::new(output("DP-4"))
        .live(true)
        .style(BOX.shrink(0.0)),
    );
  let row = Row::new().child(scrolled).child(clipped);
  let window = h.window(h.panel().child(row), 300.0, 50.0);
  let sources = |window: &crate::Window| -> Vec<CaptureSource> {
    window
      .captures()
      .into_iter()
      .map(|request| request.source)
      .collect()
  };
  // DP-3 is below the scroll view's box, but one-shot; DP-4 is clipped.
  assert_eq!(sources(&window), [output("DP-2"), output("DP-3")]);

  offset.set(50.0);
  h.frame(&window);
  assert_eq!(sources(&window), [output("DP-3")], "scrolled out of sight");
  offset.set(10.0);
  h.frame(&window);
  assert_eq!(sources(&window), [output("DP-2"), output("DP-3")]);

  cut.set(false);
  h.frame(&window);
  assert_eq!(
    sources(&window),
    [output("DP-2"), output("DP-3"), output("DP-4")],
    "no longer clipped"
  );
}

#[test]
fn a_view_scaled_up_asks_for_a_frame_as_large_as_it_is_drawn() {
  let mut h = Harness::new();
  let outer = h.cx.signal(1.0_f32);
  let view = ScreencopyView::new(output("DP-2")).style(BOX.scale(1.5));
  let row = Row::new()
    .style(move || Style::new().scale(outer.get()).rotate(30.0))
    .child(view);
  let window = h.window(h.panel().child(row), 400.0, 300.0);
  let size = |window: &crate::Window| window.captures()[0].size;
  // 1.5, turned, draws its layer at 1.5.
  assert_eq!(size(&window), Size::new(150.0, 75.0));
  // 1.5 times 2 is 3; a scale-down draws at its own size, and anything
  // else rounds up to a quarter.
  for (scale, zoom) in [(2.0, 3.0), (0.5, 1.0), (0.6, 1.0), (0.8, 1.25), (9.0, 4.0)] {
    outer.set(scale);
    h.frame(&window);
    assert_eq!(
      size(&window),
      Size::new(100.0 * zoom, 50.0 * zoom),
      "outer scale {scale}"
    );
  }
}

/// The one capture waiting for a frame: the number it is asked under.
fn asked_for(source: &CaptureSource) -> u64 {
  let wanted = wanted_captures();
  let [(asked, wanted_source)] = &wanted[..] else {
    panic!("{wanted:?}");
  };
  assert_eq!(wanted_source, source);
  *asked
}

#[test]
fn views_of_a_capture_share_its_one_frame_and_refresh_asks_again() {
  let mut h = Harness::new();
  let source = h.cx.signal(output("DP-2"));
  let capture = h.cx.capture(move || source.get());
  let ready = capture.ready();
  let row = Row::new()
    .child(ScreencopyView::of(capture).style(BOX))
    .child(ScreencopyView::of(capture).style(BOX));
  let window = h.window(h.panel().child(row), 200.0, 50.0);
  assert!(window.captures().is_empty(), "the views capture nothing");
  let asked = asked_for(&output("DP-2"));
  assert!(!ready.get());
  window.take_damage();

  assert!(deliver_capture(asked, frame(10)));
  assert!(wanted_captures().is_empty(), "one frame");
  h.frame(&window);
  assert!(ready.get());
  assert_eq!(shown(&window, &[0, 0]), Some(frame(10)));
  assert_eq!(shown(&window, &[0, 1]), Some(frame(10)));
  let boxes = [[0, 0], [0, 1]].map(|path| node_at(window.view().root(), &path).bounds());
  assert_eq!(window.take_damage(), boxes, "only the views repaint");

  // The old frame stays until the new one lands.
  capture.refresh();
  let again = asked_for(&output("DP-2"));
  assert_ne!(again, asked);
  h.frame(&window);
  assert!(!ready.get());
  assert_eq!(shown(&window, &[0, 1]), Some(frame(10)));
  assert!(
    !deliver_capture(asked, frame(30)),
    "asked before the refresh"
  );
  assert!(deliver_capture(again, frame(20)));
  h.frame(&window);
  assert!(ready.get());
  assert_eq!(shown(&window, &[0, 0]), Some(frame(20)));
  assert_eq!(shown(&window, &[0, 1]), Some(frame(20)));

  // A new source drops the frame.
  source.set(output("DP-3"));
  h.frame(&window);
  assert!(!ready.get());
  assert_eq!(shown(&window, &[0, 0]), None);
  asked_for(&output("DP-3"));
}

#[test]
fn a_capture_goes_with_its_context() {
  let mut h = Harness::new();
  let before = h.runtime.node_count();
  let cx = h.cx.child();
  let capture = cx.capture(output("DP-2"));
  let window = h.window(
    h.panel().child(ScreencopyView::of(capture).style(BOX)),
    100.0,
    50.0,
  );
  let asked = asked_for(&output("DP-2"));
  assert!(deliver_capture(asked, frame(10)));
  h.frame(&window);
  drop(window);
  cx.owner().dispose();
  assert_eq!(h.runtime.node_count(), before);
  assert!(wanted_captures().is_empty());
  // A handle outliving it does nothing.
  capture.refresh();
  assert!(wanted_captures().is_empty());
  assert!(!deliver_capture(asked, frame(10)));
}

#[test]
fn a_panicking_capture_source_is_logged_where_it_was_made_and_the_rest_goes_on() {
  let mut h = Harness::new();
  let fail = h.cx.signal(false);
  let failing = h.cx.capture(move || {
    assert!(!fail.get(), "capture source failure");
    output("DP-2")
  });
  let other = h.cx.capture(output("DP-3"));
  let row = Row::new()
    .child(ScreencopyView::of(failing).style(BOX))
    .child(ScreencopyView::of(other).style(BOX));
  let window = h.window(h.panel().child(row), 300.0, 50.0);
  let logs = capture_logs(|| {
    fail.set(true);
    h.frame(&window);
  });
  assert!(
    logs.contains("effect cx.capture source at crates/crabshell-ui/src/tests/screencopy.rs:"),
    "{logs}"
  );
  assert!(logs.contains("panicked and is disabled"), "{logs}");
  // It keeps its source, and the other capture is untouched.
  let sources: Vec<CaptureSource> = wanted_captures()
    .into_iter()
    .map(|(_, source)| source)
    .collect();
  assert_eq!(sources, [output("DP-2"), output("DP-3")]);
  let asked = wanted_captures()[1].0;
  assert!(deliver_capture(asked, frame(10)));
  h.frame(&window);
  assert_eq!(shown(&window, &[0, 1]), Some(frame(10)));
}

#[test]
fn a_crop_draws_that_part_and_asks_for_the_whole_frame() {
  let mut h = Harness::new();
  let part = h.cx.signal(None);
  let view = ScreencopyView::new(output("DP-2"))
    .crop(move || part.get())
    .style(BOX);
  let window = h.window(h.panel().child(view), 100.0, 50.0);
  assert_eq!(window.captures()[0].size, Size::new(100.0, 50.0));
  let id = window.captures()[0].id;
  window.show_capture(id, frame(10));
  window.take_damage();

  let corner = Rect::new(0.0, 0.0, 1.0, 1.0);
  part.set(Some(corner));
  h.frame(&window);
  let crop = match node_at(window.view().root(), &[0]).content() {
    Content::Capture(capture) => capture.crop,
    _ => panic!("no frame"),
  };
  assert_eq!(crop, Some(corner));
  let bounds = node_at(window.view().root(), &[0]).bounds();
  assert_eq!(window.take_damage(), [bounds]);
  assert_eq!(
    window.captures()[0].size,
    Size::new(f32::INFINITY, f32::INFINITY),
    "the whole frame"
  );
}

#[test]
fn shown_turns_true_once_the_commit_that_drew_the_picture_is_presented() {
  let mut h = Harness::new();
  let on_screen = h.cx.signal(true);
  let visible = h.cx.signal(true);
  let view = ScreencopyView::new(output("DP-2"))
    .shown(on_screen)
    .visible(move || visible.get())
    .style(BOX);
  let window = h.window(h.panel().child(Row::new().child(view)), 100.0, 50.0);
  assert!(!on_screen.get(), "no picture yet");
  // A commit before the picture came draws nothing of it.
  window.frame_committed();
  let id = window.captures()[0].id;
  window.show_capture(id, frame(10));
  window.frame_presented();
  assert!(!on_screen.get(), "presented a commit from before it");
  window.frame_committed();
  assert!(!on_screen.get(), "committed, not yet presented");
  window.frame_presented();
  assert!(on_screen.get());

  // A new picture is off screen until it is presented.
  window.show_capture(id, frame(20));
  assert!(!on_screen.get());
  window.frame_committed();
  window.frame_presented();
  assert!(on_screen.get());

  visible.set(false);
  h.frame(&window);
  window.frame_committed();
  assert!(!on_screen.get(), "hidden");
  visible.set(true);
  h.frame(&window);
  window.frame_committed();
  window.frame_presented();
  assert!(on_screen.get());

  window.unmapped();
  assert!(!on_screen.get(), "the surface went");
  window.frame_committed();
  window.frame_presented();
  assert!(on_screen.get());
  drop(window);
  assert!(!on_screen.get(), "the window went");
}

#[test]
fn a_view_removed_while_shown_writes_false() {
  let mut h = Harness::new();
  let on_screen = h.cx.signal(false);
  let keys = h.cx.signal(vec![1]);
  let row = Row::new().children_keyed(
    move || keys.get(),
    |key| *key,
    move |_, _| {
      ScreencopyView::new(output("DP-2"))
        .shown(on_screen)
        .style(BOX)
    },
  );
  let window = h.window(h.panel().child(row), 100.0, 50.0);
  window.show_capture(window.captures()[0].id, frame(10));
  window.frame_committed();
  window.frame_presented();
  assert!(on_screen.get());
  keys.set(Vec::new());
  h.frame(&window);
  assert!(!on_screen.get());
}

#[test]
fn a_view_that_draws_nothing_isnt_shown() {
  let mut h = Harness::new();
  let on_screen = h.cx.signal(false);
  let part = h.cx.signal(Some(Rect::new(5.0, 0.0, 1.0, 1.0)));
  let view = ScreencopyView::new(output("DP-2"))
    .crop(move || part.get())
    .shown(on_screen)
    .style(BOX);
  let window = h.window(h.panel().child(view), 100.0, 50.0);
  let id = window.captures()[0].id;
  let present = |window: &crate::Window| {
    window.frame_committed();
    window.frame_presented();
  };
  window.show_capture(id, frame(10));
  present(&window);
  assert!(!on_screen.get(), "a crop outside the frame");
  part.set(Some(Rect::new(f32::NAN, 0.0, 1.0, 1.0)));
  h.frame(&window);
  present(&window);
  assert!(!on_screen.get(), "a crop that isn't finite");
  part.set(Some(Rect::new(1.0, 0.0, 1.0, 1.0)));
  h.frame(&window);
  present(&window);
  assert!(on_screen.get(), "the frame's right pixel");

  // Pixels that don't fill the frame's size draw nothing.
  part.set(None);
  h.frame(&window);
  let short = CaptureFrame {
    rgba: Rc::from([0; 4]),
    ..frame(20)
  };
  window.show_capture(id, short);
  present(&window);
  assert!(!on_screen.get(), "a frame missing pixels");
}
