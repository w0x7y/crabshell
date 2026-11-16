//! A dashboard over the wallpaper in the top left of each screen: the
//! battery as a ring, the CPU's use over the last minute as a graph,
//! and what plays, its album art sharp in front of a blurred copy, with
//! buttons that skip and pause it. The command line hides and shows it,
//! and changes the wallpaper, which crossfades to the new picture.
//!
//! ```sh
//! WALLPAPER=~/Pictures/hills.jpg cargo run --release -p crabshell --example dashboard
//! crabshell ipc dashboard toggle
//! crabshell ipc wallpaper set ~/Pictures/lake.jpg
//! ```
//!
//! The icons are symbolic ones from the Adwaita theme, tinted in the
//! text's color. The CPU is sampled from `/proc/stat` once a second, only
//! while the dashboard shows: procfs files change without telling
//! inotify, so a file view wouldn't see them move.

use std::path::PathBuf;

use crabshell::prelude::*;

const TEXT: Color = Color::rgb(0xcd, 0xd6, 0xf4);
const DIM: Color = Color::rgb(0x9a, 0xa0, 0xb8);
const ACCENT: Color = Color::rgb(0x89, 0xb4, 0xfa);
const GREEN: Color = Color::rgb(0xa6, 0xe3, 0xa1);
const PEACH: Color = Color::rgb(0xfa, 0xb3, 0x87);
const DARK: Color = Color::rgb(0x1e, 0x1e, 0x2e);
const SURFACE: Color = Color::rgb(0x31, 0x32, 0x44);
const RAISED: Color = Color::rgb(0x45, 0x47, 0x5a);

/// How a new wallpaper replaces the old one.
pub const WALLPAPER_FADE: Transition = Transition::new(800).easing(Easing::InOutCubic);

/// The icon theme the symbolic icons come from.
pub const ICON_THEME: &str = "Adwaita";

/// The dashboard's size; it sits `MARGIN` in from the top left.
pub const WIDTH: u32 = 560;
pub const HEIGHT: u32 = 2 * PADDING + CARD_HEIGHT as u32 + GAP + MEDIA_HEIGHT as u32;
pub const MARGIN: i32 = 24;
const PADDING: u32 = 16;
const GAP: u32 = 12;
/// The space inside the dashboard's padding.
const INNER: f32 = (WIDTH - 2 * PADDING) as f32;
const PANEL: Style = Style::new()
  .background(DARK.with_alpha(0.88))
  .color(TEXT)
  .font(Font::new("sans-serif", 13.0))
  .radius(16.0)
  .border(1.0, RAISED);
const HEADING: Style = Style::new().font(Font::new("sans-serif", 13.0).weight(700));
const BODY: Style = Style::new().color(DIM);
const CARD: Style = Style::new()
  .background(SURFACE.with_alpha(0.7))
  .radius(12.0)
  .padding(12, 12)
  .height(CARD_HEIGHT);
const CARD_HEIGHT: f32 = 120.0;

/// The battery's card, with its ring.
const BATTERY_WIDTH: f32 = 216.0;
pub const RING: f32 = 96.0;
const RING_WIDTH: f32 = 9.0;
const RING_STYLE: Style = Style::new().width(RING).height(RING);
const RING_TRACK: Color = RAISED;
/// From low at 12 o'clock to full all the way round, starting a little
/// before 12 so the round cap there isn't painted in the end's color.
const RING_PAINT: Gradient = Gradient::sweep(-10.0, PEACH, GREEN);
const PERCENT: Style = Style::new().font(Font::new("sans-serif", 20.0).weight(700));

/// The CPU's card: a graph of the last `HISTORY` samples, newest at the
/// right.
pub const HISTORY: usize = 60;
const GRAPH_WIDTH: f32 = INNER - BATTERY_WIDTH - GAP as f32 - 24.0;
const GRAPH_HEIGHT: f32 = 64.0;
const GRAPH: Style = Style::new().width(GRAPH_WIDTH).height(GRAPH_HEIGHT);
const GRAPH_FILL: Gradient =
  Gradient::linear(180.0, ACCENT.with_alpha(0.5), ACCENT.with_alpha(0.0));
