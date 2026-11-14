//! A bar that shows off the effects: a translucent gradient the
//! compositor is asked to blur behind, tabs whose hover color fades in, an
//! indicator that slides under the picked tab, and a card with a soft
//! shadow. Nothing moves, and nothing wakes, between changes.
//!
//! ```sh
//! cargo run --release -p crabshell --example effects
//! ```

use crabshell::prelude::*;

/// The bar's height in logical pixels.
const HEIGHT: u32 = 48;
/// A tab's width, and the gap after it.
const TAB_WIDTH: f32 = 40.0;
const TAB_GAP: u32 = 4;

const BAR: Style = Style::new()
  .gradient(Gradient::linear(
    90.0,
    Color::rgba(0x1e, 0x1e, 0x2e, 0xd8),
    Color::rgba(0x31, 0x32, 0x44, 0xd8),
  ))
  .color(Color::rgb(0xcd, 0xd6, 0xf4));
const TAB: Style = Style::new().radius(6.0).transition(Transition::new(150));
const TAB_HOVER: Style = Style::new().background(Color::rgba(0xff, 0xff, 0xff, 0x24));
const INDICATOR: Style = Style::new()
  .background(Color::rgb(0x89, 0xb4, 0xfa))
  .radius(1.5)
  .width(TAB_WIDTH)
  .height(3.0);
const CARD: Style = Style::new()
  .background(Color::rgb(0x45, 0x47, 0x5a))
  .radius(8.0)
  .shadow(Shadow::new(0.0, 2.0, 8.0, Color::rgba(0, 0, 0, 0xa0)))
  .padding(4, 12);

#[allow(dead_code, reason = "the tests build the bar, not main")]
fn main() {
  crabshell::run(|cx| cx.per_output(effects));
}

/// The bar on `output`.
pub fn effects(cx: &Cx, output: &Output) -> PanelWindow {
  let picked = cx.signal(0_usize);
  // Slides to the picked tab instead of jumping.
  let offset = cx.animate(
    move || picked.get() as f32 * (TAB_WIDTH + TAB_GAP as f32),
    Transition::new(250),
  );
  let tabs = (0..4).fold(Row::new().gap(TAB_GAP), |row, n| {
    row.child(
      Button::new(
        Row::new()
          .align(Align::Center)
          .justify(Justify::Center)
          .style(Style::new().width(TAB_WIDTH).height(24.0))
          .child(Text::new((n + 1).to_string())),
      )
      .style(TAB)
      .hover_style(TAB_HOVER)
      .on_click(move |_| picked.set(n)),
    )
  });
  let indicator = Row::new()
    .child(Row::new().style(move || Style::new().width(offset.get())))
    .child(Row::new().style(INDICATOR));
  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(HEIGHT)
    .blur(Some(Region::rect(0, 0, u32::MAX, HEIGHT)))
    .style(BAR)
    .child(
      Row::new()
        .padding(8, 12)
        .align(Align::Center)
        .child(Column::new().gap(4).child(tabs).child(indicator))
        .child(Spacer::new())
        .child(Row::new().style(CARD).child(Text::new("effects"))),
    )
}
