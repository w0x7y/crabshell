//! Invalidation: paint changes repaint one widget, layout changes repaint
//! what moved, and nothing changing means nothing to do.

use super::{Harness, rect_at};
use crate::{Align, Button, Color, Icon, Rect, Row, Size, Spacer, Stack, Style, Text};

#[test]
fn the_first_layout_damages_every_node_and_then_nothing_is_dirty() {
    let mut h = Harness::new();
    let row = Row::new().child(Icon::new("a").size(10));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    let damage = window.take_damage();
    assert!(damage.contains(&Rect::new(0.0, 0.0, 200.0, 30.0)));
    assert!(damage.contains(&rect_at(&window, &[0, 0])));

    h.frame(&window);
    assert!(!window.needs_layout());
    assert!(!window.has_damage());
}

#[test]
fn a_paint_change_repaints_only_that_widget() {
    let mut h = Harness::new();
    let dark = h.cx.signal(true);
    let row = Row::new()
        .padding(4, 8)
        .child(Icon::new("a").size(10))
        .child(Button::new(Icon::new("b").size(10)).style(move || {
            let background = if dark.get() { 0x11 } else { 0xee };
            Style::new().background(Color::rgb(background, background, background))
        }));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    window.take_damage();

    dark.set(false);
    h.runtime.flush();
    assert!(!window.needs_layout());
    assert_eq!(window.take_damage(), vec![rect_at(&window, &[0, 1])]);
}

#[test]
fn setting_an_equal_value_invalidates_nothing() {
    let mut h = Harness::new();
    let label = h.cx.signal(String::from("same"));
    let style = h.cx.signal(Style::new().background(Color::rgb(1, 2, 3)));
    let row = Row::new()
        .style(move || style.get())
        .child(Text::new(move || label.get()));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    window.take_damage();

    label.set(String::from("same"));
    style.set(Style::new().background(Color::rgb(1, 2, 3)));
    h.runtime.flush();
    assert!(!window.needs_layout());
    assert!(!window.has_damage());
}

#[test]
fn a_layout_change_repaints_what_moved_at_both_positions() {
    let mut h = Harness::new();
    let size = h.cx.signal(10);
    let row = Row::new()
        .align(Align::Start)
        .child(Icon::new("before").size(8))
        .child(Icon::new("grows").size(move || size.get()))
        .child(Icon::new("after").size(8));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    window.take_damage();
    let before = rect_at(&window, &[0, 0]);
    let old_grows = rect_at(&window, &[0, 1]);
    let old_after = rect_at(&window, &[0, 2]);

    size.set(20);
    h.runtime.flush();
    assert!(window.needs_layout());
    assert!(!window.has_damage());
    window.layout(&mut h.text);

    let new_grows = rect_at(&window, &[0, 1]);
    let new_after = rect_at(&window, &[0, 2]);
    assert_eq!(new_after.x, old_after.x + 10.0);
    let damage = window.take_damage();
    for rect in [old_grows, new_grows, old_after, new_after] {
        assert!(damage.contains(&rect), "{rect:?} missing from {damage:?}");
    }
    assert!(!damage.contains(&before), "{damage:?}");
    assert!(!window.needs_layout());
}

#[test]
fn new_text_is_shaped_and_measured_again() {
    let mut h = Harness::new();
    let label = h.cx.signal(String::from("a"));
    let row = Row::new()
        .align(Align::Start)
        .child(Text::new(move || label.get()))
        .child(Icon::new("after").size(8));
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    let short = rect_at(&window, &[0, 0]);

    label.set(String::from("a much longer label"));
    h.frame(&window);
    let long = rect_at(&window, &[0, 0]);
    assert!(long.width > short.width + 50.0, "{short:?} {long:?}");
    assert_eq!(rect_at(&window, &[0, 1]).x, long.right());
    let view = window.view();
    let crate::Content::Text(text) = super::node_at(view.root(), &[0, 0]).content() else {
        panic!("a text node has text content");
    };
    let drawn: Vec<&str> = text.runs().map(|run| run.text).collect();
    assert_eq!(drawn, ["a much longer label"]);
}

