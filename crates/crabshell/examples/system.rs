//! A bar with what a config reads from the system and the network: the
//! workspaces, from niri when `NIRI_SOCKET` is set and from Sway
//! otherwise, the weather, the CPU, memory and temperature, a brightness
//! slider when the screen has a backlight, and the date in the user's
//! language. Two buttons at the right open the clipboard's history, where
//! a click copies an entry again, and a wallpaper picker, whose pictures
//! drag out to other programs as files.
//!
//! ```sh
//! cargo run --release -p crabshell --example system
//! ```
//!
//! `WEATHER_URL` names an Open-Meteo forecast URL, Berlin's by default,
//! fetched at the start and every half hour. `WALLPAPERS` names the
//! picker's folder, `~/Pictures/Wallpapers` by default.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crabshell::jiff::Zoned;
use crabshell::prelude::*;
use crabshell::serde_json::{self, Value};

const TEXT: Color = Color::rgb(0xcd, 0xd6, 0xf4);
const DIM: Color = Color::rgb(0x9a, 0xa0, 0xb8);
const ACCENT: Color = Color::rgb(0x89, 0xb4, 0xfa);
const DARK: Color = Color::rgb(0x1e, 0x1e, 0x2e);
const SURFACE: Color = Color::rgb(0x31, 0x32, 0x44);
const RAISED: Color = Color::rgb(0x45, 0x47, 0x5a);

pub const HEIGHT: u32 = 30;
const BAR: Style = Style::new()
  .background(DARK)
  .color(TEXT)
  .font(Font::new("sans-serif", 13.0));
const BODY: Style = Style::new().color(DIM);
const HEADING: Style = Style::new().font(Font::new("sans-serif", 13.0).weight(700));
const WS: Style = Style::new().padding(2, 8).radius(4.0);
const WS_ACTIVE: Style = WS.background(RAISED);

/// The buttons that open the popups: a 16 pixel icon in 6 of padding.
const ICON_BUTTON: Style = Style::new().padding(6, 6).radius(6.0);
const HOVER: Style = Style::new().background(SURFACE);

const TRACK: Style = Style::new()
  .background(RAISED)
  .radius(2.0)
  .width(96.0)
  .height(4.0);
const FILL: Style = Style::new().background(ACCENT).radius(2.0);
const HANDLE: Style = Style::new()
  .background(TEXT)
  .radius(6.0)
  .width(12.0)
  .height(12.0);

const POPUP: Style = Style::new()
  .background(DARK)
  .color(TEXT)
  .font(Font::new("sans-serif", 13.0))
  .radius(12.0)
  .border(1.0, RAISED)
  .padding(12, 12);
const CLIP_WIDTH: f32 = 280.0;
const CLIP: Style = Style::new().padding(6, 8).radius(6.0).width(CLIP_WIDTH);
const CLIP_TEXT: Style = Style::new().width(CLIP_WIDTH - 16.0);
/// The most clipboard entries shown; the history keeps more.
const CLIPS_SHOWN: usize = 8;
const THUMB_WIDTH: u32 = 112;
const THUMB_HEIGHT: u32 = 63;
/// A picker cell: a thumbnail, its 4 of padding and its name, with
/// `CELL_GAP` between cells.
const CELL_GAP: u32 = 8;
const CELL_WIDTH: u32 = THUMB_WIDTH + 8 + CELL_GAP;
const CELL_HEIGHT: u32 = THUMB_HEIGHT + 8 + 4 + 16 + CELL_GAP;
const SCROLL_BAR: Style = Style::new().background(RAISED).radius(2.0).width(4.0);
const THUMB: Style = Style::new().padding(4, 4).radius(8.0);
const PICTURE: Style = Style::new().radius(6.0).clip(true);
const NAME: Style = Style::new().color(DIM).width(THUMB_WIDTH as f32);

/// Berlin's temperature and weather code now, from Open-Meteo.
const OPEN_METEO: &str = "https://api.open-meteo.com/v1/forecast?latitude=52.52&longitude=13.41&current=temperature_2m,weather_code";

#[allow(dead_code, reason = "the tests build the bar, not main")]
fn main() {
  crabshell::run(setup);
}

/// What changes on its own, so the tests can pin it.
#[derive(Clone, Copy)]
pub struct Feeds {
  /// The time, to the minute.
  pub now: ReadSignal<Zoned>,
  /// CPU, memory and temperatures.
  pub stats: ReadSignal<SystemStats>,
  /// The screen's backlights.
  pub backlights: ReadSignal<Vec<Backlight>>,
}

