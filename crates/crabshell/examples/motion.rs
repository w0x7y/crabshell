//! Time and motion: a volume OSD that slides up from the bottom of each
//! screen when the volume changes and hides 1.5 s after the last change, a
//! spinner in the bar that turns while a program runs, and notifications
//! that slide in at the top right and slide out when they expire or are
//! clicked. Nothing moves, and nothing wakes, between changes.
//!
//! ```sh
//! cargo run --release -p crabshell --example motion
//! crabshell ipc motion run sleep 3
//! ```
//!
//! `crabshell ipc osd pin` keeps the OSD up until `crabshell ipc osd unpin`.
//! Scrolling on the OSD changes the volume.

use std::cell::RefCell;
use std::rc::Rc;

use crabshell::prelude::*;

const TEXT: Color = Color::rgb(0xcd, 0xd6, 0xf4);
const ACCENT: Color = Color::rgb(0x89, 0xb4, 0xfa);
const BAR: Style = Style::new()
  .background(Color::rgb(0x1e, 0x1e, 0x2e))
  .color(TEXT)
  .font(Font::new("sans-serif", 13.0));
/// A disc that fades out across, so its turning shows.
const SPINNER: Style = Style::new()
  .gradient(Gradient::linear(
    90.0,
    ACCENT,
    Color::rgba(0x89, 0xb4, 0xfa, 0x20),
  ))
  .radius(7.0)
  .width(14.0)
  .height(14.0);
/// One turn of the spinner.
const SPIN: Transition = Transition::new(900).easing(Easing::Linear);

/// The OSD window's size; the card sits in the middle.
const OSD_WIDTH: u32 = 280;
const OSD_HEIGHT: u32 = 96;
/// How far below its place the card waits while hidden: past the
/// window's bottom edge, so none of it shows.
const OSD_DROP: f32 = 80.0;
const OSD_HIDE: Duration = Duration::from_millis(1500);
/// Back runs a little past the end, so the card pops into place.
const OSD_SLIDE: Transition = Transition::new(300).easing(Easing::OutBack);
const CARD: Style = Style::new()
  .background(Color::rgb(0x31, 0x32, 0x44))
  .color(TEXT)
  .font(Font::new("sans-serif", 13.0))
  .radius(12.0)
  .padding(12, 16);
const TRACK: Style = Style::new()
  .background(Color::rgb(0x45, 0x47, 0x5a))
  .radius(3.0)
  .width(200.0)
  .height(6.0);
const FILL: Style = Style::new().background(ACCENT).radius(3.0).height(6.0);

const TOAST_WIDTH: u32 = 340;
const TOASTS_HEIGHT: u32 = 240;
/// A toast's slide, in and out; its exit lasts as long.
const TOAST_SLIDE_MS: u32 = 250;
const TOAST: Style = Style::new()
  .background(Color::rgb(0x31, 0x32, 0x44))
  .color(TEXT)
  .radius(8.0)
  .padding(8, 12)
  .width(TOAST_WIDTH as f32);
/// A toast's text: as wide as the toast less its padding, so it starts at
/// the left instead of in the middle, as a button puts it.
const TOAST_TEXT: Style = Style::new().width(TOAST_WIDTH as f32 - 24.0);
const SUMMARY: Style = Style::new().font(Font::new("sans-serif", 13.0).weight(700));

#[allow(dead_code, reason = "the tests build the windows, not main")]
fn main() {
  crabshell::run(setup);
}

/// The bar, the OSD and the toasts on every output.
pub fn setup(cx: &Cx) {
  let pinned = cx.signal(false);
  cx.ipc("osd", "pin", move |_| {
    pinned.set(true);
    Ok(String::new())
  });
  cx.ipc("osd", "unpin", move |_| {
    pinned.set(false);
    Ok(String::new())
  });
  let busy = work(cx);
  let sink = cx.audio().default_sink();
  let popups = cx.notifications().popups();
  cx.per_output(move |cx, output| {
    vec![
      bar(cx, output, busy),
      osd(cx, output, sink, pinned.read_only()),
      toasts(output, popups),
    ]
  });
}

/// Runs the program `crabshell ipc motion run` names, and says whether it
/// still runs. A new call stops the one before.
fn work(cx: &Cx) -> ReadSignal<bool> {
  let busy = cx.signal(false);
  // Each run gets a context of its own; disposing it stops the program.
  let run: Rc<RefCell<Option<Cx>>> = Rc::default();
  let parent = cx.clone();
  cx.ipc("motion", "run", move |args| {
    if args.is_empty() {
      return Err("run takes a program and its arguments".to_owned());
    }
    let old = run.borrow_mut().take();
    if let Some(old) = old {
      old.owner().dispose();
    }
    let child = parent.child();
    let command = args.to_vec();
    let program = child.process(move || command.clone());
    child.effect(move || busy.set(program.running().get()));
    *run.borrow_mut() = Some(child);
    Ok(String::new())
  });
  busy.read_only()
}

