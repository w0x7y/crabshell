//! Whole windows drawn at scales 1, 1.5 and 2, or at all five, and
//! compared with golden images.

use std::rc::Rc;

use crabshell_ui::{
  Align, Color, Column, Content, Elide, Fit, Font, FontFeature, Gradient, Icon, IconPixels,
  IconSource, Image, Mask, Menu, MenuEntry, MenuToggle, Row, Shadow, Span, Stack, Style, Text,
  TextAlign, Window, parse_markup,
};

use super::{Scene, assert_golden, fixture};

const SCALES: [(f32, &str); 3] = [(1.0, "1x"), (1.5, "1.5x"), (2.0, "2x")];

const ALL_SCALES: [(f32, &str); 5] = [
  (1.0, "1x"),
  (1.25, "1.25x"),
  (1.5, "1.5x"),
  (1.75, "1.75x"),
  (2.0, "2x"),
];

const BAR: Style = Style::new()
  .background(Color::hex("#1e1e2e"))
  .color(Color::hex("#cdd6f4"));

/// Draws the window `build` makes at each scale and checks it against
/// `<name>@<scale>.png`.
fn check(name: &str, width: f32, height: f32, build: impl Fn(&Scene) -> crabshell_ui::PanelWindow) {
  check_at(&SCALES, name, width, height, build);
}

/// [`check`] at each of `scales`.
fn check_at(
  scales: &[(f32, &str)],
  name: &str,
  width: f32,
  height: f32,
  build: impl Fn(&Scene) -> crabshell_ui::PanelWindow,
) {
  for &(scale, suffix) in scales {
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

#[test]
fn gradients() {
  check("gradients", 220.0, 40.0, |scene| {
    scene.panel().style(BAR).child(
      Row::new()
        .padding(4, 6)
        .gap(6)
        .align(Align::Center)
        // Left to right, rounded.
        .child(
          Row::new().style(
            Style::new()
              .gradient(Gradient::linear(
                90.0,
                Color::hex("#f38ba8"),
                Color::hex("#89b4fa"),
              ))
              .radius(6.0)
              .padding(10, 20),
          ),
        )
        // Four colors at a slant, under a border.
        .child(
          Row::new().style(
            Style::new()
              .gradient(
                Gradient::linear(135.0, Color::hex("#a6e3a1"), Color::hex("#cba6f7"))
                  .stop(0.3, Color::hex("#f9e2af"))
                  .stop(0.6, Color::hex("#fab387")),
              )
              .border(2.0, Color::hex("#cdd6f4"))
              .radius(4.0)
              .padding(10, 24),
          ),
        )
        // A round glow from the middle, fading out over a background.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::hex("#313244"))
              .gradient(Gradient::radial(
                Color::rgba(0xf9, 0xe2, 0xaf, 255),
                Color::rgba(0xf9, 0xe2, 0xaf, 0),
              ))
              .radius(100.0)
              .padding(14, 30),
          ),
        ),
    )
  });
}

#[test]
fn shadows() {
  check("shadows", 240.0, 60.0, |scene| {
    scene.panel().style(BAR).child(
      Row::new()
        .padding(14, 14)
        .gap(18)
        .align(Align::Center)
        // A card lifted off the bar.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::hex("#45475a"))
              .radius(8.0)
              .shadow(Shadow::new(0.0, 4.0, 10.0, Color::rgba(0, 0, 0, 200)))
              .padding(12, 22),
          ),
        )
        // A sharp shadow down and to the right.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::hex("#f9e2af"))
              .shadow(Shadow::new(4.0, 4.0, 0.0, Color::hex("#f38ba8")))
              .padding(10, 16),
          ),
        )
        // A glow spread around a pill, through its translucent fill.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::rgba(0x89, 0xb4, 0xfa, 96))
              .radius(100.0)
              .shadow(Shadow::new(0.0, 0.0, 8.0, Color::hex("#89b4fa")).spread(2.0))
              .padding(10, 26),
          ),
        ),
    )
  });
}

