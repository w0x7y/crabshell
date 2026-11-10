//! Layout: rows, columns, stacks, spacers, buttons and the window itself.
//! Icons have exact sizes, so these tests can check exact rectangles.

use super::{Harness, node_at, rect_at};
use crate::text::TextNode;
use crate::{Align, Button, Column, Content, Icon, Justify, Rect, Row, Spacer, Stack, Style, Text};

#[test]
fn the_window_child_fills_the_window() {
    let mut h = Harness::new();
    let window = h.window(h.panel().child(Row::new()), 200.0, 30.0);
    assert_eq!(rect_at(&window, &[]), Rect::new(0.0, 0.0, 200.0, 30.0));
    assert_eq!(rect_at(&window, &[0]), Rect::new(0.0, 0.0, 200.0, 30.0));
}

#[test]
fn content_too_wide_overflows_inside_the_window_child() {
    let mut h = Harness::new();
    let row = Row::new()
        .child(Icon::new("a").size(150))
        .child(Spacer::new())
        .child(Icon::new("b").size(100));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    // The row stays the window's size instead of growing to its content.
    assert_eq!(rect_at(&window, &[0]), Rect::new(0.0, 0.0, 200.0, 30.0));
    assert_eq!(rect_at(&window, &[0, 1]).width, 0.0);
    assert_eq!(rect_at(&window, &[0, 2]).right(), 250.0);
}

#[test]
fn a_row_places_children_with_padding_gap_and_a_spacer() {
    let mut h = Harness::new();
    let row = Row::new()
        .padding(4, 8)
        .gap(6)
        .child(Icon::new("a").size(10))
        .child(Icon::new("b").size(12))
        .child(Spacer::new())
        .child(Icon::new("c").size(16));
    let window = h.window(h.panel().child(row), 200.0, 30.0);

    // Children stretch to the row's 22-pixel content height by default.
    assert_eq!(rect_at(&window, &[0, 0]), Rect::new(8.0, 4.0, 10.0, 22.0));
    assert_eq!(rect_at(&window, &[0, 1]), Rect::new(24.0, 4.0, 12.0, 22.0));
    // 184 wide inside the padding, minus 38 of icons and 18 of gaps.
    assert_eq!(rect_at(&window, &[0, 2]), Rect::new(42.0, 4.0, 128.0, 22.0));
    assert_eq!(rect_at(&window, &[0, 3]), Rect::new(176.0, 4.0, 16.0, 22.0));
}

#[test]
fn a_stretched_icon_is_drawn_centered_at_its_size() {
    let mut h = Harness::new();
    let row = Row::new().padding(4, 8).child(Icon::new("a").size(10));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    let view = window.view();
    let Content::Icon(icon) = node_at(view.root(), &[0, 0]).content() else {
        panic!("an icon node has icon content");
    };
    assert_eq!(icon.rect, Rect::new(8.0, 10.0, 10.0, 10.0));
}

#[test]
fn align_and_justify_place_children() {
    let mut h = Harness::new();
    let row = Row::new()
        .padding(4, 8)
        .gap(6)
        .align(Align::Center)
        .justify(Justify::Center)
        .child(Icon::new("a").size(10))
        .child(Icon::new("b").size(12));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    // (184 - 28) / 2 = 78 from the content edge; (22 - 10) / 2 = 6 down.
    assert_eq!(rect_at(&window, &[0, 0]), Rect::new(86.0, 10.0, 10.0, 10.0));
    assert_eq!(rect_at(&window, &[0, 1]), Rect::new(102.0, 9.0, 12.0, 12.0));

    let mut h = Harness::new();
    let row = Row::new()
        .align(Align::End)
        .justify(Justify::SpaceBetween)
        .child(Icon::new("a").size(10))
        .child(Icon::new("b").size(12));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 20.0, 10.0, 10.0));
    assert_eq!(
        rect_at(&window, &[0, 1]),
        Rect::new(188.0, 18.0, 12.0, 12.0)
    );
}

#[test]
fn a_column_stacks_children_downward() {
    let mut h = Harness::new();
    let column = Column::new()
        .gap(2)
        .padding(1, 3)
        .child(Icon::new("a").size(10))
        .child(Spacer::new())
        .child(Icon::new("b").size(12));
    let window = h.window(h.panel().child(column), 40.0, 100.0);
    assert_eq!(rect_at(&window, &[0, 0]), Rect::new(3.0, 1.0, 34.0, 10.0));
    assert_eq!(rect_at(&window, &[0, 1]), Rect::new(3.0, 13.0, 34.0, 72.0));
    assert_eq!(rect_at(&window, &[0, 2]), Rect::new(3.0, 87.0, 34.0, 12.0));
}