#[test]
fn a_layout_change_that_moves_nothing_still_repaints_its_widget() {
    let mut h = Harness::new();
    let label = h.cx.signal(String::from("ab"));
    let row = Row::new()
        .align(Align::Start)
        .child(Text::new(move || label.get()))
        .child(Icon::new("after").size(8));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    window.take_damage();
    let text = rect_at(&window, &[0, 0]);

    // The same glyphs in another order: the same width.
    label.set(String::from("ba"));
    h.frame(&window);
    assert_eq!(rect_at(&window, &[0, 0]), text);
    assert_eq!(window.take_damage(), vec![text]);
}

#[test]
fn a_style_change_to_padding_needs_a_layout() {
    let mut h = Harness::new();
    let wide = h.cx.signal(false);
    let button = Button::new(Icon::new("a").size(10)).style(move || {
        Style::new()
            .background(Color::rgb(1, 1, 1))
            .padding(0, if wide.get() { 8 } else { 2 })
    });
    let window = h.window(h.panel().child(Row::new().child(button)), 200.0, 30.0);
    window.take_damage();
    assert_eq!(rect_at(&window, &[0, 0]).width, 14.0);

    wide.set(true);
    h.runtime.flush();
    assert!(window.needs_layout());
    window.layout(&mut h.text);
    assert_eq!(rect_at(&window, &[0, 0]).width, 26.0);
    assert!(window.has_damage());
}

#[test]
fn a_font_change_on_an_ancestor_measures_its_text_again() {
    use crate::Font;

    let mut h = Harness::new();
    let big = h.cx.signal(false);
    let row = Row::new()
        .align(Align::Start)
        .style(move || {
            Style::new().font(Font::new("sans-serif", if big.get() { 20.0 } else { 10.0 }))
        })
        .child(Text::new("hello"));
    let window = h.window(h.panel().child(row), 400.0, 40.0);
    let small = rect_at(&window, &[0, 0]);
    assert_eq!(small.height, 12.0);

    big.set(true);
    h.frame(&window);
    let large = rect_at(&window, &[0, 0]);
    assert_eq!(large.height, 24.0);
    assert!(large.width > small.width, "{small:?} {large:?}");
}

#[test]
fn a_new_size_lays_out_again_and_repaints_old_and_new_area() {
    let mut h = Harness::new();
    let window = h.window(h.panel().child(Row::new()), 200.0, 30.0);
    window.take_damage();

    window.set_size(Size::new(200.0, 30.0));
    assert!(!window.needs_layout());

    window.set_size(Size::new(300.0, 30.0));
    assert!(window.needs_layout());
    window.layout(&mut h.text);
    let damage = window.take_damage();
    assert!(damage.contains(&Rect::new(0.0, 0.0, 200.0, 30.0)));
    assert!(damage.contains(&Rect::new(0.0, 0.0, 300.0, 30.0)));
}

#[test]
fn a_color_change_repaints_the_descendants_that_inherit_it() {
    let mut h = Harness::new();
    let red = h.cx.signal(false);
    // The row fills the 30-pixel window; its text overflows it.
    let row = Row::new()
        .style(move || Style::new().color(Color::rgb(if red.get() { 255 } else { 0 }, 0, 0)))
        .child(Button::new(Text::new("overflowing text")));
    let window = h.window(h.panel().child(row), 30.0, 30.0);
    window.take_damage();
    let text = rect_at(&window, &[0, 0, 0]);
    assert_eq!(rect_at(&window, &[0]).width, 30.0);
    assert!(text.right() > 30.0, "{text:?}");

    red.set(true);
    h.runtime.flush();
    let damage = window.take_damage();
    assert_eq!(damage.len(), 1);
    assert!(damage[0].right() >= text.right(), "{damage:?} {text:?}");
}