/// The bar on every output, as a user runs it.
fn setup(cx: &Cx) {
  let feeds = Feeds {
    now: cx.clock(Precision::Minute),
    stats: cx.system_stats(Duration::from_secs(2)),
    backlights: cx.brightness().devices(),
  };
  shell(cx, feeds);
}

/// The bar from `feeds` on every output, with the weather, and the
/// clipboard's history kept from now on.
pub fn shell(cx: &Cx, feeds: Feeds) {
  let weather = weather(cx);
  let clipboard = cx.clipboard();
  clipboard.history();
  cx.per_output(move |cx, output| bar(cx, output, feeds, weather, clipboard));
}

/// The temperature and weather code now, once a fetch has worked.
fn weather(cx: &Cx) -> Memo<Option<(f64, u64)>> {
  let url = std::env::var("WEATHER_URL").unwrap_or_else(|_| OPEN_METEO.to_owned());
  let http = cx.http_get(url);
  cx.interval(Duration::from_secs(30 * 60), move || http.refresh());
  let body = http.body();
  cx.memo(move || body.with(|body| parse_weather(body.as_deref()?)))
}

/// The temperature and WMO weather code in an Open-Meteo reply, or
/// `None` for one without them.
pub fn parse_weather(body: &str) -> Option<(f64, u64)> {
  let reply: Value = serde_json::from_str(body).ok()?;
  let current = reply.get("current")?;
  let celsius = current.get("temperature_2m")?.as_f64()?;
  Some((celsius, current.get("weather_code")?.as_u64()?))
}

/// The symbolic icon for a WMO weather code.
pub fn weather_icon(code: u64) -> &'static str {
  match code {
    0 => "weather-clear-symbolic",
    1 | 2 => "weather-few-clouds-symbolic",
    3 => "weather-overcast-symbolic",
    45 | 48 => "weather-fog-symbolic",
    51..=67 | 80..=82 => "weather-showers-symbolic",
    71..=77 | 85 | 86 => "weather-snow-symbolic",
    95..=99 => "weather-storm-symbolic",
    _ => "weather-severe-alert-symbolic",
  }
}

/// The bar on `output`.
fn bar(
  cx: &Cx,
  output: &Output,
  feeds: Feeds,
  weather: Memo<Option<(f64, u64)>>,
  clipboard: Clipboard,
) -> PanelWindow {
  let locale = cx.locale();
  let now = feeds.now;
  let stats = feeds.stats;
  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(HEIGHT)
    .exclusive_zone(HEIGHT as i32)
    .style(BAR)
    .child(
      Row::new()
        .gap(12)
        .padding(1, 8)
        .align(Align::Center)
        .child(workspaces(cx, output.name()))
        .child(Spacer::new())
        .child(
          Row::new()
            .gap(4)
            .align(Align::Center)
            .child(
              // A theme without the icon for this weather shows overcast.
              Icon::new(move || {
                let code = weather.get().map_or(0, |(_, code)| code);
                IconSource::Name(weather_icon(code).to_owned())
              })
              .fallback("weather-overcast-symbolic")
              .size(16)
              .tint(TEXT)
              .visible(move || weather.with(Option::is_some)),
            )
            .child(Text::new(move || match weather.get() {
              Some((celsius, _)) => format!("{celsius:.0}°C"),
              None => "–".to_owned(),
            })),
        )
        .child(Text::new(move || stats.with(|s| share("cpu", s, s.cpu))).style(BODY))
        .child(Text::new(move || stats.with(|s| share("mem", s, memory_used(s)))).style(BODY))
        .child(
          Text::new(move || {
            stats.with(|s| {
              cpu_temperature(s)
                .map(|c| format!("{c:.0}°C"))
                .unwrap_or_default()
            })
          })
          .style(BODY)
          .visible(move || stats.with(|s| cpu_temperature(s).is_some())),
        )
        .child(brightness(cx, feeds.backlights))
        .child(Text::new(move || {
          locale.with(|locale| now.with(|now| locale.strftime(now, "%a %d %b  %H:%M")))
        }))
        .child(
          popup_button("edit-paste-symbolic")
            .on_click(move |cx| cx.open_popup(move |_| clipboard_popup(clipboard))),
        )
        .child(
          popup_button("image-x-generic-symbolic")
            .on_click(|cx| cx.open_popup(|cx| wallpaper_popup(cx, wallpapers()))),
        ),
    )
}

/// `label` and `value` as a percentage, or a dash before the first
/// sample, which is the only one with no memory.
pub fn share(label: &str, stats: &SystemStats, value: f32) -> String {
  match stats.memory_total {
    0 => format!("{label} –"),
    _ => format!("{label} {:.0}%", value * 100.0),
  }
}