#[test]
fn a_stack_puts_children_in_one_cell_sized_to_the_largest() {
    let mut h = Harness::new();
    let row = Row::new().align(Align::Start).child(
        Stack::new()
            .child(Icon::new("a").size(10))
            .child(Icon::new("b").size(20)),
    );
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 20.0, 20.0));
    assert_eq!(
        rect_at(&window, &[0, 0, 0]),
        Rect::new(0.0, 0.0, 20.0, 20.0)
    );
    assert_eq!(
        rect_at(&window, &[0, 0, 1]),
        Rect::new(0.0, 0.0, 20.0, 20.0)
    );
}

#[test]
fn a_button_centers_its_child_inside_its_padding() {
    let mut h = Harness::new();
    let row = Row::new()
        .padding(4, 8)
        .child(Button::new(Icon::new("a").size(10)).style(Style::new().padding(2, 8)));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    assert_eq!(rect_at(&window, &[0, 0]), Rect::new(8.0, 4.0, 26.0, 22.0));
    assert_eq!(
        rect_at(&window, &[0, 0, 0]),
        Rect::new(16.0, 10.0, 10.0, 10.0)
    );
}

#[test]
fn row_padding_wins_over_style_padding() {
    let mut h = Harness::new();
    let row = Row::new()
        .style(Style::new().padding(1, 1))
        .padding(4, 8)
        .child(Icon::new("a").size(10));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    assert_eq!(rect_at(&window, &[0, 0]), Rect::new(8.0, 4.0, 10.0, 22.0));

    let mut h = Harness::new();
    let row = Row::new()
        .style(Style::new().padding(1, 2))
        .child(Icon::new("a").size(10));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    assert_eq!(rect_at(&window, &[0, 0]), Rect::new(2.0, 1.0, 10.0, 28.0));
}

#[test]
fn the_window_style_padding_insets_its_child() {
    let mut h = Harness::new();
    let panel = h
        .panel()
        .style(Style::new().padding(3, 5))
        .child(Row::new());
    let window = h.window(panel, 200.0, 30.0);
    assert_eq!(rect_at(&window, &[0]), Rect::new(5.0, 3.0, 190.0, 24.0));
}

#[test]
fn text_is_as_wide_as_its_glyphs_and_one_line_high() {
    let mut h = Harness::new();
    let row = Row::new()
        .align(Align::Start)
        .child(Text::new("hello"))
        .child(Text::new("hello world"));
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    let short = rect_at(&window, &[0, 0]);
    let long = rect_at(&window, &[0, 1]);
    assert!(short.width > 10.0, "{short:?}");
    assert!(long.width > short.width + 10.0, "{short:?} {long:?}");
    // The default font is 13 pixels with a line height of round(13 * 1.2).
    assert_eq!(short.height, 16.0);
    assert_eq!(long.height, 16.0);
    assert_eq!(long.x, short.right());
}

#[test]
fn a_text_measures_with_the_buffer_it_is_drawn_from() {
    let mut h = Harness::new();
    let row = Row::new()
        .align(Align::Start)
        .child(Text::new("hello world"));
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    let view = window.view();
    let node = node_at(view.root(), &[0, 0]);
    let Content::Text(text) = node.content() else {
        panic!("a text node has text content");
    };
    let runs: Vec<_> = text.runs().collect();
    assert_eq!(runs.len(), 1);
    assert_eq!(node.rect().width, runs[0].line_w.ceil());
    assert_eq!(text.content(), "hello world");
}

#[test]
fn stretched_text_is_centered_vertically() {
    let mut h = Harness::new();
    let row = Row::new().padding(4, 8).child(Text::new("hi"));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    let view = window.view();
    let node = node_at(view.root(), &[0, 0]);
    assert_eq!(node.rect().height, 22.0);
    let Content::Text(text) = node.content() else {
        panic!("a text node has text content");
    };
    // A 16-pixel line in a 22-pixel box starting at y = 4.
    assert_eq!(text.origin().y, 7.0);
    assert_eq!(text.origin().x, 8.0);
}

#[test]
fn ellipsis_lets_text_shrink_to_fit() {
    let long = "a sentence much too long for the narrow window it sits in";
    let mut h = Harness::new();
    let row = Row::new().child(Text::new(long));
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    let natural = rect_at(&window, &[0, 0]).width;
    assert!(natural > 120.0, "{natural}");

    let mut h = Harness::new();
    let row = Row::new().child(Text::new(long).ellipsis(true));
    let window = h.window(h.panel().child(row), 120.0, 30.0);
    let view = window.view();
    let node = node_at(view.root(), &[0, 0]);
    assert!(node.rect().width <= 120.0, "{:?}", node.rect());
    let Content::Text(text) = node.content() else {
        panic!("a text node has text content");
    };
    let line = text.runs().next().expect("one line");
    assert!(line.line_w <= 120.0, "{}", line.line_w);
    assert!(line.glyphs.len() < long.chars().count());
    // The line ends in the font's own "…" glyph.
    let mut dots = TextNode::new();
    dots.set_content(String::from("\u{2026}"));
    dots.prepare(crate::Font::DEFAULT, &mut h.text);
    let dots = dots.runs().next().expect("a line").glyphs[0].glyph_id;
    assert_ne!(dots, 0);
    assert_eq!(line.glyphs.last().map(|glyph| glyph.glyph_id), Some(dots));

    // Without an ellipsis the text keeps its width and overflows.
    let mut h = Harness::new();
    let row = Row::new().child(Text::new(long));
    let window = h.window(h.panel().child(row), 120.0, 30.0);
    assert_eq!(rect_at(&window, &[0, 0]).width, natural);
}