const GRID_LINE: Color = RAISED;

/// What plays: art in front of a blurred copy of itself that fades out
/// toward the right.
const MEDIA_HEIGHT: f32 = 128.0;
const MEDIA: Style = Style::new()
  .background(SURFACE.with_alpha(0.7))
  .radius(12.0)
  .clip(true)
  .width(INNER)
  .height(MEDIA_HEIGHT);
const BACKDROP: Style = Style::new().mask(Mask::gradient(Gradient::linear(
  90.0,
  Color::rgb(0, 0, 0),
  Color::rgba(0, 0, 0, 0x50),
)));
pub const ART: u32 = 96;
const ART_STYLE: Style = Style::new()
  .width(ART as f32)
  .height(ART as f32)
  .radius(8.0)
  .clip(true);
/// A shadow cast by the letters, so the title reads over any picture.
const OVER_ART: Shadow = Shadow::new(0.0, 1.0, 3.0, Color::rgba(0, 0, 0, 0xc0)).follow_content();
const TITLE: Style = HEADING
  .font(Font::new("sans-serif", 15.0).weight(700))
  .shadow(OVER_ART);
const ARTIST: Style = Style::new().shadow(OVER_ART);
const CONTROL: Style = Style::new().padding(4, 4).radius(6.0);
const CONTROL_HOVER: Style = Style::new().background(RAISED.with_alpha(0.8));
/// A disabled button shows no style of its own, so its own is dimmed.
const CONTROL_OFF: Style = CONTROL.opacity(0.4);

#[allow(dead_code, reason = "the tests build the windows, not main")]
fn main() {
  crabshell::run(setup);
}

/// The wallpaper and the dashboard on every output, as a user runs them.
pub fn setup(cx: &Cx) {
  let feeds = Feeds {
    battery: cx.upower().display_device(),
    players: cx.mpris().players(),
    cpu: cpu_usage,
  };
  shell(cx, feeds);
}

/// The wallpaper `WALLPAPER` names and the dashboard from `feeds`, with
/// the command line's handlers: `wallpaper set <path>` and `dashboard
/// toggle`, `open` and `close`.
pub fn shell(cx: &Cx, feeds: Feeds) {
  cx.set_icon_theme(ICON_THEME);
  let wallpaper = cx.signal(std::env::var_os("WALLPAPER").map(PathBuf::from));
  cx.ipc("wallpaper", "set", move |args| {
    wallpaper.set(Some(wallpaper_path(args)?));
    Ok(String::new())
  });
  let outputs = cx.outputs();
  cx.per_output(move |cx, output| wallpaper_window(cx, output, outputs, wallpaper.read_only()));

  let shown = cx.signal(true);
  cx.ipc("dashboard", "toggle", move |_| {
    shown.update(|shown| *shown = !*shown);
    Ok(String::new())
  });
  // A set notifies even with the value it has, so only a change is set.
  let show = move |to: bool| {
    if shown.get_untracked() != to {
      shown.set(to);
    }
    Ok(String::new())
  };
  cx.ipc("dashboard", "open", move |_| show(true));
  cx.ipc("dashboard", "close", move |_| show(false));
  dashboard(cx, shown.read_only(), feeds);
}

/// The picture `wallpaper set` names. A relative path would resolve
/// against the shell's own directory, so only an absolute one is taken.
pub fn wallpaper_path(args: &[String]) -> Result<PathBuf, String> {
  args
    .first()
    .map(PathBuf::from)
    .filter(|path| path.is_absolute())
    .ok_or_else(|| String::from("set takes an absolute path"))
}