/// The bar on `output`: a spinner that turns while `busy`, and what it
/// does.
pub fn bar(cx: &Cx, output: &Output, busy: ReadSignal<bool>) -> PanelWindow {
  // Once `busy` turns false, the turn finishes and rests at 0.
  let turn = cx.cycle(move || busy.get(), SPIN);
  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .style(BAR)
    .child(
      Row::new()
        .gap(8)
        .padding(4, 10)
        .align(Align::Center)
        .child(Row::new().style(move || SPINNER.rotate(turn.get() * 360.0)))
        .child(Text::new(move || {
          if busy.get() { "working" } else { "idle" }.to_owned()
        })),
    )
}

/// The volume of `sink` near the bottom of `output`, shown on each change
/// and while `pinned`.
pub fn osd(
  cx: &Cx,
  output: &Output,
  sink: ReadSignal<Option<Sink>>,
  pinned: ReadSignal<bool>,
) -> PanelWindow {
  // The OSD shows while this runs; each change starts it again.
  let hide = cx.after(OSD_HIDE, || {}).stopped();
  let mut last = None;
  cx.effect(move || {
    let now = sink.with(|sink| sink.as_ref().map(|sink| (sink.volume_percent, sink.muted)));
    // The first value is where the volume was at startup, not a change.
    if now.is_some() && last.is_some() && now != last {
      hide.restart();
    }
    last = now;
  });
  let shown = move || hide.running() || pinned.get();
  let slide = cx.animation(move || if shown() { 1.0 } else { 0.0 }, OSD_SLIDE);
  // Fully out: let clicks and scrolls through to what is below.
  let out = move || !shown() && !slide.running().get();
  let card_style = move || {
    let at = slide.value().get();
    CARD
      .translate(0.0, (1.0 - at) * OSD_DROP)
      .scale(0.9 + 0.1 * at)
  };
  let fill_style = move || {
    let percent = sink.with(|sink| match sink {
      Some(sink) if !sink.muted => sink.volume_percent.min(100),
      _ => 0,
    });
    FILL.width(2.0 * percent as f32)
  };
  PanelWindow::new(output)
    .anchors(Edge::Bottom)
    .layer(Layer::Overlay)
    .width(OSD_WIDTH)
    .height(OSD_HEIGHT)
    // Over the windows, without pushing them up.
    .exclusive_zone(0)
    .namespace("osd")
    .mask(move || out().then(Region::empty))
    .child(
      Column::new()
        .align(Align::Center)
        .justify(Justify::Center)
        .child(
          Button::new(
            Column::new()
              .gap(8)
              .child(Text::new(move || match sink.get() {
                Some(sink) if sink.muted => "muted".to_owned(),
                Some(sink) => format!("volume {}%", sink.volume_percent),
                None => "no audio".to_owned(),
              }))
              .child(Row::new().style(TRACK).child(Row::new().style(fill_style))),
          )
          .style(card_style)
          .on_scroll(|cx, steps| cx.audio().change_volume(steps * 5)),
        ),
    )
}

/// The notifications in `popups`, top right, newest first. Each slides in
/// from the right, and out again when it goes.
pub fn toasts(output: &Output, popups: ReadSignal<Vec<Notification>>) -> PanelWindow {
  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Right)
    .layer(Layer::Overlay)
    .margins(Margins::new(36, 8, 0, 0))
    .width(TOAST_WIDTH)
    .height(TOASTS_HEIGHT)
    .exclusive_zone(-1)
    .namespace("toasts")
    .mask(move || {
      // Room for each toast, at most 64 pixels tall with its gap.
      let tall = (popups.with(Vec::len) as u32 * 64).min(TOASTS_HEIGHT);
      Some(Region::rect(0, 0, TOAST_WIDTH, tall))
    })
    .child(
      Column::new()
        .gap(8)
        .children_keyed(
          move || popups.get(),
          |notification: &Notification| notification.id,
          toast,
        )
        .exit(Duration::from_millis(TOAST_SLIDE_MS.into())),
    )
}

/// One notification, which slides in when made and out while it leaves.
/// A click dismisses it.
fn toast(cx: &Cx, notification: Memo<Notification>) -> Button {
  let entered = cx.signal(false);
  let leaving = cx.leaving();
  let x = cx.animate(
    move || {
      if entered.get() && !leaving.get() {
        0.0
      } else {
        TOAST_WIDTH as f32
      }
    },
    Transition::new(TOAST_SLIDE_MS),
  );
  // After the animation took its first value, so the next flush slides it.
  entered.set(true);
  Button::new(
    Column::new()
      .gap(4)
      .style(TOAST_TEXT)
      .child(Text::new(move || notification.get().summary).style(SUMMARY))
      .child(Text::new(move || notification.get().body)),
  )
  .style(move || TOAST.translate(x.get(), 0.0))
  .on_click(move |cx| cx.notifications().dismiss(notification.get_untracked().id))
}
