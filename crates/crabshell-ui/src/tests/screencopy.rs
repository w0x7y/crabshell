//! Screencopy views: what the platform captures for, and the frames it
//! shows.

use std::rc::Rc;

use super::{Harness, capture_logs, node_at};
use crate::{
  CaptureFrame, CaptureRequest, CaptureSource, Content, Row, ScreencopyView, Size, Style,
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
