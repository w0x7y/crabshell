//! A launcher: a box in the middle of every monitor that takes the
//! keyboard, with a search field that filters a scrolling list of apps,
//! and an about window. Type to filter, Up and Down to pick, Enter to
//! launch; it prints what it would run.
//!
//! ```sh
//! cargo run --release -p crabshell --example launcher
//! ```

use crabshell::prelude::*;

const BOX: Style = Style::new()
  .background(Color::rgb(0x1e, 0x1e, 0x2e))
  .color(Color::rgb(0xcd, 0xd6, 0xf4))
  .padding(12, 12)
  .radius(10.0);
const FIELD: Style = Style::new()
  .background(Color::rgb(0x31, 0x32, 0x44))
  .padding(6, 8)
  .radius(6.0);
const ROW: Style = Style::new().padding(4, 8).radius(4.0);
const PICKED: Style = ROW.background(Color::rgb(0x45, 0x47, 0x5a));

/// What the launcher lists when run on its own.
const APPS: &[&str] = &[
  "firefox",
  "files",
  "foot",
  "gimp",
  "inkscape",
  "kitty",
  "krita",
  "libreoffice",
  "mpv",
  "neovim",
  "obs",
  "pavucontrol",
  "signal",
  "steam",
  "thunderbird",
  "vlc",
];

#[allow(dead_code, reason = "the tests build the launcher, not main")]
fn main() {
  crabshell::run(|cx| {
    let about = cx.signal(false);
    cx.floating(about_window(about));
    cx.per_output(move |cx, output| {
      launcher(
        cx,
        output,
        APPS.iter().map(|app| app.to_string()).collect(),
        about,
      )
    });
  });
}

/// The window `about` shows; closing it sets `about` back.
pub fn about_window(about: Signal<bool>) -> FloatingWindow {
  FloatingWindow::new()
    .title("about crabshell")
    .width(320)
    .height(120)
    .visible(move || about.get())
    .on_close(move |_| about.set(false))
    .style(BOX)
    .child(Text::new("crabshell, a rust replacement for quickshell"))
}

/// The launcher on `output`, listing `apps`.
pub fn launcher(cx: &Cx, output: &Output, apps: Vec<String>, about: Signal<bool>) -> PanelWindow {
  let query = cx.signal(String::new());
  let picked = cx.signal(0_usize);
  let shown = cx.memo(move || {
    let query = query.get().to_lowercase();
    apps
      .iter()
      .filter(|app| app.contains(&query))
      .cloned()
      .collect::<Vec<_>>()
  });
  // A new query starts at the top of what it finds.
  cx.effect(move || {
    shown.get();
    picked.set(0);
  });
  let launch = move || {
    if let Some(app) = shown.with(|apps| apps.get(picked.get_untracked()).cloned()) {
      println!("launching {app}");
    }
  };
  PanelWindow::new(output)
    .layer(Layer::Overlay)
    .keyboard_focus(KeyboardFocus::Exclusive)
    .width(420)
    .height(360)
    .exclusive_zone(-1)
    .style(BOX)
    .on_key(move |_, key| {
      let count = shown.with(Vec::len);
      match key.key {
        Key::Down if picked.get_untracked() + 1 < count => picked.update(|n| *n += 1),
        Key::Up if picked.get_untracked() > 0 => picked.update(|n| *n -= 1),
        Key::Up | Key::Down => {}
        _ => return false,
      }
      true
    })
    .child(
      Column::new()
        .gap(8)
        .child(
          Row::new()
            .gap(8)
            .child(
              TextInput::new(query)
                .placeholder("search")
                .focus(true)
                .on_accept(move |_, _| launch())
                .style(FIELD.width(330.0)),
            )
            .child(
              Button::new(Text::new("about"))
                .style(FIELD)
                .on_click(move |_| about.set(true)),
            ),
        )
        .child(
          ListView::new(
            move || shown.get(),
            |app| app.clone(),
            move |_, app| {
              Row::new()
                .gap(8)
                .align(Align::Center)
                .style(move || {
                  let here = shown.with(|apps| apps.get(picked.get()) == Some(&app.get()));
                  if here { PICKED } else { ROW }
                })
                .child(Icon::new("application-x-executable").size(16))
                .child(Text::new(move || app.get()))
            },
          )
          .row_height(28)
          .height(280)
          .current(move || Some(picked.get())),
        ),
    )
}