/// What the dashboard shows, so the tests can pin each of them.
#[derive(Clone, Copy)]
pub struct Feeds {
  /// The battery, as UPower's display device gives it.
  pub battery: ReadSignal<Option<PowerDevice>>,
  /// The media players.
  pub players: ReadSignal<Vec<Player>>,
  /// Starts the CPU's samples in a context that lives while the
  /// dashboard shows, and gives them, from 0 to 1, oldest first.
  pub cpu: fn(&Cx) -> ReadSignal<Vec<f32>>,
}

/// While `shown`, the dashboard on every output. Hidden, its context is
/// freed, its windows and the CPU's sampling with it. `shown` set to the
/// value it has changes nothing.
pub fn dashboard(cx: &Cx, shown: ReadSignal<bool>, feeds: Feeds) {
  let parent = cx.clone();
  let mut open: Option<Cx> = None;
  // A memo runs what reads it only when its value changes.
  let shown = cx.memo(move || shown.get());
  cx.effect(move || {
    if let Some(old) = open.take() {
      old.owner().dispose();
    }
    if shown.get() {
      let child = parent.child();
      untrack(|| {
        let cpu = (feeds.cpu)(&child);
        child.per_output(move |cx, output| panel(cx, output, feeds, cpu));
      });
      open = Some(child);
    }
  });
}

/// The wallpaper on `output`, filling it, behind everything. A new one
/// crossfades in, both pictures held only while it does.
pub fn wallpaper_window(
  cx: &Cx,
  output: &Output,
  outputs: ReadSignal<Vec<OutputInfo>>,
  wallpaper: ReadSignal<Option<PathBuf>>,
) -> PanelWindow {
  let name = output.name();
  let size = cx.memo(move || {
    outputs.with(|outputs| {
      outputs
        .iter()
        .find(|info| info.name == name)
        .map_or((0, 0), |info| (info.width, info.height))
    })
  });
  PanelWindow::new(output)
    .layer(Layer::Background)
    .anchors(Edge::Top | Edge::Bottom | Edge::Left | Edge::Right)
    .exclusive_zone(-1)
    .namespace("wallpaper")
    .style(Style::new().background(DARK))
    .child(
      Image::new(move || wallpaper.get().unwrap_or_default())
        .width(move || size.get().0)
        .height(move || size.get().1)
        .fit(Fit::Cover)
        .transition(ImageTransition::crossfade(WALLPAPER_FADE))
        .visible(move || wallpaper.with(Option::is_some)),
    )
}

/// The dashboard on `output`, from `feeds` and the CPU's samples.
pub fn panel(cx: &Cx, output: &Output, feeds: Feeds, cpu: ReadSignal<Vec<f32>>) -> PanelWindow {
  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Left)
    .margins(Margins::new(MARGIN, 0, 0, MARGIN))
    .width(WIDTH)
    .height(HEIGHT)
    .exclusive_zone(-1)
    .layer(Layer::Bottom)
    .namespace("dashboard")
    .child(
      Column::new()
        .style(PANEL)
        .padding(PADDING, PADDING)
        .gap(GAP)
        .child(
          Row::new()
            .gap(GAP)
            .child(battery(feeds.battery))
            .child(processor(cpu)),
        )
        .child(media(cx, feeds.players)),
    )
}

