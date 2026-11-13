//! An example bar: one panel per output, with a button that counts its
//! clicks and the output's name at the right. No clock, no services. The
//! counts survive a reload under `crabshell run`.
//!
//! Run it inside Sway with `cargo run -p crabshell --example counter`.

use crabshell::prelude::*;

const BAR: Style = Style::new()
  .background(Color::rgb(0x1e, 0x1e, 0x2e))
  .color(Color::rgb(0xcd, 0xd6, 0xf4));
const BUTTON: Style = Style::new()
  .background(Color::rgb(0x45, 0x47, 0x5a))
  .padding(2, 10)
  .radius(4.0);
const HOVERED: Style = Style::new().background(Color::rgb(0x58, 0x5b, 0x70));

fn main() {
  crabshell::run(|cx| cx.per_output(bar));
}

/// The bar for one output. Each output's bar counts its own clicks.
pub fn bar(cx: &Cx, output: &Output) -> PanelWindow {
  let count = cx.persist(&format!("clicks on {}", output.name()), 0_u32);
  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .style(BAR)
    .child(
      Row::new()
        .padding(4, 8)
        .gap(8)
        .align(Align::Center)
        .child(
          Button::new(Text::new(move || match count.get() {
            1 => String::from("clicked once"),
            count => format!("clicked {count} times"),
          }))
          .style(BUTTON)
          .hover_style(HOVERED)
          .on_click(move |_| count.update(|count| *count += 1)),
        )
        .child(Spacer::new())
        .child(Text::new(output.name())),
    )
}