/// The share of memory in use, from 0 to 1.
fn memory_used(stats: &SystemStats) -> f32 {
  1.0 - stats.memory_available as f32 / stats.memory_total.max(1) as f32
}

/// The CPU's temperature: the first sensor of a CPU's chip, or else the
/// first sensor.
pub fn cpu_temperature(stats: &SystemStats) -> Option<f32> {
  let cpu = ["k10temp", "coretemp", "zenpower", "cpu"];
  let sensors = &stats.temperatures;
  sensors
    .iter()
    .find(|sensor| cpu.iter().any(|chip| sensor.name.starts_with(chip)))
    .or(sensors.first())
    .map(|sensor| sensor.celsius)
}

/// `output`'s workspaces, niri's when `NIRI_SOCKET` is set and Sway's
/// otherwise, each as its id, name and whether it shows. A click switches
/// to one.
fn workspaces(cx: &Cx, output: String) -> Row {
  let niri = std::env::var_os("NIRI_SOCKET").is_some();
  let list: Box<dyn Fn() -> Vec<(u64, String, bool)>> = if niri {
    let all = cx.niri().workspaces();
    Box::new(move || {
      let here = |w: &&NiriWorkspace| w.output.as_deref() == Some(output.as_str());
      let label = |w: &NiriWorkspace| w.name.clone().unwrap_or_else(|| w.idx.to_string());
      all.with(|all| {
        all
          .iter()
          .filter(here)
          .map(|w| (w.id, label(w), w.is_active))
          .collect()
      })
    })
  } else {
    let all = cx.sway().workspaces();
    Box::new(move || {
      let here = all.with(|all| {
        all
          .iter()
          .filter(|w| w.output == output)
          .cloned()
          .collect::<Vec<_>>()
      });
      here
        .into_iter()
        .map(|w| (w.id as u64, w.name, w.focused))
        .collect()
    })
  };
  Row::new().gap(4).children_keyed(
    list,
    |w| w.0,
    move |_cx, ws| {
      Button::new(Text::new(move || ws.get().1))
        .style(move || if ws.get().2 { WS_ACTIVE } else { WS })
        .on_click(move |cx| match ws.get_untracked() {
          (id, _, _) if niri => cx.niri().focus_workspace(id),
          (_, name, _) => cx.sway().focus_workspace(&name),
        })
    },
  )
}

/// A slider for the first backlight, shown while there is one.
fn brightness(cx: &Cx, backlights: ReadSignal<Vec<Backlight>>) -> Row {
  let screen = cx.memo(move || backlights.with(|all| all.first().cloned()));
  let percent = cx.signal(0.0_f32);
  let dragging = cx.signal(false);
  // The backlight moves the handle, except while it is held.
  cx.effect(move || {
    if let (Some(screen), false) = (screen.get(), dragging.get()) {
      percent.set(percent_of(&screen) as f32);
    }
  });
  // And a value the backlight doesn't have yet goes to it.
  let parent = cx.clone();
  cx.effect(move || {
    let wanted = percent.get().round() as u32;
    let target = screen.with_untracked(|screen| {
      let screen = screen
        .as_ref()
        .filter(|screen| percent_of(screen) != wanted)?;
      let level = u64::from(screen.max_brightness) * u64::from(wanted) / 100;
      Some((screen.name.clone(), level as u32))
    });
    if let Some((name, level)) = target {
      parent.brightness().set(&name, level);
    }
  });
  Row::new()
    .gap(6)
    .align(Align::Center)
    .visible(move || screen.with(Option::is_some))
    .child(Icon::new("display-brightness-symbolic").size(16).tint(TEXT))
    .child(
      Slider::new(percent)
        .range(0.0, 100.0)
        .step(1.0)
        .dragging(dragging)
        .style(TRACK)
        .fill_style(FILL)
        .handle_style(HANDLE),
    )
}

/// How bright `screen` is, from 0 to 100.
fn percent_of(screen: &Backlight) -> u32 {
  let max = u64::from(screen.max_brightness.max(1));
  (u64::from(screen.brightness) * 100 / max) as u32
}

/// A button showing the symbolic `icon`, for a popup.
fn popup_button(icon: &str) -> Button {
  Button::new(Icon::new(icon).size(16).tint(TEXT))
    .style(ICON_BUTTON)
    .hover_style(HOVER)
}