/// The battery's charge as a ring, and what it is doing.
fn battery(device: ReadSignal<Option<PowerDevice>>) -> Row {
  let battery = move || device.get().filter(|device| device.present);
  let ring = Canvas::new(move |pen| {
    let size = pen.size();
    let center = Point::new(size.width / 2.0, size.height / 2.0);
    let radius = size.width.min(size.height) / 2.0 - RING_WIDTH / 2.0;
    pen.stroke(
      &Path::circle(center, radius),
      &Stroke::new(RING_WIDTH, RING_TRACK),
    );
    let percent = device.with(|device| {
      device
        .as_ref()
        .filter(|device| device.present)
        .map(|device| device.percent.clamp(0.0, 100.0) as f32)
    });
    if let Some(percent) = percent.filter(|percent| *percent > 0.0) {
      pen.stroke(
        &Path::new().arc(center, radius, -90.0, percent * 3.6),
        &Stroke::new(RING_WIDTH, RING_PAINT).cap(Cap::Round),
      );
    }
  })
  .style(RING_STYLE);
  let label = Row::new()
    .justify(Justify::Center)
    .align(Align::Center)
    .child(
      Text::new(move || match battery() {
        Some(device) => format!("{}%", device.percent.round()),
        None => "–".to_owned(),
      })
      .style(PERCENT),
    );
  Row::new()
    .style(CARD.width(BATTERY_WIDTH))
    .gap(12)
    .align(Align::Center)
    .child(Stack::new().child(ring).child(label))
    .child(
      Column::new()
        .gap(4)
        .child(
          Row::new()
            .gap(6)
            .align(Align::Center)
            .child(
              Icon::new(move || {
                let name = battery()
                  .map(|device| device.icon_name)
                  .filter(|name| !name.is_empty());
                IconSource::Name(name.unwrap_or_else(|| "battery-missing-symbolic".to_owned()))
              })
              .size(16)
              .tint(TEXT),
            )
            .child(Text::new("Battery").style(HEADING)),
        )
        .child(Text::new(move || state(battery().as_ref())).style(BODY))
        .child(Text::new(move || time_left(battery().as_ref())).style(BODY)),
    )
}

/// What the battery is doing, in a word or two.
fn state(device: Option<&PowerDevice>) -> String {
  match device.map(|device| device.state) {
    None => "Not found",
    Some(BatteryState::Charging) => "Charging",
    Some(BatteryState::Discharging | BatteryState::PendingDischarge) => "On battery",
    Some(BatteryState::FullyCharged) => "Full",
    Some(BatteryState::Empty) => "Empty",
    Some(BatteryState::PendingCharge) => "Plugged in",
    Some(BatteryState::Unknown) => "",
  }
  .to_owned()
}

/// How long until the battery is empty or full, when UPower knows.
fn time_left(device: Option<&PowerDevice>) -> String {
  let Some(device) = device else {
    return String::new();
  };
  let (time, what) = match device.state {
    BatteryState::Charging => (device.time_to_full, "until full"),
    _ => (device.time_to_empty, "left"),
  };
  let Some(minutes) = time.map(|time| time.as_secs() / 60) else {
    return String::new();
  };
  match minutes / 60 {
    0 => format!("{minutes} min {what}"),
    hours => format!("{hours} h {} min {what}", minutes % 60),
  }
}

/// The CPU's use over the last minute: a line with a fading fill under
/// it, newest at the right, over dashed lines at each quarter.
fn processor(cpu: ReadSignal<Vec<f32>>) -> Column {
  let graph = Canvas::new(move |pen| {
    let size = pen.size();
    let grid = Stroke::new(1.0, GRID_LINE).dash(&[3.0, 3.0], 0.0);
    for quarter in 1..4 {
      let y = (size.height * quarter as f32 / 4.0).round() + 0.5;
      let line = Path::new()
        .move_to(Point::new(0.0, y))
        .line_to(Point::new(size.width, y));
      pen.stroke(&line, &grid);
    }
    let points: Vec<Point> = cpu.with(|samples| {
      let step = size.width / (HISTORY - 1) as f32;
      let start = HISTORY.saturating_sub(samples.len());
      samples
        .iter()
        .rev()
        .take(HISTORY)
        .rev()
        .enumerate()
        .map(|(at, usage)| {
          let x = (start + at) as f32 * step;
          // Inside the stroke's half width at either end.
          let y = 1.0 + (1.0 - usage.clamp(0.0, 1.0)) * (size.height - 2.0);
          Point::new(x, y)
        })
        .collect()
    });
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
      return;
    };
    let line = points[1..]
      .iter()
      .fold(Path::new().move_to(*first), |path, point| {
        path.line_to(*point)
      });
    let area = line
      .clone()
      .line_to(Point::new(last.x, size.height))
      .line_to(Point::new(first.x, size.height))
      .close();
    pen.fill(&area, GRAPH_FILL);
    pen.stroke(
      &line,
      &Stroke::new(2.0, ACCENT).join(Join::Round).cap(Cap::Round),
    );
  })
  .style(GRAPH);
  Column::new()
    .style(CARD.grow(1.0))
    .gap(10)
    .child(
      Row::new()
        .gap(6)
        .align(Align::Center)
        .child(Icon::new("computer-symbolic").size(16).tint(TEXT))
        .child(Text::new("CPU").style(HEADING))
        .child(Spacer::new())
        .child(
          Text::new(move || match cpu.with(|samples| samples.last().copied()) {
            Some(usage) => format!("{}%", (usage * 100.0).round()),
            None => "–".to_owned(),
          })
          .style(BODY),
        ),
    )
    .child(graph)
}

