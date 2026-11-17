//! A notification center at the top right of each screen: the time with
//! its seconds, in digits that keep their width, the notifications kept,
//! newest first, each body in the spec's markup with links that open in
//! the browser, and a quick note that a reload keeps. A click on a
//! notification dismisses it.
//!
//! ```sh
//! cargo run --release -p crabshell --example notes
//! notify-send 'Build finished' '<b>crabshell</b> passed, see <a href="https://example.org">the log</a>'
//! ```
//!
//! `NOTES_NOW=2026-11-17T11:11:11` stops the clock at that time, as the
//! tests do. The section labels are in DejaVu Sans, found by fontconfig
//! at run time: no font small enough to bundle with `include_bytes!` was
//! at hand.

use crabshell::jiff::{Zoned, civil::DateTime, tz::TimeZone};
use crabshell::prelude::*;

const TEXT: Color = Color::rgb(0xcd, 0xd6, 0xf4);
const DIM: Color = Color::rgb(0x9a, 0xa0, 0xb8);
const ACCENT: Color = Color::rgb(0x89, 0xb4, 0xfa);
const DARK: Color = Color::rgb(0x1e, 0x1e, 0x2e);
const SURFACE: Color = Color::rgb(0x31, 0x32, 0x44);
const RAISED: Color = Color::rgb(0x45, 0x47, 0x5a);

/// The panel's size; it sits 8 pixels in from the top right.
pub const WIDTH: u32 = 360;
const HEIGHT: u32 = 640;
const PADDING: u32 = 16;
/// The space inside the panel's padding.
const INNER: f32 = (WIDTH - 2 * PADDING) as f32;
const PANEL: Style = Style::new()
  .background(DARK)
  .color(TEXT)
  .font(Font::new("sans-serif", 13.0))
  .radius(14.0)
  .border(1.0, RAISED);

/// The time, in digits of one width, so the seconds ticking never move
/// it or what is around it.
const CLOCK: Style = Style::new().font(
  Font::new("sans-serif", 36.0)
    .weight(300)
    .features(&[FontFeature::TABULAR_NUMBERS]),
);
const DATE: Style = Style::new().color(DIM);

/// A section's label: small spaced capitals in a heading font.
const LABEL: Style = Style::new().color(DIM).font(
  Font::new("DejaVu Sans", 10.0)
    .weight(700)
    .letter_spacing(1.5),
);
const CLEAR: Style = Style::new().color(ACCENT).padding(2, 6).radius(6.0);
const CLEAR_HOVER: Style = Style::new().background(SURFACE);

/// The notifications, in a list that scrolls past this height.
const LIST_HEIGHT: f32 = 312.0;
const CARD: Style = Style::new()
  .background(SURFACE)
  .radius(10.0)
  .padding(10, 12);
const CARD_HOVER: Style = Style::new().background(RAISED);
const SCROLL_BAR: Style = Style::new().background(RAISED).radius(2.0).width(4.0);
/// A card's text, as wide as the card's inside, so it starts at the left
/// and wraps there.
const CARD_TEXT: Style = Style::new().width(INNER - 24.0);
const APP: Style = Style::new().color(DIM).font(Font::new("sans-serif", 11.0));
const SUMMARY: Style = Style::new().font(Font::new("sans-serif", 13.0).weight(700));
/// A body over a few lines, spaced to read easily.
const BODY: Style = Style::new().color(DIM).line_height(1.3);
const BODY_LINES: u32 = 3;
/// Nothing to show, said in the middle of the list's width.
const EMPTY: Style = Style::new()
  .color(DIM)
  .width(INNER)
  .padding(24, 0)
  .text_align(TextAlign::Center);

/// The quick note: an area that scrolls past this height.
const NOTE_HEIGHT: f32 = 120.0;
const NOTE_BOX: Style = Style::new()
  .background(SURFACE)
  .radius(10.0)
  .width(INNER)
  .height(NOTE_HEIGHT);
const NOTE: Style = Style::new().width(INNER).padding(8, 10).line_height(1.3);
/// The key the note is persisted under.
pub const NOTE_KEY: &str = "quick note";

#[allow(dead_code, reason = "the tests build the windows, not main")]
fn main() {
  crabshell::run(setup);
}

/// What the panel shows, and what a link does.
#[derive(Clone, Copy)]
pub struct Feeds {
  /// The time.
  pub now: ReadSignal<Zoned>,
  /// The notifications kept, oldest first.
  pub notifications: ReadSignal<Vec<Notification>>,
  /// What a click on a link in a body runs with its URL.
  pub open: fn(&Cx, &str),
}

/// The notification center on every output, as a user runs it.
fn setup(cx: &Cx) {
  shell(cx, xdg_open);
}

