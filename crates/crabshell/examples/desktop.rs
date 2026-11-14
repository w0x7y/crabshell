//! A bar with Phase 9's desktop services: what plays, the volume, the
//! network, Bluetooth, the battery and the power profile, a button that
//! keeps the screen awake, and an "away" mark once nothing was touched
//! for five minutes. The shell is the notification server, and new
//! notifications show as toasts in the top right corner. Click one to
//! dismiss it, or its button for its action.
//!
//! ```sh
//! cargo run --release -p crabshell --example desktop
//! ```

use std::time::Duration;

use crabshell::prelude::*;

const BAR: Style = Style::new()
  .background(Color::rgb(0x1e, 0x1e, 0x2e))
  .color(Color::rgb(0xcd, 0xd6, 0xf4))
  .font(Font::new("sans-serif", 13.0));
const CHIP: Style = Style::new().padding(2, 8).radius(4.0);
const ON: Style = CHIP.background(Color::rgb(0x45, 0x47, 0x5a));
const TOAST: Style = Style::new()
  .background(Color::rgb(0x31, 0x32, 0x44))
  .color(Color::rgb(0xcd, 0xd6, 0xf4))
  .radius(8.0)
  .padding(8, 12)
  .width(340.0);
const SUMMARY: Style = Style::new().font(Font::new("sans-serif", 13.0).weight(700));

#[allow(dead_code, reason = "the tests build the windows, not main")]
fn main() {
  crabshell::run(|cx| {
    cx.per_output(|cx, output| vec![bar(cx, output), toasts(cx, output)]);
  });
}

/// The bar on `output`.
pub fn bar(cx: &Cx, output: &Output) -> PanelWindow {
  let awake = cx.signal(false);
  let away = cx.idle_monitor(Duration::from_secs(300));
  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .inhibit_idle(move || awake.get())
    .style(BAR)
    .child(
      Row::new()
        .gap(8)
        .padding(4, 8)
        .align(Align::Center)
        .child(playing(cx))
        .child(Spacer::new())
        .child(Text::new(move || {
          if away.get() { "away" } else { "" }.to_owned()
        }))
        .child(
          Button::new(Text::new("stay awake"))
            .style(move || if awake.get() { ON } else { CHIP })
            .on_click(move |_| awake.update(|on| *on = !*on)),
        )
        .child(Text::new(volume(cx)))
        .child(Text::new(network(cx)))
        .child(Text::new(bluetooth(cx)))
        .child(Text::new(battery(cx))),
    )
}

/// The player that plays, else the first, with a play/pause button.
fn playing(cx: &Cx) -> Row {
  let players = cx.mpris().players();
  let shown = cx.memo(move || {
    let players = players.get();
    let playing = players.iter().find(|p| p.status == PlaybackStatus::Playing);
    playing.or(players.first()).cloned()
  });
  Row::new().gap(6).align(Align::Center).children_keyed(
    move || shown.get().into_iter().collect(),
    |player: &Player| player.id.clone(),
    move |_, player| {
      Button::new(Text::new(move || {
        let player = player.get();
        let mark = if player.status == PlaybackStatus::Playing {
          "⏸"
        } else {
          "▶"
        };
        format!("{mark} {} · {}", player.title, player.artists.join(", "))
      }))
      .style(CHIP)
      .on_click(move |cx| cx.mpris().play_pause(&player.get().id))
    },
  )
}

fn volume(cx: &Cx) -> impl Fn() -> String + 'static {
  let sink = cx.audio().default_sink();
  move || match sink.get() {
    Some(sink) if sink.muted => "muted".to_owned(),
    Some(sink) => format!("vol {}%", sink.volume_percent),
    None => String::new(),
  }
}

fn network(cx: &Cx) -> impl Fn() -> String + 'static {
  let network = cx.network();
  let (devices, wifi) = (network.devices(), network.wifi_networks());
  move || {
    if let Some(wifi) = wifi.get().into_iter().find(|wifi| wifi.connected) {
      return format!("{} {}%", wifi.ssid, wifi.strength);
    }
    let wired = devices.get().into_iter().any(|device| {
      device.kind == NetworkKind::Ethernet && device.state == NetworkState::Connected
    });
    if wired { "wired" } else { "offline" }.to_owned()
  }
}

fn bluetooth(cx: &Cx) -> impl Fn() -> String + 'static {
  let bluetooth = cx.bluetooth();
  let (adapters, devices) = (bluetooth.adapters(), bluetooth.devices());
  move || {
    if !adapters.get().iter().any(|adapter| adapter.powered) {
      return "bt off".to_owned();
    }
    match devices
      .get()
      .iter()
      .filter(|device| device.connected)
      .count()
    {
      0 => "bt on".to_owned(),
      1 => devices
        .get()
        .into_iter()
        .find(|d| d.connected)
        .map_or_else(String::new, |d| d.name),
      n => format!("bt {n}"),
    }
  }
}

fn battery(cx: &Cx) -> impl Fn() -> String + 'static {
  let display = cx.upower().display_device();
  let profile = cx.power_profiles().active();
  move || {
    let saver = profile.get() == Some(PowerProfile::PowerSaver);
    let leaf = if saver { " (saver)" } else { "" };
    match display.get().filter(|battery| battery.present) {
      Some(battery) => {
        let charging = battery.state == BatteryState::Charging;
        let plug = if charging { "+" } else { "" };
        format!("bat {:.0}%{plug}{leaf}", battery.percent)
      }
      None => leaf.trim().to_owned(),
    }
  }
}

/// The notifications still showing, top right, newest first. It takes
/// clicks only where a toast is.
pub fn toasts(cx: &Cx, output: &Output) -> PanelWindow {
  let popups = cx.notifications().popups();
  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Right)
    .layer(Layer::Overlay)
    .margins(Margins::new(36, 8, 0, 0))
    .width(340)
    .height(400)
    .exclusive_zone(-1)
    .namespace("toasts")
    .mask(move || {
      // Room for each toast, at most 64 pixels tall with its gap.
      let tall = (popups.with(Vec::len) as u32 * 64).min(400);
      Some(Region::rect(0, 0, 340, tall))
    })
    .child(Column::new().gap(8).children_keyed(
      move || popups.get(),
      |notification: &Notification| notification.id,
      |_, notification| toast(notification),
    ))
}

/// One notification: its summary, its body and its actions.
fn toast(notification: Memo<Notification>) -> Button {
  let id = move || notification.get().id;
  Button::new(
    Column::new()
      .gap(4)
      .child(Text::new(move || notification.get().summary).style(SUMMARY))
      .child(Text::new(move || notification.get().body))
      .child(Row::new().gap(6).children_keyed(
        move || {
          let actions = notification.get().actions;
          actions
            .into_iter()
            .filter(|(key, _)| key != "default")
            .collect()
        },
        |(key, _): &(String, String)| key.clone(),
        move |_, action| {
          Button::new(Text::new(move || action.get().1))
            .style(ON)
            .on_click(move |cx| cx.notifications().invoke(id(), &action.get().0))
        },
      )),
  )
  .style(TOAST)
  .on_click(move |cx| cx.notifications().dismiss(id()))
}
