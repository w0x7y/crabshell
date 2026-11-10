//! `children_keyed`: children follow the items, reused by key.

use super::{Counter, Harness, capture_logs, node_at, paths};
use crabshell_reactive::Signal;

use crate::{Button, Content, Element, Icon, Row, Text, Widget, Window};

/// The text of each child of the window's row.
fn texts(window: &Window) -> Vec<String> {
    let view = window.view();
    node_at(view.root(), &[0])
        .children()
        .map(|child| match child.content() {
            Content::Text(text) => text.content().to_owned(),
            _ => String::from("(not text)"),
        })
        .collect()
}

#[test]
fn children_follow_the_items_and_are_reused_by_key() {
    let mut h = Harness::new();
    let items = h.cx.signal(vec![1, 2, 3]);
    let built = Counter::default();
    let counted = built.clone();
    let row = Row::new().children_keyed(
        move || items.get(),
        |n| *n,
        move |_cx, n| {
            counted.hit();
            Text::new(move || format!("item {}", n.get()))
        },
    );
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    assert_eq!(texts(&window), ["item 1", "item 2", "item 3"]);
    assert_eq!(built.get(), 3);

    items.set(vec![3, 1, 4]);
    h.frame(&window);
    assert_eq!(texts(&window), ["item 3", "item 1", "item 4"]);
    // Only item 4 is new. A child's path holds the order it was created in
    // among its siblings, so the reused children show that they were not
    // rebuilt, and the new one gets a number no other child had.
    assert_eq!(built.get(), 4);
    let row_paths: Vec<String> = paths(&window).into_iter().skip(2).collect();
    assert_eq!(
        row_paths,
        [
            "PanelWindow[DP-1]/Row/Text[2]",
            "PanelWindow[DP-1]/Row/Text[0]",
            "PanelWindow[DP-1]/Row/Text[3]",
        ]
    );
}

#[test]
fn a_child_updates_only_when_its_own_item_changes() {
    let mut h = Harness::new();
    let items = h.cx.signal(vec![(1_usize, "a"), (2, "b")]);
    let runs = [Counter::default(), Counter::default()];
    let counters = runs.clone();
    let row = Row::new().children_keyed(
        move || items.get(),
        |item| item.0,
        move |_cx, item| {
            let runs = counters[item.get_untracked().0 - 1].clone();
            Text::new(move || {
                runs.hit();
                item.get().1.to_owned()
            })
        },
    );
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    assert_eq!([runs[0].get(), runs[1].get()], [1, 1]);

    items.set(vec![(1, "a"), (2, "B")]);
    h.frame(&window);
    assert_eq!(texts(&window), ["a", "B"]);
    assert_eq!([runs[0].get(), runs[1].get()], [1, 2]);

    // Same keys, same values, same order: no layout and nothing to repaint.
    window.take_damage();
    items.set(vec![(1, "a"), (2, "B")]);
    h.runtime.flush();
    assert!(!window.needs_layout());
    assert!(!window.has_damage());
}

#[test]
fn removed_children_free_everything_they_created() {
    let mut h = Harness::new();
    let items = h.cx.signal(Vec::<u32>::new());
    let row = Row::new().children_keyed(
        move || items.get(),
        |n| *n,
        |cx, n| {
            let extra = cx.signal(0);
            Text::new(move || format!("{} {}", n.get(), extra.get()))
        },
    );
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    let empty = h.runtime.node_count();

    items.set(vec![1, 2, 3]);
    h.frame(&window);
    assert!(h.runtime.node_count() > empty);
    let rects: Vec<_> = {
        let view = window.view();
        node_at(view.root(), &[0])
            .children()
            .map(|child| child.rect())
            .collect()
    };
    window.take_damage();

    items.set(Vec::new());
    h.frame(&window);
    assert_eq!(h.runtime.node_count(), empty);
    assert!(texts(&window).is_empty());
    let damage = window.take_damage();
    for rect in rects {
        assert!(damage.contains(&rect), "{rect:?} missing from {damage:?}");
    }
}