/// The player that plays, or else the first, with its art in front of a
/// blurred copy and buttons that skip and pause it.
fn media(cx: &Cx, players: ReadSignal<Vec<Player>>) -> Stack {
  let player = cx.memo(move || {
    players.with(|players| {
      players
        .iter()
        .find(|player| player.status == PlaybackStatus::Playing)
        .or(players.first())
        .cloned()
    })
  });
  let art =
    cx.memo(move || player.with(|player| player.as_ref().and_then(|p| art_path(&p.art_url))));
  let has_art = move || art.with(Option::is_some);
  let picture = move || art.get().unwrap_or_default();
  let id = move || player.with_untracked(|player| player.as_ref().map(|p| p.id.clone()));
  let can = move |test: fn(&Player) -> bool| move || player.with(|p| p.as_ref().is_some_and(test));
  let playing =
    move || player.with(|p| p.as_ref().map(|p| p.status)) == Some(PlaybackStatus::Playing);
  let buttons = Row::new()
    .gap(4)
    .child(
      control(
        || "media-skip-backward-symbolic",
        can(|p| p.can_go_previous),
      )
      .on_click(move |cx| id().map_or((), |id| cx.mpris().previous(&id))),
    )
    .child(
      control(
        move || {
          if playing() {
            "media-playback-pause-symbolic"
          } else {
            "media-playback-start-symbolic"
          }
        },
        can(|p| p.can_pause || p.can_play),
      )
      .on_click(move |cx| id().map_or((), |id| cx.mpris().play_pause(&id))),
    )
    .child(
      control(|| "media-skip-forward-symbolic", can(|p| p.can_go_next))
        .on_click(move |cx| id().map_or((), |id| cx.mpris().next(&id))),
    );
  let cover = Stack::new()
    .style(ART_STYLE.background(RAISED))
    .child(
      Row::new()
        .justify(Justify::Center)
        .align(Align::Center)
        .child(Icon::new("audio-x-generic-symbolic").size(32).tint(DIM)),
    )
    .child(
      Image::new(picture)
        .width(ART)
        .height(ART)
        .fit(Fit::Cover)
        .visible(has_art),
    );
  Stack::new()
    .style(MEDIA)
    .child(
      Image::new(picture)
        .width(INNER as u32)
        .height(MEDIA_HEIGHT as u32)
        .fit(Fit::Cover)
        .blur(18.0)
        .brightness(0.6)
        .style(BACKDROP)
        .visible(has_art),
    )
    .child(
      Row::new()
        .padding(16, 16)
        .gap(16)
        .align(Align::Center)
        .child(cover)
        .child(
          Column::new()
            .gap(4)
            .child(
              Text::new(move || {
                player.with(|p| match p {
                  Some(p) if !p.title.is_empty() => p.title.clone(),
                  Some(p) => p.identity.clone(),
                  None => "Nothing playing".to_owned(),
                })
              })
              .max_lines(1)
              .style(TITLE),
            )
            .child(
              Text::new(move || {
                player.with(|p| p.as_ref().map(|p| p.artists.join(", ")).unwrap_or_default())
              })
              .max_lines(1)
              .style(ARTIST),
            )
            .child(buttons.visible(move || player.with(Option::is_some))),
        ),
    )
}

