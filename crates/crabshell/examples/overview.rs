//! An overview of the desktop that the command line opens and closes: the
//! installed applications to launch, recent ones first, the windows that
//! are open, the wallpaper's main colors, and what `uname -s` says.
//!
//! ```sh
//! cargo run --release -p crabshell --example overview
//! crabshell ipc overview toggle
//! ```
//!
//! `WALLPAPER` names the image whose colors it shows. It keeps the recent
//! applications in `$XDG_STATE_HOME/crabshell/overview.json`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crabshell::prelude::*;

const TEXT: Color = Color::rgb(0xcd, 0xd6, 0xf4);
const CARD: Style = Style::new()
  .background(Color::rgba(0x1e, 0x1e, 0x2e, 0xd8))
  .color(TEXT)
  .font(Font::new("sans-serif", 13.0))
  .padding(12, 14)
  .radius(12.0)
  .border(1.0, Color::rgba(0xcd, 0xd6, 0xf4, 0x30))
  // Under translucent glass the shadow would show through, so it is cut
  // out there.
  .shadow(Shadow::new(0.0, 6.0, 14.0, Color::rgba(0, 0, 0, 0x80)).cut_out());
const ITEM: Style = Style::new().padding(4, 8).radius(6.0);
/// An item's content: as wide as the column less the item's padding, so
/// it starts at the left instead of in the middle, as a button puts it.
const LINE: Style = Style::new().width(314.0);
const HOVER: Style = Style::new().background(Color::rgba(0x45, 0x47, 0x5a, 0xc0));
const DIM: Style = Style::new().color(Color::rgb(0x9a, 0xa0, 0xb8));
const TITLE: Style = Style::new().font(Font::new("sans-serif", 13.0).weight(700));

/// How many recently launched applications come first.
const RECENT: usize = 4;

#[allow(dead_code, reason = "the tests build the overview, not main")]
fn main() {
  crabshell::run(root);
}

/// The overview, closed until `crabshell ipc overview toggle` or `open`.
pub fn root(cx: &Cx) {
  let open = cx.signal(false);
  // At the root, so a write still to come outlives a closing overview.
  let recent: Signal<Vec<String>> = cx.json_file(recent_file(), Vec::new());
  cx.ipc("overview", "toggle", move |_| {
    open.update(|open| *open = !*open);
    Ok(String::new())
  });
  cx.ipc("overview", "open", move |_| {
    open.set(true);
    Ok(String::new())
  });
  cx.ipc("overview", "close", move |_| {
    open.set(false);
    Ok(String::new())
  });
  // While open, a context of its own holds a panel on every output;
  // closing disposes it, so a closed overview has no surface at all.
  let shown: Rc<RefCell<Option<Cx>>> = Rc::default();
  let parent = cx.clone();
  cx.effect(move || {
    if let Some(old) = shown.borrow_mut().take() {
      old.owner().dispose();
    }
    if open.get() {
      let child = parent.child();
      untrack(|| child.per_output(move |cx, output| overview(cx, output, open, recent)));
      *shown.borrow_mut() = Some(child);
    }
  });
}

/// The overview on `output`. Launching or picking a window closes it.
/// `recent` holds the ids of the applications launched last.
pub fn overview(
  cx: &Cx,
  output: &Output,
  open: Signal<bool>,
  recent: Signal<Vec<String>>,
) -> PanelWindow {
  PanelWindow::new(output)
    .anchors(Edge::Top)
    .margins(Margins::new(40, 0, 0, 0))
    .layer(Layer::Overlay)
    .width(760)
    .height(420)
    .exclusive_zone(-1)
    .namespace("overview")
    .child(
      // Room around the card for its shadow.
      Column::new().padding(18, 18).child(
        Column::new().gap(10).style(CARD).child(header(cx)).child(
          Row::new()
            .gap(12)
            .child(applications(cx, open, recent))
            .child(windows(cx, open)),
        ),
      ),
    )
}