/// The clipboard's latest entries, newest first. A click puts one back
/// on the clipboard.
fn clipboard_popup(clipboard: Clipboard) -> PopupWindow {
  let history = clipboard.history();
  let latest = move || history.with(|all| all.iter().take(CLIPS_SHOWN).cloned().collect());
  PopupWindow::new().style(POPUP).child(
    Column::new()
      .gap(4)
      .child(Text::new("Clipboard").style(HEADING))
      .child(
        Text::new("Nothing copied yet")
          .style(BODY)
          .visible(move || history.with(Vec::is_empty)),
      )
      .children_keyed(
        latest,
        |entry: &ClipboardEntry| entry.id,
        move |_cx, entry| {
          let entry = entry.get_untracked();
          let row = Row::new().style(CLIP_TEXT);
          let row = match entry.content {
            ClipboardContent::Text(text) => {
              let line = text.lines().next().unwrap_or_default().trim().to_owned();
              row.child(Text::new(line).max_lines(1).ellipsis(true))
            }
            ClipboardContent::Image(path) => {
              row.child(Image::new(path).height(48).width(96).fit(Fit::Contain))
            }
          };
          Button::new(row)
            .style(CLIP)
            .hover_style(HOVER)
            .on_click(move |cx| {
              clipboard.copy(entry.id);
              cx.close_popup();
            })
        },
      ),
  )
}

/// The picker's folder: `WALLPAPERS`, or `~/Pictures/Wallpapers`.
fn wallpapers() -> PathBuf {
  wallpapers_from(std::env::var_os("WALLPAPERS"), std::env::var_os("HOME"))
}

/// The picker's folder from `WALLPAPERS` and `HOME`, made absolute
/// against the current directory, so its pictures' URIs are whole.
pub fn wallpapers_from(folder: Option<OsString>, home: Option<OsString>) -> PathBuf {
  let folder = match folder {
    Some(folder) => PathBuf::from(folder),
    None => PathBuf::from(home.unwrap_or_default()).join("Pictures/Wallpapers"),
  };
  std::path::absolute(&folder).unwrap_or(folder)
}

/// Whether `name` is a picture an `Image` shows.
pub fn is_picture(name: &str) -> bool {
  let extension = name.rsplit_once('.').map(|(_, extension)| extension);
  let extension = extension.unwrap_or_default().to_ascii_lowercase();
  ["png", "jpg", "jpeg", "webp", "gif"].contains(&extension.as_str())
}

/// The pictures in `folder`, three a row in two rows' height, read
/// while the popup is open. Only the rows in view are built, however many
/// pictures the folder holds.
fn wallpaper_popup(cx: &Cx, folder: PathBuf) -> PopupWindow {
  let entries = cx.folder_view(folder).entries();
  let pictures = move || {
    entries.with(|all| {
      let pictures = all.iter().filter(|e| !e.is_dir && is_picture(&e.name));
      pictures.cloned().collect::<Vec<_>>()
    })
  };
  let none = move || entries.with(|all| !all.iter().any(|e| !e.is_dir && is_picture(&e.name)));
  let grid = GridView::new(
    pictures,
    |e: &FolderEntry| e.path.clone(),
    |_cx, picture| {
      let gap = Style::new().padding(CELL_GAP / 2, CELL_GAP / 2);
      Row::new()
        .style(gap)
        .child(thumbnail(picture.get_untracked()))
    },
  )
  .cell_size(CELL_WIDTH, CELL_HEIGHT)
  .height(2 * CELL_HEIGHT)
  .style(Style::new().width((3 * CELL_WIDTH) as f32))
  .scroll_bar(SCROLL_BAR);
  PopupWindow::new().style(POPUP).child(
    Column::new()
      .gap(8)
      .child(Text::new("Wallpapers").style(HEADING))
      .child(Text::new("No pictures here").style(BODY).visible(none))
      .child(grid),
  )
}

/// A picture in the picker, which drags out to other programs as its
/// `file://` URI, as a file manager's does.
pub fn thumbnail(picture: FolderEntry) -> Button {
  let uri = file_uri(&picture.path);
  Button::new(
    Column::new()
      .gap(4)
      .child(
        Image::new(picture.path)
          .width(THUMB_WIDTH)
          .height(THUMB_HEIGHT)
          .fit(Fit::Cover)
          .style(PICTURE),
      )
      .child(
        Text::new(picture.name)
          .max_lines(1)
          .ellipsis(true)
          .style(NAME),
      ),
  )
  .style(THUMB)
  .hover_style(HOVER)
  .drag_content(Some(DragContent::Uris(vec![uri])))
}

/// The `file://` URI of the absolute `path`, each byte but letters,
/// digits, `/` and `-._~` written as `%XX`, as RFC 8089 and 3986 say.
pub fn file_uri(path: &Path) -> String {
  path
    .as_os_str()
    .as_bytes()
    .iter()
    .fold(String::from("file://"), |mut uri, &byte| {
      if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
        uri.push(char::from(byte));
      } else {
        uri.push_str(&format!("%{byte:02X}"));
      }
      uri
    })
}
