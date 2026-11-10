//! Colors, fonts and styles.

use crate::{Border, Color, Font, Padding, Style};

#[test]
fn hex_parses_both_forms_in_either_case() {
    assert_eq!(Color::hex("#1e1E2e"), Color::rgb(0x1e, 0x1e, 0x2e));
    assert_eq!(Color::hex("#CDD6F480"), Color::rgba(0xcd, 0xd6, 0xf4, 0x80));
}

#[test]
fn hex_works_in_a_const() {
    const THEME: Color = Color::hex("#45475a");
    assert_eq!(THEME, Color::rgb(0x45, 0x47, 0x5a));
}

#[test]
#[should_panic(expected = "Color::hex takes")]
fn hex_rejects_a_missing_hash() {
    Color::hex("1e1e2e");
}

#[test]
#[should_panic(expected = "isn't a hex digit")]
fn hex_rejects_other_characters() {
    Color::hex("#1e1e2g");
}

#[test]
fn builders_set_their_fields_and_nothing_else() {
    const WS: Style = Style::new().padding(2, 8).radius(4.0);
    assert_eq!(
        WS,
        Style {
            padding: Some(Padding {
                vertical: 2,
                horizontal: 8
            }),
            radius: Some(4.0),
            ..Style::new()
        }
    );
    let bordered = Style::new().border(1.5, Color::rgb(1, 2, 3)).opacity(0.5);
    assert_eq!(
        bordered.border,
        Some(Border {
            width: 1.5,
            color: Color::rgb(1, 2, 3)
        })
    );
    assert_eq!(bordered.opacity, Some(0.5));
    assert_eq!(bordered.background, None);
}

#[test]
fn merge_takes_the_fields_the_top_style_sets() {
    let base = Style::new()
        .background(Color::rgb(1, 1, 1))
        .color(Color::rgb(2, 2, 2))
        .padding(2, 8);
    let hover = Style::new().background(Color::rgb(9, 9, 9));
    let merged = base.merge(hover);
    assert_eq!(merged.background, Some(Color::rgb(9, 9, 9)));
    assert_eq!(merged.color, Some(Color::rgb(2, 2, 2)));
    assert_eq!(merged.padding, base.padding);
    assert_eq!(Style::new().merge(base), base);
}

#[test]
fn font_defaults_to_regular_weight() {
    const BOLD: Font = Font::new("Inter", 13.0).weight(700);
    assert_eq!(Font::new("Inter", 13.0).weight, 400);
    assert_eq!(BOLD.weight, 700);
    assert_eq!(BOLD.family, "Inter");
}