#[test]
fn max_lines_wraps_up_to_that_many_lines() {
    let long = "a sentence much too long for the narrow window it sits in";
    let mut h = Harness::new();
    let column = Column::new()
        .child(Text::new(long).max_lines(2))
        .child(Text::new(long).max_lines(20))
        .child(Text::new(long));
    let window = h.window(h.panel().child(column), 120.0, 400.0);
    let two = rect_at(&window, &[0, 0]);
    let many = rect_at(&window, &[0, 1]);
    assert_eq!(two.height, 32.0);
    assert!(two.width <= 120.0, "{two:?}");
    assert!(many.height > 32.0 && many.height % 16.0 == 0.0, "{many:?}");
    assert_eq!(rect_at(&window, &[0, 2]).height, 16.0);
}

#[test]
fn a_word_too_long_for_the_line_breaks_inside_it() {
    let word = "Supercalifragilisticexpialidocious-and-then-some";
    let mut h = Harness::new();
    let column = Column::new().child(Text::new(word).max_lines(3));
    let window = h.window(h.panel().child(column), 100.0, 200.0);
    let view = window.view();
    let node = node_at(view.root(), &[0, 0]);
    let Content::Text(text) = node.content() else {
        panic!("a text node has text content");
    };
    let widths: Vec<f32> = text.runs().map(|run| run.line_w).collect();
    assert!(widths.len() > 1, "{widths:?}");
    assert!(widths.iter().all(|&width| width <= 100.0), "{widths:?}");
}

#[test]
fn text_takes_the_nearest_font_and_color() {
    use crate::{Color, Font};

    let mut h = Harness::new();
    let panel = h
        .panel()
        .style(
            Style::new()
                .font(Font::new("sans-serif", 20.0))
                .color(Color::rgb(1, 2, 3)),
        )
        .child(
            Row::new()
                .align(Align::Start)
                .child(Text::new("inherits"))
                .child(
                    Text::new("own").style(
                        Style::new()
                            .font(Font::new("sans-serif", 10.0))
                            .color(Color::rgb(4, 5, 6)),
                    ),
                )
                .child(Button::new(Text::new("in a button"))),
        );
    let window = h.window(panel, 400.0, 40.0);
    assert_eq!(rect_at(&window, &[0, 0]).height, 24.0);
    assert_eq!(rect_at(&window, &[0, 1]).height, 12.0);
    let view = window.view();
    let color = |path: &[usize]| match node_at(view.root(), path).content() {
        Content::Text(text) => text.color(),
        _ => panic!("not a text node"),
    };
    assert_eq!(color(&[0, 0]), Color::rgb(1, 2, 3));
    assert_eq!(color(&[0, 1]), Color::rgb(4, 5, 6));
    assert_eq!(color(&[0, 2, 0]), Color::rgb(1, 2, 3));
}

#[test]
fn text_defaults_to_black() {
    let mut h = Harness::new();
    let window = h.window(h.panel().child(Text::new("x")), 100.0, 30.0);
    let view = window.view();
    let Content::Text(text) = node_at(view.root(), &[0]).content() else {
        panic!("a text node has text content");
    };
    assert_eq!(text.color(), crate::Color::rgb(0, 0, 0));
}

#[test]
fn bounds_cover_a_node_and_everything_it_paints() {
    let mut h = Harness::new();
    // The icon overflows its 16-pixel box by 4 above and below.
    let row = Row::new()
        .padding(10, 4)
        .child(Icon::new("x").size(24))
        .child(Text::new("text"));
    let window = h.window(h.panel().child(row), 100.0, 36.0);
    let view = window.view();
    let icon = node_at(view.root(), &[0, 0]);
    assert_eq!(icon.rect(), Rect::new(4.0, 10.0, 24.0, 16.0));
    assert_eq!(icon.bounds(), Rect::new(4.0, 6.0, 24.0, 24.0));
    let row = node_at(view.root(), &[0]);
    assert!(
        row.bounds().y <= 6.0 && row.bounds().bottom() >= 30.0,
        "{:?}",
        row.bounds()
    );
    // Text bounds reach past the line box by the ink allowance.
    let text = node_at(view.root(), &[0, 1]);
    assert!(
        text.bounds().y < text.rect().y,
        "{:?} {:?}",
        text.bounds(),
        text.rect()
    );
}
