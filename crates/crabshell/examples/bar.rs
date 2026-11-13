//! The v1 bar from the design doc's "Writing a config": Sway's workspaces
//! for each output at the left, the tray, the volume and the clock at the
//! right. Scroll over the volume to change it by 5% a notch, and click it
//! to mute. Click a tray icon to activate its app, or right-click it for
//! its menu.
//!
//! Run it inside Sway with `cargo run -p crabshell --example bar`.

use crabshell::prelude::*;

const BAR: Style = Style::new()
  .background(Color::rgb(0x1e, 0x1e, 0x2e))
  .color(Color::rgb(0xcd, 0xd6, 0xf4))
  .font(Font::new("Inter", 13.0));
const WS: Style = Style::new().padding(2, 8).radius(4.0);
const WS_FOCUSED: Style = WS.background(Color::rgb(0x45, 0x47, 0x5a));

fn main() {
  crabshell::run(|cx| cx.per_output(bar));
}

/// The bar for one output.
pub fn bar(cx: &Cx, output: &Output) -> PanelWindow {
  let clock = cx.clock(Precision::Minute);
  let workspaces = cx.sway().workspaces();
  let sink = cx.audio().default_sink();
  let output_name = output.name();

  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .exclusive_zone(30)
    .style(BAR)
    .child(
      Row::new()
        .gap(8)
        .padding(4, 8)
        .child(Row::new().gap(4).children_keyed(
          move || {
            workspaces
              .get()
              .into_iter()
              .filter(|w| w.output == output_name)
              .collect::<Vec<_>>()
          },
          |w| w.id,
          |_cx, ws| {
            Button::new(Text::new(move || ws.get().name))
              .style(move || if ws.get().focused { WS_FOCUSED } else { WS })
              .on_click(move |cx| cx.sway().focus_workspace(&ws.get().name))
          },
        ))
        .child(Spacer::new())
        .child(Tray::new().icon_size(18))
        .child(
          Button::new(Text::new(move || match sink.get() {
            Some(s) if s.muted => "muted".to_string(),
            Some(s) => format!("vol {}%", s.volume_percent),
            None => "no audio".to_string(),
          }))
          .on_click(|cx| cx.audio().toggle_mute())
          .on_scroll(|cx, steps| cx.audio().change_volume(steps * 5)),
        )
        .child(Text::new(move || {
          clock.get().strftime("%a %d %b  %H:%M").to_string()
        })),
    )
}