#[test]
fn inset_shadows() {
  check("inset_shadows", 240.0, 60.0, |scene| {
    scene.panel().style(BAR).child(
      Row::new()
        .padding(12, 14)
        .gap(16)
        .align(Align::Center)
        // A pressed button: dark from the top.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::hex("#45475a"))
              .radius(8.0)
              .shadow(Shadow::new(0.0, 3.0, 4.0, Color::rgba(0, 0, 0, 180)).inset())
              .padding(12, 20),
          ),
        )
        // A sharp, moved one inside a border, square at two corners.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::hex("#f9e2af"))
              .border(2.0, Color::hex("#fab387"))
              .radii(0.0, 10.0, 0.0, 10.0)
              .shadow(Shadow::new(4.0, 4.0, 0.0, Color::hex("#f38ba8")).inset())
              .padding(12, 18),
          ),
        )
        // A glow spread in from the edges of a pill, over a gradient.
        .child(
          Row::new().style(
            Style::new()
              .gradient(Gradient::linear(
                90.0,
                Color::hex("#313244"),
                Color::hex("#1e1e2e"),
              ))
              .radius(100.0)
              .shadow(
                Shadow::new(0.0, 0.0, 8.0, Color::hex("#89b4fa"))
                  .spread(2.0)
                  .inset(),
              )
              .padding(12, 26),
          ),
        ),
    )
  });
}

#[test]
fn corners() {
  check("corners", 240.0, 60.0, |scene| {
    scene.panel().style(BAR).child(
      Row::new()
        .padding(10, 14)
        .gap(16)
        .align(Align::Center)
        // Four different corners, a border and a blurred shadow.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::hex("#f38ba8"))
              .border(2.0, Color::hex("#fab387"))
              .radii(0.0, 6.0, 14.0, 22.0)
              .shadow(Shadow::new(0.0, 3.0, 8.0, Color::rgba(0, 0, 0, 200)))
              .padding(14, 22),
          ),
        )
        // A gradient leaf with a sharp spread shadow.
        .child(
          Row::new().style(
            Style::new()
              .gradient(Gradient::linear(
                90.0,
                Color::hex("#a6e3a1"),
                Color::hex("#89b4fa"),
              ))
              .radii(16.0, 2.0, 16.0, 2.0)
              .shadow(Shadow::new(3.0, 3.0, 0.0, Color::hex("#11111b")).spread(1.0))
              .padding(12, 18),
          ),
        )
        // A clip cutting a taller child at two corners.
        .child(
          Row::new()
            .align(Align::Start)
            .style(
              Style::new()
                .border(1.0, Color::hex("#cdd6f4"))
                .radii(14.0, 0.0, 14.0, 0.0)
                .clip(true)
                .width(36.0)
                .height(30.0),
            )
            .child(
              Row::new().style(
                Style::new()
                  .background(Color::hex("#f9e2af"))
                  .width(36.0)
                  .height(40.0),
              ),
            ),
        )
        // Glass with a cut-out glow round at two corners.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::rgba(255, 255, 255, 40))
              .radii(0.0, 12.0, 0.0, 12.0)
              .shadow(
                Shadow::new(0.0, 0.0, 6.0, Color::hex("#cba6f7"))
                  .spread(1.0)
                  .cut_out(),
              )
              .padding(12, 16),
          ),
        ),
    )
  });
}

#[test]
fn sweep() {
  check("sweep", 220.0, 60.0, |scene| {
    scene.panel().style(BAR).child(
      Row::new()
        .padding(8, 14)
        .gap(16)
        .align(Align::Center)
        // A ring: a sweep around a circle, with a circle of the bar's
        // color inside it.
        .child(
          Stack::new()
            .style(
              Style::new()
                .gradient(
                  Gradient::sweep(0.0, Color::hex("#89b4fa"), Color::hex("#89b4fa"))
                    .stop(0.5, Color::hex("#f5c2e7")),
                )
                .radius(22.0)
                .width(44.0)
                .height(44.0),
            )
            .child(Row::new().style(BAR.radius(16.0).left(6.0).top(6.0).width(32.0).height(32.0))),
        )
        // Four colors from 135 degrees in a rounded box.
        .child(
          Row::new().style(
            Style::new()
              .gradient(
                Gradient::sweep(135.0, Color::hex("#a6e3a1"), Color::hex("#cba6f7"))
                  .stop(0.3, Color::hex("#f9e2af"))
                  .stop(0.6, Color::hex("#fab387")),
              )
              .radius(8.0)
              .padding(18, 30),
          ),
        )
        // A beam fading out to a clear stop, over a background.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::hex("#313244"))
              .gradient(
                Gradient::sweep(270.0, Color::hex("#f38ba8"), Color::rgba(0, 0, 0, 0))
                  .stop(0.25, Color::rgba(0, 0, 0, 0)),
              )
              .padding(18, 26),
          ),
        ),
    )
  });
}

