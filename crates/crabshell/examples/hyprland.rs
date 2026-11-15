//! A bar for Hyprland: each monitor's workspaces at the left, the focused
//! window's title at the right. Click a workspace to switch to it.
//!
//! Run it inside Hyprland with `cargo run -p crabshell --example hyprland`.

use crabshell::prelude::*;

const BAR: Style = Style::new()
  .background(Color::rgb(0x1e, 0x1e, 0x2e))
  .color(Color::rgb(0xcd, 0xd6, 0xf4));
const WS: Style = Style::new().padding(2, 8).radius(4.0);
const WS_FOCUSED: Style = WS.background(Color::rgb(0x45, 0x47, 0x5a));
const WS_URGENT: Style = WS.background(Color::rgb(0xf3, 0x8b, 0xa8));

#[allow(dead_code, reason = "the tests run the example, not this main")]
fn main() {
  crabshell::run(|cx| cx.per_output(bar));
}

/// The bar for one output.
pub fn bar(cx: &Cx, output: &Output) -> PanelWindow {
  let hyprland = cx.hyprland();
  let workspaces = hyprland.workspaces();
  let toplevels = hyprland.toplevels();
  let monitor = output.name();

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
            // A special workspace is toggled, not switched to.
            let on =
              |w: &HyprlandWorkspace| w.monitor == monitor && !w.name.starts_with("special:");
            workspaces.with(|all| all.iter().filter(|w| on(w)).cloned().collect::<Vec<_>>())
          },
          |w| w.name.clone(),
          |cx, ws| {
            let hyprland = cx.hyprland();
            Button::new(Text::new(move || ws.get().name))
              .style(move || match ws.get() {
                w if w.focused => WS_FOCUSED,
                w if w.urgent => WS_URGENT,
                _ => WS,
              })
              .on_click(move |_| {
                let ws = ws.get();
                // A named workspace has no number, or before Hyprland 0.57
                // a negative one, which would read as a relative move.
                let target = match ws.id {
                  Some(id) if id > 0 => id.to_string(),
                  _ => format!("name:{}", ws.name),
                };
                hyprland.focus_workspace(&target);
              })
          },
        ))
        .child(Spacer::new())
        .child(
          Text::new(move || {
            let focused = toplevels.with(|all| all.iter().find(|w| w.focused).cloned());
            focused.map(|w| w.title).unwrap_or_default()
          })
          .ellipsis(true),
        ),
    )
}