#[test]
fn keyed_children_sit_between_the_static_ones() {
    let mut h = Harness::new();
    let items = h.cx.signal(vec!["b", "c"]);
    let row = Row::new()
        .child(Text::new("a"))
        .children_keyed(move || items.get(), |s| *s, |_cx, s| Text::new(s.get()))
        .child(Text::new("d"));
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    assert_eq!(texts(&window), ["a", "b", "c", "d"]);

    items.set(vec!["x"]);
    h.frame(&window);
    assert_eq!(texts(&window), ["a", "x", "d"]);
    // Single and keyed children share one count, so no two paths are equal.
    let all = paths(&window);
    let mut unique = all.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(all.len(), unique.len(), "{all:#?}");
}

#[test]
fn a_duplicate_key_shows_the_first_item_and_warns_once() {
    let mut h = Harness::new();
    let items = h.cx.signal(vec![(1, "first"), (1, "second"), (2, "other")]);
    let row = Row::new().children_keyed(
        move || items.get(),
        |item| item.0,
        |_cx, item| Text::new(item.get().1),
    );
    let mut window = None;
    let logs = capture_logs(|| {
        let built = h.window(h.panel().child(row), 400.0, 30.0);
        items.set(vec![(2, "other"), (2, "again")]);
        h.frame(&built);
        window = Some(built);
    });
    let window = window.expect("the window was built");
    assert_eq!(texts(&window), ["other"]);
    assert_eq!(
        logs.matches("two items have the same key").count(),
        1,
        "{logs}"
    );
    assert!(logs.contains("PanelWindow[DP-1]/Row.children"), "{logs}");
}

#[test]
fn a_view_reading_a_signal_does_not_rerun_the_list() {
    let mut h = Harness::new();
    let items = h.cx.signal(vec![1]);
    let other = h.cx.signal(0);
    let runs = Counter::default();
    let counted = runs.clone();
    let row = Row::new().children_keyed(
        move || {
            counted.hit();
            items.get()
        },
        |n| *n,
        move |_cx, _n| {
            // Read outside any binding: only the list's own effect could see it.
            let size = other.get();
            Icon::new("x").size(size)
        },
    );
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    assert_eq!(runs.get(), 1);
    other.set(5);
    h.frame(&window);
    assert_eq!(runs.get(), 1);
}

/// A widget of the user's own whose description reads a signal.
struct ReadsSignal(Signal<u32>);

impl Widget for ReadsSignal {
    fn into_element(self: Box<Self>) -> Element {
        let size = self.0.get();
        Box::new(Icon::new("x").size(size)).into_element()
    }
}

#[test]
fn a_nested_widget_in_a_view_adds_no_dependency_to_the_list() {
    let mut h = Harness::new();
    let items = h.cx.signal(vec![1]);
    let other = h.cx.signal(4_u32);
    let runs = Counter::default();
    let counted = runs.clone();
    let row = Row::new().children_keyed(
        move || {
            counted.hit();
            items.get()
        },
        |n| *n,
        move |_cx, _n| Button::new(ReadsSignal(other)),
    );
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    assert_eq!(runs.get(), 1);

    other.set(5);
    h.frame(&window);
    assert_eq!(runs.get(), 1);
}

#[test]
fn a_view_may_change_the_list_while_it_is_built() {
    let mut h = Harness::new();
    let items = h.cx.signal(vec![1]);
    let row = Row::new().children_keyed(
        move || items.get(),
        |n| *n,
        move |_cx, n| {
            if n.get_untracked() == 1 {
                items.update(|list| {
                    if !list.contains(&2) {
                        list.push(2);
                    }
                });
            }
            Text::new(move || n.get().to_string())
        },
    );
    let window = h.window(h.panel().child(row), 400.0, 30.0);
    assert_eq!(texts(&window), ["1", "2"]);
}
