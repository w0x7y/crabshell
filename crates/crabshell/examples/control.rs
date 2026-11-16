//! A control center at the top right of each screen: a volume slider on
//! the default sink, quick-setting chips that wrap onto a second line,
//! and this month's calendar with today marked.
//!
//! ```sh
//! cargo run --release -p crabshell --example control
//! ```
//!
//! The chips are plain toggles here, a place to hang a config's own
//! services. `CONTROL_TODAY=2026-11-16` pins the calendar's date, as the
//! tests do.

use crabshell::jiff::{Zoned, civil::Date};
use crabshell::prelude::*;

const TEXT: Color = Color::rgb(0xcd, 0xd6, 0xf4);
const DIM: Color = Color::rgb(0x7f, 0x84, 0x9c);
const ACCENT: Color = Color::rgb(0x89, 0xb4, 0xfa);
const DARK: Color = Color::rgb(0x1e, 0x1e, 0x2e);
const SURFACE: Color = Color::rgb(0x31, 0x32, 0x44);
const RAISED: Color = Color::rgb(0x45, 0x47, 0x5a);

/// The panel's size; it sits 8 pixels in from the top right.
pub const WIDTH: u32 = 360;
pub const HEIGHT: u32 = 460;
/// The space inside the panel's edges, so its content is this much
/// narrower on each side.
pub const PADDING: u32 = 16;
const PANEL: Style = Style::new()
  .background(DARK)
  .color(TEXT)
  .font(Font::new("sans-serif", 13.0))
  .radius(14.0)
  .border(1.0, RAISED);
const HEADING: Style = Style::new().font(Font::new("sans-serif", 13.0).weight(700));

/// The slider's track, as wide as the panel's content.
pub const TRACK_WIDTH: f32 = (WIDTH - 2 * PADDING) as f32;
const TRACK: Style = Style::new()
  .background(RAISED)
  .radius(3.0)
  .width(TRACK_WIDTH)
  .height(6.0);
const FILL: Style = Style::new().background(ACCENT).radius(3.0);
pub const HANDLE: f32 = 18.0;
const HANDLE_STYLE: Style = Style::new()
  .background(TEXT)
  .radius(HANDLE / 2.0)
  .width(HANDLE)
  .height(HANDLE);

const CHIP: Style = Style::new().background(SURFACE).radius(14.0).padding(6, 12);
const CHIP_ON: Style = CHIP.background(ACCENT).color(DARK);
const CHIP_HOVER: Style = Style::new().background(RAISED);
const CHIP_OFF: Style = CHIP.opacity(0.4);

/// A calendar cell: a circle in the middle of its seventh of the width.
const DAY: Style = Style::new()
  .width(26.0)
  .height(26.0)
  .radius(13.0)
  .justify_self(Align::Center);
const TODAY: Style = DAY.background(ACCENT).color(DARK);
const WEEKDAY: Style = DAY.color(DIM);

#[allow(dead_code, reason = "the tests build the windows, not main")]
fn main() {
  crabshell::run(setup);
}

/// The control center on every output.
pub fn setup(cx: &Cx) {
  let sink = cx.audio().default_sink();
  let today = today(cx);
  cx.per_output(move |cx, output| panel(cx, output, sink, today));
}

/// Today's date, or `CONTROL_TODAY`'s. It moves on at midnight, on a
/// timer that wakes the shell once a day, never every minute as a clock
/// would.
pub fn today(cx: &Cx) -> ReadSignal<Date> {
  if let Some(date) = std::env::var("CONTROL_TODAY")
    .ok()
    .and_then(|date| date.parse().ok())
  {
    return cx.signal(date).read_only();
  }
  let today = cx.signal(Zoned::now().date());
  let parent = cx.clone();
  // The day's timer lives in a context of its own, freed at the next.
  let mut day: Option<Cx> = None;
  cx.effect(move || {
    today.get();
    if let Some(old) = day.take() {
      old.owner().dispose();
    }
    let now = Zoned::now();
    let Some(wait) = now
      .tomorrow()
      .and_then(|tomorrow| tomorrow.start_of_day())
      .ok()
      .and_then(|midnight| Duration::try_from(now.duration_until(&midnight)).ok())
    else {
      return;
    };
    let child = parent.child();
    child.after(wait, move || today.set(Zoned::now().date()));
    day = Some(child);
  });
  today.read_only()
}