#[test]
fn dashes() {
  check("dashes", 240.0, 60.0, |scene| {
    scene.panel().style(BAR).child(
      Row::new()
        .padding(10, 12)
        .gap(12)
        .align(Align::Center)
        // Dashes around rounded corners, over a background.
        .child(
          Row::new().style(
            Style::new()
              .background(Color::hex("#313244"))
              .border(2.0, Color::hex("#89b4fa"))
              .border_dash(6.0, 4.0)
              .radius(8.0)
              .padding(12, 22),
          ),
        )
        // Round dots around a pill.
        .child(
          Row::new().style(
            Style::new()
              .border(3.0, Color::hex("#f9e2af"))
              .border_dash(0.0, 7.0)
              .radius(100.0)
              .padding(10, 24),
          ),
        )
        // Long dashes, round at two corners only.
        .child(
          Row::new().style(
            Style::new()
              .border(2.0, Color::hex("#a6e3a1"))
              .border_dash(10.0, 3.0)
              .radii(0.0, 12.0, 0.0, 12.0)
              .padding(12, 18),
          ),
        )
        // A thin, fine dash on square corners.
        .child(
          Row::new().style(
            Style::new()
              .border(1.0, Color::hex("#cba6f7"))
              .border_dash(3.0, 2.0)
              .padding(12, 16),
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
  let italic = Style::new().font(Font::new("sans-serif", 13.0).italic());
  let panel = scene.panel().child(
    Row::new()
      .child(Text::new("x"))
      .child(Text::new("\u{2603}").style(bold))
      .child(Text::new("x").style(italic)),
  );
  let window = scene.window(panel, 100.0, 30.0);
  let view = window.view();
  for (index, expected, what, query) in [
    (0, "Noto Sans", "`sans-serif`", "sans-serif"),
    (
      1,
      "DejaVu Sans",
      "the snowman's bold fallback",
      "sans-serif:bold",
    ),
    (
      2,
      "Noto Sans Italic",
      "italic `sans-serif`",
      "sans-serif:italic",
    ),
  ] {
    let Content::Text(text) = super::node_at(view.root(), &[0, index]).content() else {
      panic!("a text node has text content");
    };
    // Only an italic face counts as italic: an upright one cosmic-text
    // slants draws other pixels.
    let face = text
      .runs()
      .flat_map(|run| run.glyphs.iter())
      .next()
      .and_then(|glyph| {
        let fonts = scene.text.font_system();
        let face = fonts.db().face(glyph.font_id)?;
        let (name, _) = face.families.first()?;
        Some(match face.style {
          cosmic_text::fontdb::Style::Normal => name.clone(),
          _ => format!("{name} Italic"),
        })
      })
      .unwrap_or_default();
    assert_eq!(
      face, expected,
      "the text goldens were drawn with {expected} for {what}, but fontconfig picks \
             {face:?} here (see `fc-match {query}`); they can't match"
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
fn text_styles() {
  check_fonts();
  const PROPORTIONAL: [FontFeature; 1] = [FontFeature::new(*b"pnum", 1)];
  const TABULAR: [FontFeature; 2] = [PROPORTIONAL[0], FontFeature::TABULAR_NUMBERS];
  const WRAPPED: &str = "words that wrap onto a second and third line";
  const LONG: &str = "start middle end";
  let font = |font: Font| Style::new().font(font);
  let boxed = |style: Style| style.width(110.0).background(Color::hex("#313244"));
  check("text_styles", 240.0, 226.0, |scene| {
    let clocks = |features| {
      Column::new()
        .align(Align::Start)
        .child(Text::new("11:11").style(font(Font::DEFAULT.features(features))))
        .child(Text::new("00:00").style(font(Font::DEFAULT.features(features))))
    };
    scene.panel().style(BAR).child(
      Column::new()
        .padding(4, 6)
        .gap(4)
        .align(Align::Start)
        .child(
          Row::new()
            .gap(10)
            .child(Text::new("Italic text").style(font(Font::DEFAULT.italic())))
            .child(Text::new("spaced").style(font(Font::DEFAULT.letter_spacing(3.0))))
            .child(clocks(&PROPORTIONAL))
            .child(clocks(&TABULAR)),
        )
        .child(
          Row::new()
            .gap(8)
            .child(
              Text::new(WRAPPED)
                .max_lines(3)
                .style(boxed(Style::new().text_align(TextAlign::Center))),
            )
            .child(
              Text::new(WRAPPED)
                .max_lines(3)
                .style(boxed(Style::new().text_align(TextAlign::Justify))),
            ),
        )
        .child(
          Row::new()
            .gap(8)
            .child(
              Text::new("tall lines\nof text")
                .max_lines(2)
                .style(Style::new().line_height(1.8)),
            )
            .child(
              Column::new()
                .align(Align::Start)
                .style(Style::new().underline(true))
                .child(Text::new("underlined gjy"))
                .child(Text::new("and struck").style(Style::new().strikethrough(true)))
                .child(
                  Text::new("Bold, larger").style(font(Font::new("sans-serif", 20.0).weight(700))),
                ),
            ),
        )
        .child(Text::new("end").style(boxed(Style::new().text_align(TextAlign::End))))
        .child(
          Column::new()
            .align(Align::Start)
            .child(
              Text::new(LONG)
                .elide(Elide::Start)
                .style(Style::new().width(70.0)),
            )
            .child(
              Text::new(LONG)
                .elide(Elide::Middle)
                .style(Style::new().width(70.0)),
            )
            .child(
              Text::new(LONG)
                .elide(Elide::End)
                .style(Style::new().width(70.0)),
            ),
        ),
    )
  });
}

#[test]
fn rich_text() {
  check_fonts();
  const BODY: &str = "<b>Build</b> <i>finished</i> in <u>4 s</u> &amp; \
    <a href=\"https://example.org/log\">the log</a> <img src=\"/x.png\" alt=\"[ok]\"/>\n\
    next: <b><i>deploy &lt;prod&gt;</i></b>";
  check("rich_text", 220.0, 96.0, |scene| {
    let spans = vec![
      Span::new("Plain, "),
      Span::new("bold").bold(),
      Span::new(", "),
      Span::new("italic").italic(),
      Span::new(", "),
      Span::new("under").underline(),
      Span::new(" and "),
      Span::new("red struck")
        .color(Color::hex("#f38ba8"))
        .strikethrough(),
      Span::new(" with "),
      Span::new("a link")
        .underline()
        .color(Color::hex("#89b4fa"))
        .link("https://example.org"),
    ];
    scene.panel().style(BAR).child(
      Column::new()
        .padding(4, 6)
        .gap(6)
        .align(Align::Start)
        .child(
          Text::rich(spans)
            .max_lines(2)
            .style(Style::new().width(200.0)),
        )
        .child(Text::rich(parse_markup(BODY)).max_lines(2)),
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

/// A bordered, rounded view scrolled partway with a rounded bar inside
/// its border, and a rounded horizontal one whose bar along the bottom is
/// cut to its corner.
#[test]
fn scroll_bar() {
  check("scroll_bar", 140.0, 60.0, |scene| {
    let bands = || {
      (0..8).fold(Column::new(), |list, n| {
        let shade = if n % 2 == 0 { "#45475a" } else { "#585b70" };
        list.child(Row::new().style(Style::new().height(12.0).background(Color::hex(shade))))
      })
    };
    let across = (0..8).fold(Row::new(), |row, n| {
      let shade = if n % 2 == 0 { "#45475a" } else { "#585b70" };
      row.child(Row::new().style(Style::new().width(12.0).background(Color::hex(shade))))
    });
    let bar = Style::new()
      .width(5.0)
      .radius(2.5)
      .background(Color::hex("#f9e2af"));
    let offset = scene.cx.signal(30.0);
    scene.panel().style(BAR).child(
      Row::new()
        .padding(5, 5)
        .gap(10)
        .align(Align::Start)
        .child(
          crabshell_ui::ScrollView::new(bands())
            .offset(offset)
            .scroll_bar(bar)
            .style(
              Style::new()
                .width(40.0)
                .height(50.0)
                .radius(6.0)
                .border(1.5, Color::hex("#89b4fa")),
            ),
        )
        .child(
          crabshell_ui::ScrollView::new(across)
            .horizontal(true)
            .offset(offset)
            .scroll_bar(bar.width(3.0))
            .style(Style::new().width(60.0).height(30.0).radius(10.0)),
        ),
    )
  });
}

/// The stripes picture in 30 by 30 boxes: filled, contained and covered.
#[test]
fn images() {
  check("images", 120.0, 40.0, |scene| {
    let image = |fit| {
      Image::new(fixture("stripes.png"))
        .width(30)
        .height(30)
        .fit(fit)
    };
    scene.panel().style(BAR).child(
      Row::new()
        .padding(5, 5)
        .gap(5)
        .align(Align::Start)
        .child(image(Fit::Fill))
        .child(image(Fit::Contain))
        .child(image(Fit::Cover)),
    )
  });
}

/// A symbolic icon as it is, and tinted in the theme's text color and in
/// a translucent red, at 16 and 24.
#[test]
fn tinted_icons() {
  check("tinted_icons", 120.0, 32.0, |scene| {
    let icon = |size, tint| {
      Icon::new(fixture("symbolic.svg").as_path())
        .size(size)
        .tint(tint)
    };
    scene.panel().style(BAR).child(
      Row::new()
        .padding(4, 4)
        .gap(4)
        .align(Align::Center)
        .child(icon(16, None))
        .child(icon(16, Some(Color::hex("#cdd6f4"))))
        .child(icon(24, Some(Color::hex("#cdd6f4"))))
        .child(icon(24, Some(Color::rgba(0xf3, 0x8b, 0xa8, 0x80)))),
    )
  });
}

/// The swatches picture as it is, then through each filter: a tint,
/// brightness 0.5 and 1.5, contrast 0.5 and 2, saturation 0 and 2.
#[test]
fn image_filters() {
  check("image_filters", 296.0, 26.0, |scene| {
    let image = || Image::new(fixture("swatches.png")).width(32).height(16);
    scene.panel().style(BAR).child(
      Row::new()
        .padding(5, 5)
        .gap(4)
        .align(Align::Start)
        .child(image())
        .child(image().tint(Color::hex("#cdd6f4")))
        .child(image().brightness(0.5))
        .child(image().brightness(1.5))
        .child(image().contrast(0.5))
        .child(image().contrast(2.0))
        .child(image().saturation(0.0))
        .child(image().saturation(2.0)),
    )
  });
}

/// The stripes blurred by 3 pixels: filled, then contained, whose
/// letterbox stays clear.
#[test]
fn blurred_images() {
  check("blurred_images", 120.0, 50.0, |scene| {
    let image = |fit| {
      Image::new(fixture("stripes.png"))
        .width(50)
        .height(40)
        .fit(fit)
        .blur(3.0)
    };
    scene.panel().style(BAR).child(
      Row::new()
        .padding(5, 5)
        .gap(10)
        .align(Align::Start)
        .child(image(Fit::Fill))
        .child(image(Fit::Contain)),
    )
  });
}

/// Three fields: one focused with "world" selected and the caret before
/// it, an empty one showing its placeholder, and a password.
#[test]
fn input() {
  for (scale, suffix) in SCALES {
    let mut scene = Scene::new();
    let typed = scene.cx.signal(String::from("hello world"));
    let empty = scene.cx.signal(String::new());
    let secret = scene.cx.signal(String::from("hunter2"));
    let field = |text| {
      crabshell_ui::TextInput::new(text).style(
        Style::new()
          .width(70.0)
          .padding(2, 4)
          .background(Color::hex("#313244"))
          .radius(4.0),
      )
    };
    let panel = scene.panel().style(BAR).child(
      Row::new()
        .padding(4, 4)
        .gap(4)
        .align(Align::Center)
        .child(field(typed).focus(true))
        .child(field(empty).placeholder("Search"))
        .child(field(secret).password(true)),
    );
    let window = scene.window(panel, 230.0, 30.0);
    window.keyboard_enter();
    let shift = crabshell_ui::Modifiers {
      ctrl: true,
      shift: true,
      ..crabshell_ui::Modifiers::default()
    };
    window.key(&crabshell_ui::KeyEvent {
      key: crabshell_ui::Key::Left,
      text: None,
      modifiers: shift,
      repeat: false,
    });
    scene.frame(&window);
    let buffer = scene.full(&window, scale);
    assert_golden(&format!("input@{suffix}"), &buffer);
  }
}

/// A selectable text on two lines, selected from the middle of the first
/// to the middle of the second by a drag.
#[test]
fn selectable_text() {
  for (scale, suffix) in SCALES {
    let mut scene = Scene::new();
    let panel = scene.panel().style(BAR).child(
      Row::new().padding(4, 6).child(
        Text::new("hello world\nselect me")
          .max_lines(2)
          .selectable(true),
      ),
    );
    let window = scene.window(panel, 120.0, 44.0);
    let text = window
      .view()
      .root()
      .children()
      .next()
      .expect("a row")
      .children()
      .next()
      .expect("a text")
      .rect();
    let (top, bottom) = (
      text.y + text.height / 4.0,
      text.bottom() - text.height / 4.0,
    );
    window.pointer_motion(crabshell_ui::Point::new(text.x + 20.0, top));
    window.pointer_button(crabshell_ui::PointerButton::Left, true);
    window.pointer_motion(crabshell_ui::Point::new(text.x + 30.0, bottom));
    window.pointer_button(crabshell_ui::PointerButton::Left, false);
    scene.frame(&window);
    let buffer = scene.full(&window, scale);
    assert_golden(&format!("selectable_text@{suffix}"), &buffer);
  }
}

/// A focused text area whose first line wraps, with a selection from the
/// first line into the second and the caret at its end.
#[test]
fn text_area() {
  use crabshell_ui::{Key, KeyEvent, Modifiers};

  let press = |window: &Window, key, ctrl, shift| {
    window.key(&KeyEvent {
      key,
      text: None,
      modifiers: Modifiers {
        ctrl,
        shift,
        ..Modifiers::default()
      },
      repeat: false,
    });
  };
  for (scale, suffix) in SCALES {
    let mut scene = Scene::new();
    let note = scene
      .cx
      .signal(String::from("a note that wraps onto\nthe next line"));
    let panel = scene.panel().style(BAR).child(
      Row::new().padding(4, 4).align(Align::Start).child(
        crabshell_ui::TextArea::new(note).focus(true).style(
          Style::new()
            .width(100.0)
            .padding(2, 4)
            .background(Color::hex("#313244"))
            .radius(4.0),
        ),
      ),
    );
    let window = scene.window(panel, 120.0, 64.0);
    window.keyboard_enter();
    press(&window, Key::Home, true, false);
    press(&window, Key::Right, true, false);
    press(&window, Key::Down, false, true);
    scene.frame(&window);
    let buffer = scene.full(&window, scale);
    assert_golden(&format!("text_area@{suffix}"), &buffer);
  }
}

#[test]
fn a_field_draws_only_inside_its_content_box() {
  let mut scene = Scene::new();
  let text = scene.cx.signal(String::from("a long text that scrolls"));
  let panel = scene.panel().style(BAR).child(
    Row::new().padding(4, 10).align(Align::Center).child(
      crabshell_ui::TextInput::new(text)
        .focus(true)
        .style(Style::new().width(60.0).padding(2, 6)),
    ),
  );
  let window = scene.window(panel, 100.0, 30.0);
  window.keyboard_enter();
  scene.frame(&window);
  let buffer = scene.full(&window, 1.0);
  // The field runs from x 10 to 70 and its text from 16 to 64: anything
  // outside that is the bar's background.
  let background = [0x1e, 0x1e, 0x2e];
  for x in (0..16).chain(64..100) {
    for y in 0..30 {
      let at = ((y * buffer.width + x) * 4) as usize;
      assert_eq!(&buffer.pixels[at..at + 3], &background, "pixel {x},{y}");
    }
  }
}

/// A rounded card with a border and text, moved, scaled and turned; a
/// turned pair of boxes at half opacity, drawn as one layer; and a turned
/// clip, which cuts its children along its turned corners.
#[test]
fn transforms() {
  check_fonts();
  const CARD: Style = Style::new()
    .background(Color::hex("#45475a"))
    .border(2.0, Color::hex("#fab387"))
    .radius(6.0)
    .padding(4, 8);
  check_at(&ALL_SCALES, "transforms", 330.0, 64.0, |scene| {
    scene.panel().style(BAR).child(
      Row::new()
        .padding(10, 14)
        .gap(18)
        .align(Align::Center)
        // Half a pixel off the grid, so it is filtered.
        .child(
          Row::new()
            .style(CARD.translate(3.5, -6.0))
            .child(Text::new("moved")),
        )
        .child(Row::new().style(CARD.scale(1.4)).child(Text::new("big")))
        .child(
          Row::new()
            .style(CARD.rotate(20.0))
            .child(Text::new("turned")),
        )
        .child(
          Stack::new()
            .style(Style::new().opacity(0.5).rotate(-15.0))
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
        .child(
          Row::new()
            .style(
              CARD
                .padding(0, 0)
                .width(40.0)
                .height(30.0)
                .clip(true)
                .rotate(30.0),
            )
            .child(
              Row::new().style(
                Style::new()
                  .background(Color::hex("#f38ba8"))
                  .padding(20, 40),
              ),
            ),
        ),
    )
  });
}

/// An image in an ellipse, a list fading out at its bottom, and a sweep
/// cut to the shapes picture's alpha, the last turned and faded.
#[test]
fn masks() {
  check_fonts();
  let shapes: &'static str = fixture("shapes.svg").display().to_string().leak();
  check("masks", 200.0, 64.0, |scene| {
    let fade = Gradient::linear(180.0, Color::rgb(0, 0, 0), Color::rgba(0, 0, 0, 0))
      .stop(0.5, Color::rgb(0, 0, 0));
    scene.panel().style(BAR).child(
      Row::new()
        .padding(6, 10)
        .gap(14)
        .align(Align::Start)
        .child(
          Stack::new()
            .style(Style::new().mask(Mask::ellipse()))
            .child(Image::new(fixture("stripes.png")).width(48).height(48)),
        )
        .child(
          Column::new()
            .style(
              Style::new()
                .mask(Mask::gradient(fade))
                .height(52.0)
                .clip(true),
            )
            .child(Text::new("Firefox"))
            .child(Text::new("Terminal"))
            .child(Text::new("Files"))
            .child(Text::new("Settings")),
        )
        .child(
          Stack::new().style(
            Style::new()
              .gradient(
                Gradient::sweep(0.0, Color::hex("#89b4fa"), Color::hex("#89b4fa"))
                  .stop(0.5, Color::hex("#f38ba8")),
              )
              .mask(Mask::image(shapes))
              .width(44.0)
              .height(44.0)
              .rotate(-12.0)
              .opacity(0.9),
          ),
        ),
    )
  });
}

/// Text and an icon casting shadows through their alpha: one soft and
/// low, one sharp and spread, and one cut out under translucent text.
#[test]
fn content_shadows() {
  check_fonts();
  check("content_shadows", 220.0, 48.0, |scene| {
    let soft = Shadow::new(0.0, 2.0, 4.0, Color::rgba(0, 0, 0, 220)).follow_content();
    let sharp = Shadow::new(1.0, 1.0, 0.0, Color::hex("#f38ba8"))
      .spread(1.0)
      .follow_content();
    let glow = Shadow::new(0.0, 0.0, 6.0, Color::hex("#89b4fa"))
      .spread(2.0)
      .follow_content()
      .cut_out();
    scene
      .panel()
      .style(
        Style::new()
          .background(Color::hex("#45475a"))
          .color(Color::hex("#cdd6f4")),
      )
      .child(
        Row::new()
          .padding(6, 10)
          .gap(12)
          .align(Align::Center)
          .child(
            Text::new("14:05").style(
              Style::new()
                .font(Font::new("sans-serif", 22.0))
                .shadow(soft),
            ),
          )
          .child(
            Icon::new(fixture("shapes.svg").as_path())
              .size(32)
              .style(Style::new().shadow(sharp)),
          )
          .child(
            Text::new("glow").style(
              Style::new()
                .color(Color::rgba(255, 255, 255, 140))
                .shadow(glow),
            ),
          ),
      )
  });
}
