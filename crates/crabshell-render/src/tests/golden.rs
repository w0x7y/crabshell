//! Whole windows drawn at scales 1, 1.5 and 2 and compared with golden
//! images.

use std::rc::Rc;

use crabshell_ui::{
  Align, Color, Column, Content, Font, Icon, IconPixels, IconSource, Menu, MenuEntry, MenuToggle,
  Row, Stack, Style, Text, Window,
};

use super::{Scene, assert_golden, fixture};

const SCALES: [(f32, &str); 3] = [(1.0, "1x"), (1.5, "1.5x"), (2.0, "2x")];

const BAR: Style = Style::new()
  .background(Color::hex("#1e1e2e"))
  .color(Color::hex("#cdd6f4"));

/// Draws the window `build` makes at each scale and checks it against
/// `<name>@<scale>.png`.
fn check(name: &str, width: f32, height: f32, build: impl Fn(&Scene) -> crabshell_ui::PanelWindow) {
  for (scale, suffix) in SCALES {
    let mut scene = Scene::new();
    let panel = build(&scene);
    let window: Window = scene.window(panel, width, height);
    let buffer = scene.full(&window, scale);
    assert_golden(&format!("{name}@{suffix}"), &buffer);
  }
}

#[test]
fn styles() {
  check("styles", 220.0, 40.0, |scene| {
    scene.panel().style(BAR).child(
      Row::new()
        .padding(4, 6)
        .gap(6)
        .align(Align::Center)
        // A rounded background.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::hex("#f38ba8"))
              .radius(6.0)
              .padding(8, 12),
          ),
        )
        // A background with a border inside it.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::hex("#45475a"))
              .border(2.0, Color::hex("#fab387"))
              .radius(4.0)
              .padding(8, 10),
          ),
        )
        // A border alone, with a radius past half the height.
        .child(
          Row::new().style(
            Style::new()
              .border(1.5, Color::hex("#a6e3a1"))
              .radius(100.0)
              .padding(8, 16),
          ),
        )
        // Half opacity for two overlapping boxes as one layer: where
        // the yellow covers the blue, the blue doesn't show through.
        .child(
          Stack::new()
            .style(Style::new().opacity(0.5))
            .child(
              Row::new().style(
                Style::new()
                  .background(Color::hex("#89b4fa"))
                  .padding(10, 12),
              ),
            )
            .child(Row::new().padding(4, 4).align(Align::Start).child(
              Row::new().style(Style::new().background(Color::hex("#f9e2af")).padding(4, 4)),
            )),
        )
        // A translucent background and a thin border.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::rgba(255, 255, 255, 96))
              .border(0.5, Color::hex("#cba6f7"))
              .padding(8, 8),
          ),
        ),
    )
  });
}

/// Fails unless the fonts the text goldens were drawn with are the ones
/// fontconfig picks here: other fonts draw other pixels.
fn check_fonts() {
  let mut scene = Scene::new();
  let bold = Style::new().font(Font::new("sans-serif", 13.0).weight(700));
  let panel = scene.panel().child(
    Row::new()
      .child(Text::new("x"))
      .child(Text::new("\u{2603}").style(bold)),
  );
  let window = scene.window(panel, 100.0, 30.0);
  let view = window.view();
  for (index, expected, what) in [
    (0, "Noto Sans", "`sans-serif`"),
    (1, "DejaVu Sans", "the snowman's bold fallback"),
  ] {
    let Content::Text(text) = super::node_at(view.root(), &[0, index]).content() else {
      panic!("a text node has text content");
    };
    let family = text
      .runs()
      .flat_map(|run| run.glyphs.iter())
      .next()
      .and_then(|glyph| scene.text.font_system().db().face(glyph.font_id))
      .and_then(|face| face.families.first())
      .map(|(name, _)| name.clone())
      .unwrap_or_default();
    assert_eq!(
      family, expected,
      "the text goldens were drawn with {expected} for {what}, but fontconfig picks \
             {family:?} here (see `fc-match sans-serif`); they can't match"
    );
  }
}

#[test]
fn text() {
  check_fonts();
  check("text", 220.0, 72.0, |scene| {
    scene.panel().style(BAR).child(
      Column::new()
        .padding(2, 6)
        .child(Text::new("Crabshell 12:34 gjy"))
        // The ellipsis, in a row padded down to 48 pixels.
        .child(
          Row::new()
            .padding(0, 80)
            .child(Text::new("a sentence too long for the space").ellipsis(true)),
        )
        // A glyph from a fallback font, in another color and weight.
        .child(
          Text::new("snow \u{2603} man").style(
            Style::new()
              .color(Color::hex("#f9e2af"))
              .font(Font::new("sans-serif", 13.0).weight(700)),
          ),
        ),
    )
  });
}

#[test]
fn icons() {
  let pixels = IconPixels {
    width: 2,
    height: 2,
    // ARGB in network order: red, green, blue and half-transparent white.
    data: Rc::from(
      &[
        255, 255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 128, 255, 255, 255,
      ][..],
    ),
  };
  check("icons", 120.0, 32.0, |scene| {
    scene.panel().style(BAR).child(
      Row::new()
        .padding(4, 4)
        .gap(4)
        .align(Align::Center)
        .child(Icon::new(fixture("shapes.svg").as_path()).size(24))
        .child(Icon::new(fixture("disc.png").as_path()).size(24))
        .child(Icon::new(IconSource::Pixels(pixels.clone())).size(16))
        .child(Icon::new(fixture("shapes.svg").as_path()).size(12)),
    )
  });
}

#[test]
fn menu() {
  check_fonts();
  let entry = |id, label: &str| MenuEntry {
    id,
    label: label.into(),
    separator: false,
    enabled: true,
    toggle: None,
    checked: false,
    submenu: false,
    children: Vec::new(),
  };
  // An entry, a separator, a checked checkmark, a checked radio button, a
  // submenu and a disabled entry.
  let entries = vec![
    entry(1, "Open"),
    MenuEntry {
      separator: true,
      ..entry(2, "")
    },
    MenuEntry {
      toggle: Some(MenuToggle::Checkmark),
      checked: true,
      ..entry(3, "Wi-Fi")
    },
    MenuEntry {
      toggle: Some(MenuToggle::Radio),
      checked: true,
      ..entry(4, "Balanced")
    },
    MenuEntry {
      submenu: true,
      ..entry(5, "More")
    },
    MenuEntry {
      enabled: false,
      ..entry(6, "Off")
    },
  ];
  check("menu", 140.0, 150.0, |scene| {
    scene
      .panel()
      .style(BAR)
      .child(Menu::new(entries.clone()).style(Style::new().padding(4, 0)))
  });
}

/// A rounded box with a border that clips: a taller box inside is cut
/// inside the border along the bottom corners, and a label running past
/// the right side is cut mid-glyph.
fn clipping_panel(scene: &Scene, label: String) -> crabshell_ui::PanelWindow {
  scene.panel().style(BAR).child(
    Row::new()
      .padding(3, 3)
      .gap(4)
      .align(Align::Start)
      .style(
        Style::new()
          .background(Color::hex("#313244"))
          .border(2.0, Color::hex("#fab387"))
          .radius(10.0)
          .clip(true),
      )
      .child(
        Row::new().style(
          Style::new()
            .background(Color::hex("#f38ba8"))
            .padding(30, 12),
        ),
      )
      .child(Text::new(label)),
  )
}

#[test]
fn clip() {
  check("clip", 160.0, 40.0, |scene| {
    clipping_panel(scene, String::from("a label far too long to fit here"))
  });
}