/// The notification center on every output, its links opened by `open`.
pub fn shell(cx: &Cx, open: fn(&Cx, &str)) {
  let notifications = cx.notifications();
  // Apps then send their bodies' markup instead of plain text.
  notifications.set_body_markup(true);
  let feeds = Feeds {
    now: now(cx),
    notifications: notifications.list(),
    open,
  };
  cx.per_output(move |cx, output| panel(cx, output, feeds));
}

/// Opens `url` in the user's browser or mail program.
fn xdg_open(cx: &Cx, url: &str) {
  cx.exec_detached(["xdg-open", url]);
}

/// The time, each second, or `NOTES_NOW`'s, which never moves.
fn now(cx: &Cx) -> ReadSignal<Zoned> {
  let pinned = std::env::var("NOTES_NOW").ok().and_then(|now| {
    let now: DateTime = now.parse().ok()?;
    now.to_zoned(TimeZone::UTC).ok()
  });
  match pinned {
    Some(now) => cx.signal(now).read_only(),
    None => cx.clock(Precision::Second),
  }
}

/// The notification center on `output`.
pub fn panel(cx: &Cx, output: &Output, feeds: Feeds) -> PanelWindow {
  let note = cx.persist(NOTE_KEY, String::new());
  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Right)
    .margins(Margins::new(8, 8, 0, 0))
    .width(WIDTH)
    .height(HEIGHT)
    // Over the windows, without pushing them aside, and taking the keys
    // once the note is clicked.
    .exclusive_zone(0)
    .keyboard_focus(KeyboardFocus::OnDemand)
    .namespace("notes")
    .child(
      Column::new()
        .style(PANEL)
        .padding(PADDING, PADDING)
        .gap(14)
        .child(
          Column::new().gap(2).child(clock(feeds.now)).child(
            Text::new(move || feeds.now.get().strftime("%A %-d %B").to_string()).style(DATE),
          ),
        )
        .child(notifications(feeds))
        .child(Text::new("QUICK NOTE").style(LABEL))
        .child(
          ScrollView::new(
            TextArea::new(note)
              .placeholder("Jot something down")
              .style(NOTE),
          )
          .style(NOTE_BOX),
        ),
    )
}

/// The time with its seconds.
pub fn clock(now: ReadSignal<Zoned>) -> Text {
  Text::new(move || now.get().strftime("%H:%M:%S").to_string()).style(CLOCK)
}

/// The notifications' label with a button that clears them, and the list,
/// newest first, or a line saying there are none.
fn notifications(feeds: Feeds) -> Column {
  let list = feeds.notifications;
  let any = move || list.with(|list| !list.is_empty());
  let cards = Column::new().gap(8).children_keyed(
    move || list.with(|list| list.iter().rev().cloned().collect()),
    |notification: &Notification| notification.id,
    move |_cx, notification| card(notification, feeds.open),
  );
  Column::new()
    .gap(8)
    .child(
      Row::new()
        .align(Align::Center)
        .child(Text::new("NOTIFICATIONS").style(LABEL))
        .child(Spacer::new())
        .child(
          Button::new(Text::new("Clear all"))
            .style(CLEAR)
            .hover_style(CLEAR_HOVER)
            .visible(any)
            .on_click(|cx| cx.notifications().clear()),
        ),
    )
    .child(
      ScrollView::new(cards)
        .style(Style::new().height(LIST_HEIGHT))
        .scroll_bar(SCROLL_BAR)
        .visible(any),
    )
    .child(
      Text::new("You're all caught up. New notifications show here.")
        .style(EMPTY)
        .max_lines(2)
        .visible(move || !any()),
    )
}

/// One notification: its app, its summary in bold, and its body's markup
/// over at most a few lines, its links opened by `open`.
fn card(notification: Memo<Notification>, open: fn(&Cx, &str)) -> Button {
  let id = notification.get_untracked().id;
  let has_app = move || notification.with(|n| !n.app_name.is_empty());
  let has_body = move || notification.with(|n| !n.body.is_empty());
  Button::new(
    Column::new()
      .gap(3)
      .style(CARD_TEXT)
      .child(
        Text::new(move || notification.get().app_name)
          .style(APP)
          .visible(has_app),
      )
      .child(
        Text::new(move || notification.get().summary)
          .style(SUMMARY)
          .ellipsis(true),
      )
      .child(
        Text::rich(move || notification.with(|n| parse_markup(&n.body)))
          .style(BODY)
          .max_lines(BODY_LINES)
          .elide(Elide::End)
          .on_link(open)
          .visible(has_body),
      ),
  )
  .style(CARD)
  .hover_style(CARD_HOVER)
  .on_click(move |cx| cx.notifications().dismiss(id))
}