/// What `uname -s` says, and the wallpaper's main colors.
fn header(cx: &Cx) -> Row {
  let system = cx.process(|| ["uname", "-s"]).stdout();
  let wallpaper = std::env::var_os("WALLPAPER").map(PathBuf::from);
  let colors = match wallpaper {
    Some(path) => cx.quantize_colors(path, 3),
    None => cx.signal(Vec::new()).read_only(),
  };
  Row::new()
    .gap(6)
    .align(Align::Center)
    .child(Text::new(move || system.get().trim().to_owned()).style(TITLE))
    .child(Spacer::new())
    .children_keyed(
      move || colors.get().into_iter().enumerate().collect(),
      |(at, _): &(usize, Color)| *at,
      |_, swatch| {
        Row::new().style(move || {
          Style::new()
            .background(swatch.get().1)
            .width(18.0)
            .height(18.0)
            .radius(9.0)
        })
      },
    )
}

/// Where the recently launched applications are kept.
fn recent_file() -> PathBuf {
  let state = std::env::var_os("XDG_STATE_HOME")
    .map(PathBuf::from)
    .filter(|dir| dir.is_absolute())
    .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
    .unwrap_or_default();
  state.join("crabshell/overview.json")
}

/// The applications, recent ones first. Hovering one shows what it is;
/// clicking it launches it.
fn applications(cx: &Cx, open: Signal<bool>, recent: Signal<Vec<String>>) -> Column {
  let entries = cx.desktop_entries();
  let all = entries.applications();
  let listed = cx.memo(move || {
    let recent = recent.get();
    let mut shown: Vec<DesktopEntry> = all
      .get()
      .into_iter()
      .filter(|entry| !entry.no_display)
      .collect();
    // A stable sort keeps the rest in name order.
    shown.sort_by_key(|entry| {
      recent
        .iter()
        .position(|id| *id == entry.id)
        .unwrap_or(RECENT)
    });
    shown.truncate(10);
    shown
  });
  Column::new()
    .gap(2)
    .child(Text::new("applications").style(DIM))
    .children_keyed(
      move || listed.get(),
      |entry: &DesktopEntry| entry.id.clone(),
      move |cx, entry| {
        let hovered = cx.signal(false);
        let icon = move || {
          let icon = entry.get().icon;
          if icon.starts_with('/') {
            IconSource::File(icon.into())
          } else {
            IconSource::Name(icon)
          }
        };
        Button::new(
          Row::new()
            .gap(8)
            .align(Align::Center)
            .style(LINE)
            .child(Icon::new(icon).size(20))
            .child(Text::new(move || {
              let entry = entry.get();
              match hovered.get() {
                // What it is, on hover.
                true if !entry.comment.is_empty() => format!("{} · {}", entry.name, entry.comment),
                _ => entry.name,
              }
            })),
        )
        .style(ITEM)
        .hover_style(HOVER)
        .hovered(hovered)
        .on_click(move |cx| {
          let id = entry.get().id;
          cx.desktop_entries().launch(&id);
          recent.update(|recent| {
            recent.retain(|other| *other != id);
            recent.insert(0, id);
            recent.truncate(RECENT);
          });
          open.set(false);
        })
      },
    )
}

/// The windows that are open; clicking one focuses it.
fn windows(cx: &Cx, open: Signal<bool>) -> Column {
  let toplevels = cx.toplevels();
  let list = toplevels.list();
  Column::new()
    .gap(2)
    .child(Text::new("windows").style(DIM))
    .children_keyed(
      move || list.get(),
      |toplevel: &Toplevel| toplevel.id,
      move |_, toplevel| {
        Button::new(Row::new().style(LINE).child(Text::new(move || {
          let toplevel = toplevel.get();
          if toplevel.app_id.is_empty() {
            toplevel.title
          } else {
            format!("{} · {}", toplevel.title, toplevel.app_id)
          }
        })))
        .style(ITEM)
        .hover_style(HOVER)
        .on_click(move |_| {
          toplevels.activate(toplevel.get().id);
          open.set(false);
        })
      },
    )
}