/// The control center on `output`, for `sink` and `today`.
pub fn panel(
  cx: &Cx,
  output: &Output,
  sink: ReadSignal<Option<Sink>>,
  today: ReadSignal<Date>,
) -> PanelWindow {
  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Right)
    .margins(Margins::new(8, 8, 0, 0))
    .width(WIDTH)
    .height(HEIGHT)
    // Over the windows, without pushing them aside.
    .exclusive_zone(0)
    .namespace("control")
    .child(
      Column::new()
        .style(PANEL)
        .padding(PADDING, PADDING)
        .gap(18)
        .child(volume(cx, sink))
        .child(chips(cx))
        .child(calendar(today)),
    )
}

/// The default sink's volume, as a label and a slider that sets it.
fn volume(cx: &Cx, sink: ReadSignal<Option<Sink>>) -> Column {
  let value = cx.signal(0.0_f32);
  let dragging = cx.signal(false);
  // The sink moves the handle, except while it is held.
  cx.effect(move || {
    let percent = sink.with(|sink| sink.as_ref().map(|sink| sink.volume_percent));
    if let (Some(percent), false) = (percent, dragging.get()) {
      value.set(percent as f32);
    }
  });
  // And a value the sink doesn't have yet, from the pointer, the wheel or
  // the keys, goes to the sink.
  let parent = cx.clone();
  cx.effect(move || {
    let percent = value.get().round() as u32;
    let name = sink.with_untracked(|sink| {
      sink
        .as_ref()
        .filter(|sink| sink.volume_percent != percent)
        .map(|sink| sink.name.clone())
    });
    if let Some(name) = name {
      parent.audio().set_sink_volume(&name, percent);
    }
  });
  Column::new()
    .gap(10)
    .child(
      Row::new()
        .child(Text::new("Volume").style(HEADING))
        .child(Spacer::new())
        .child(Text::new(move || match sink.get() {
          Some(sink) if sink.muted => "muted".to_owned(),
          Some(_) => format!("{}%", value.get().round()),
          None => "no audio".to_owned(),
        })),
    )
    .child(
      Slider::new(value)
        .range(0.0, 100.0)
        .step(1.0)
        .enabled(move || sink.with(Option::is_some))
        .dragging(dragging)
        .style(TRACK)
        .fill_style(FILL)
        .handle_style(HANDLE_STYLE),
    )
}

/// Quick settings, each a toggle, wrapping onto as many lines as they
/// take. Airplane mode is off limits here, so it is turned off.
fn chips(cx: &Cx) -> Row {
  let names = [
    ("Wi-Fi", true),
    ("Bluetooth", true),
    ("Do not disturb", false),
    ("Night light", false),
    ("Airplane mode", false),
    ("Caffeine", false),
  ];
  names
    .into_iter()
    .fold(Row::new().wrap(true).gap(8), |row, (name, on)| {
      let on = cx.signal(on);
      let enabled = name != "Airplane mode";
      row.child(
        Button::new(Text::new(name))
          .style(move || match (enabled, on.get()) {
            (false, _) => CHIP_OFF,
            (true, true) => CHIP_ON,
            (true, false) => CHIP,
          })
          .hover_style(move || if on.get() { Style::new() } else { CHIP_HOVER })
          .enabled(enabled)
          .on_click(move |_| on.update(|on| *on = !*on)),
      )
    })
}

/// `today`'s month, Monday first, with today marked.
fn calendar(today: ReadSignal<Date>) -> Column {
  let cell = |text: String, style: Style| {
    Row::new()
      .style(style)
      .justify(Justify::Center)
      .align(Align::Center)
      .child(Text::new(text))
  };
  // The grid is built again only when the day changes.
  let days = Column::new().children_keyed(
    move || vec![today.get()],
    |date| *date,
    move |_cx, date| {
      let date = date.get_untracked();
      let first = date.first_of_month();
      let blanks = first.weekday().to_monday_zero_offset() as u32;
      let grid = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"]
        .into_iter()
        .fold(Grid::new().columns(7).gap(4), |grid, name| {
          grid.child(cell(name.to_owned(), WEEKDAY))
        });
      let grid = if blanks > 0 {
        grid.child(Row::new().style(Style::new().span(blanks, 1)))
      } else {
        grid
      };
      (1..=date.days_in_month()).fold(grid, |grid, day| {
        let style = if day == date.day() { TODAY } else { DAY };
        grid.child(cell(day.to_string(), style))
      })
    },
  );
  Column::new()
    .gap(10)
    .child(Text::new(move || today.get().strftime("%B %Y").to_string()).style(HEADING))
    .child(days)
}