#[test]
fn empty_and_repeated_areas_are_left_out() {
    let mut h = Harness::new();
    let row = Row::new()
        .child(Spacer::new())
        .child(Stack::new().child(Icon::new("a").size(10)));
    let window = h.window(h.panel().child(row), 10.0, 10.0);
    // The spacer is 0 wide, and the stack and its icon share a rectangle.
    let damage = window.take_damage();
    assert_eq!(damage, vec![Rect::new(0.0, 0.0, 10.0, 10.0)]);
}

/// The top and bottom of the lines `path`'s text draws.
fn line_box(window: &crate::Window, path: &[usize]) -> (f32, f32) {
    let view = window.view();
    let crate::Content::Text(text) = super::node_at(view.root(), path).content() else {
        panic!("a text node has text content");
    };
    let top = text.origin().y;
    (
        top,
        top + text.runs().map(|run| run.line_height).sum::<f32>(),
    )
}

/// The smallest rectangle covering all of `damage`.
fn covered(damage: &[Rect]) -> Rect {
    damage
        .iter()
        .fold(Rect::default(), |all, rect| all.union(*rect))
}

#[test]
fn text_taller_than_its_box_is_repainted_where_it_is_drawn() {
    let mut h = Harness::new();
    let label = h.cx.signal(String::from("gy"));
    // A 20-pixel bar with 4 pixels of padding leaves 12 pixels for a
    // 16-pixel line, which overflows by 2 pixels above and below.
    let row = Row::new()
        .padding(4, 8)
        .child(Text::new(move || label.get()));
    let window = h.window(h.panel().child(row), 200.0, 20.0);
    window.take_damage();
    let (top, bottom) = line_box(&window, &[0, 0]);
    assert!(top < rect_at(&window, &[0, 0]).y, "the line overflows");

    label.set(String::from("yg"));
    h.frame(&window);
    let damage = covered(&window.take_damage());
    assert!(damage.y <= top && damage.bottom() >= bottom, "{damage:?}");
}

#[test]
fn a_paint_change_covers_the_texts_overflow() {
    let mut h = Harness::new();
    let red = h.cx.signal(false);
    let row = Row::new().padding(4, 8).child(
        Text::new("gy")
            .style(move || Style::new().color(Color::rgb(if red.get() { 255 } else { 0 }, 0, 0))),
    );
    let window = h.window(h.panel().child(row), 200.0, 20.0);
    window.take_damage();
    let (top, bottom) = line_box(&window, &[0, 0]);

    red.set(true);
    h.runtime.flush();
    let damage = covered(&window.take_damage());
    assert!(damage.y <= top && damage.bottom() >= bottom, "{damage:?}");
}

#[test]
fn the_damage_covers_every_glyph_pixel() {
    use cosmic_text::SwashCache;

    let mut h = Harness::new();
    // A capital with a ring and an accent above, and letters with descenders.
    let label = h.cx.signal(String::from("\u{1FA}gjy"));
    let row = Row::new()
        .align(Align::Center)
        .child(Text::new(move || label.get()));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    window.take_damage();

    // The same glyphs in another order, so the rectangle stays put.
    label.set(String::from("yjg\u{1FA}"));
    h.frame(&window);
    let damage = covered(&window.take_damage());

    let mut cache = SwashCache::new();
    let view = window.view();
    let crate::Content::Text(text) = super::node_at(view.root(), &[0, 0]).content() else {
        panic!("a text node has text content");
    };
    let (mut top, mut bottom) = (f32::MAX, f32::MIN);
    for run in text.runs() {
        let baseline = (text.origin().y + run.line_y).round();
        for glyph in run.glyphs {
            let key = glyph.physical((0.0, 0.0), 1.0).cache_key;
            if let Some(image) = cache.get_image_uncached(h.text.font_system(), key) {
                let glyph_top = baseline - image.placement.top as f32;
                top = top.min(glyph_top);
                bottom = bottom.max(glyph_top + image.placement.height as f32);
            }
        }
    }
    assert!(top < bottom, "some glyph was drawn");
    assert!(
        damage.y <= top && damage.bottom() >= bottom,
        "glyphs {top}..{bottom}, {damage:?}"
    );
}