/// A media button showing the symbolic `icon`, tinted, turned off while
/// `enabled` says the player can't.
fn control(
  icon: impl Fn() -> &'static str + 'static,
  enabled: impl Fn() -> bool + Copy + 'static,
) -> Button {
  Button::new(
    Icon::new(move || IconSource::Name(icon().to_owned()))
      .size(18)
      .tint(TEXT),
  )
  .style(move || if enabled() { CONTROL } else { CONTROL_OFF })
  .hover_style(CONTROL_HOVER)
  .enabled(enabled)
}

/// The file an MPRIS `artUrl` names, when it is a `file://` URL. Remote
/// art isn't fetched here.
pub fn art_path(url: &str) -> Option<PathBuf> {
  use std::os::unix::ffi::OsStringExt;

  let path = url.strip_prefix("file://")?.as_bytes();
  let mut bytes = Vec::with_capacity(path.len());
  let mut rest = path;
  while let Some((&byte, after)) = rest.split_first() {
    let hex = |at: usize| {
      after
        .get(at)
        .and_then(|digit| (*digit as char).to_digit(16))
    };
    match (byte, hex(0), hex(1)) {
      (b'%', Some(high), Some(low)) => {
        bytes.push((high * 16 + low) as u8);
        rest = &after[2..];
      }
      _ => {
        bytes.push(byte);
        rest = after;
      }
    }
  }
  bytes
    .starts_with(b"/")
    .then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// Samples the CPU's use once a second while `cx` lives, keeping the
/// last [`HISTORY`] samples, from 0 to 1, oldest first.
pub fn cpu_usage(cx: &Cx) -> ReadSignal<Vec<f32>> {
  let samples = cx.signal(Vec::new());
  let mut last = read_cpu();
  cx.interval(Duration::from_secs(1), move || {
    let now = read_cpu();
    if let Some(usage) = last.zip(now).and_then(|(then, now)| usage(then, now)) {
      samples.update(|samples| {
        if samples.len() == HISTORY {
          samples.remove(0);
        }
        samples.push(usage);
      });
    }
    last = now;
  });
  samples.read_only()
}

/// The CPU's busy and total time so far, from `/proc/stat`.
fn read_cpu() -> Option<(u64, u64)> {
  let stat = std::fs::read_to_string("/proc/stat").ok()?;
  cpu_times(&stat)
}

/// The busy and total time of all CPUs from the first line of
/// `/proc/stat`, in its ticks, or `None` for one that doesn't parse. Idle
/// and waiting for I/O count as not busy. Only the first eight fields
/// count: the kernel counts guest time in user and nice time already.
pub fn cpu_times(stat: &str) -> Option<(u64, u64)> {
  let mut fields = stat.lines().next()?.split_whitespace();
  if fields.next()? != "cpu" {
    return None;
  }
  let times: Vec<u64> = fields.map(str::parse).collect::<Result<_, _>>().ok()?;
  let total = times
    .iter()
    .take(8)
    .try_fold(0_u64, |sum, time| sum.checked_add(*time))?;
  let idle = times.get(3)? + times.get(4).copied().unwrap_or(0);
  Some((total.checked_sub(idle)?, total))
}

/// The share of the time from `then` to `now` the CPU was busy, or `None`
/// when no time passed or the counters went back.
pub fn usage(
  (busy_then, total_then): (u64, u64),
  (busy_now, total_now): (u64, u64),
) -> Option<f32> {
  let total = total_now
    .checked_sub(total_then)
    .filter(|total| *total > 0)?;
  let busy = busy_now.checked_sub(busy_then)?;
  Some((busy as f32 / total as f32).min(1.0))
}
