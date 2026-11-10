//! Windows: building, widget paths, layer-surface settings and freeing.

use crabshell_reactive::Runtime;

use super::{Harness, capture_logs, paths};
use crate::{
    Anchors, Button, Cx, Edge, Icon, Layer, Output, PanelWindow, Row, Spacer, Stack, Style, Text,
    Window,
};

#[test]
fn every_widget_gets_a_path_for_log_lines() {
    let mut h = Harness::new();
    let row = Row::new()
        .child(Text::new("a"))
        .child(Spacer::new())
        .child(Button::new(Stack::new().child(Icon::new("x"))));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    assert_eq!(
        paths(&window),
        [
            "PanelWindow[DP-1]",
            "PanelWindow[DP-1]/Row",
            "PanelWindow[DP-1]/Row/Text[0]",
            "PanelWindow[DP-1]/Row/Spacer[1]",
            "PanelWindow[DP-1]/Row/Button[2]",
            "PanelWindow[DP-1]/Row/Button[2]/Stack",
            "PanelWindow[DP-1]/Row/Button[2]/Stack/Icon[0]",
        ]
    );
}

#[test]
fn a_panicking_binding_is_logged_with_its_widget_path() {
    let mut h = Harness::new();
    let fail = h.cx.signal(false);
    let label = h.cx.signal(String::from("fine"));
    let row = Row::new()
        .child(Text::new(move || label.get()))
        .child(Button::new(Text::new(move || {
            assert!(!fail.get(), "binding failure");
            String::from("ok")
        })));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    let logs = capture_logs(|| {
        fail.set(true);
        h.frame(&window);
    });
    assert!(
        logs.contains("effect PanelWindow[DP-1]/Row/Button[1]/Text.text panicked and is disabled"),
        "{logs}"
    );

    // The rest of the window keeps working; the failed binding keeps its
    // last value and never runs again.
    label.set(String::from("still fine"));
    fail.set(false);
    h.frame(&window);
    let view = window.view();
    let text = |path: &[usize]| match super::node_at(view.root(), path).content() {
        crate::Content::Text(text) => text.content().to_owned(),
        _ => panic!("not text"),
    };
    assert_eq!(text(&[0, 0]), "still fine");
    assert_eq!(text(&[0, 1, 0]), "ok");
}

#[test]
fn dropping_a_window_frees_everything_it_created() {
    let mut h = Harness::new();
    let count = h.cx.signal(0);
    let items = h.cx.signal(vec![1, 2]);
    let before = h.runtime.node_count();
    let row = Row::new()
        .gap(move || count.get())
        .child(
            Button::new(Text::new(move || count.get().to_string()))
                .style(move || Style::new().radius(count.get() as f32))
                .on_click(move |_| count.update(|n| *n += 1)),
        )
        .children_keyed(
            move || items.get(),
            |n| *n,
            |cx, n| {
                let local = cx.signal(1);
                Text::new(move || format!("{} {}", n.get(), local.get()))
            },
        );
    let window = h.window(
        h.panel().height(move || 30 + count.get()).child(row),
        200.0,
        30.0,
    );
    assert!(h.runtime.node_count() > before);

    drop(window);
    assert_eq!(h.runtime.node_count(), before);
    // The signals the window read are untouched and still work.
    count.set(5);
    h.runtime.flush();
    assert_eq!(count.get(), 5);
}

#[test]
fn surface_settings_have_layer_shell_defaults() {
    let mut h = Harness::new();
    let window = h.window(h.panel(), 200.0, 30.0);
    let surface = window.surface();
    assert_eq!(surface.anchors, Anchors::NONE);
    assert_eq!((surface.width, surface.height), (0, 0));
    assert_eq!(surface.exclusive_zone, 0);
    assert_eq!(surface.layer, Layer::Top);
    assert_eq!(window.output(), Output::new("DP-1"));
}

#[test]
fn the_exclusive_zone_defaults_to_the_bar_thickness() {
    let surface = |panel: PanelWindow| {
        let mut h = Harness::new();
        h.window(panel, 200.0, 30.0).surface()
    };
    let output = Output::new("DP-1");
    let top = PanelWindow::new(&output)
        .anchors(Edge::Top | Edge::Left | Edge::Right)
        .height(30);
    assert_eq!(surface(top).exclusive_zone, 30);
    let left = PanelWindow::new(&output)
        .anchors(Edge::Left)
        .width(40)
        .height(500);
    assert_eq!(surface(left).exclusive_zone, 40);
    let corner = PanelWindow::new(&output)
        .anchors(Edge::Top | Edge::Left)
        .height(30);
    assert_eq!(surface(corner).exclusive_zone, 0);
    let explicit = PanelWindow::new(&output)
        .anchors(Edge::Bottom)
        .height(30)
        .exclusive_zone(-1)
        .layer(Layer::Overlay);
    let explicit = surface(explicit);
    assert_eq!(explicit.exclusive_zone, -1);
    assert_eq!(explicit.layer, Layer::Overlay);
}

#[test]
fn anchors_name_the_edge_a_bar_sits_on() {
    assert_eq!(Anchors::from(Edge::Top).edge(), Some(Edge::Top));
    assert_eq!(
        (Edge::Bottom | Edge::Left | Edge::Right).edge(),
        Some(Edge::Bottom)
    );
    assert_eq!(
        (Edge::Top | Edge::Bottom | Edge::Left).edge(),
        Some(Edge::Left)
    );
    assert_eq!(
        (Edge::Top | Edge::Bottom | Edge::Right).edge(),
        Some(Edge::Right)
    );
    assert_eq!((Edge::Top | Edge::Bottom).edge(), None);
    assert_eq!(
        (Edge::Top | Edge::Bottom | Edge::Left | Edge::Right).edge(),
        None
    );
    assert_eq!(Anchors::NONE.edge(), None);
    assert!((Edge::Top | Edge::Left).contains(Edge::Left));
    assert!(!(Edge::Top | Edge::Left).contains(Edge::Right));
}

#[test]
fn surface_bindings_report_a_change() {
    let h = Harness::new();
    let tall = h.cx.signal(false);
    let panel = h
        .panel()
        .anchors(Edge::Top)
        .height(move || if tall.get() { 40 } else { 30 });
    let window = Window::new(&h.cx, panel);
    h.runtime.flush();
    assert!(window.take_surface_changed());
    assert!(!window.take_surface_changed());
    assert_eq!(window.surface().height, 30);

    tall.set(true);
    h.runtime.flush();
    assert!(window.take_surface_changed());
    assert_eq!(window.surface().height, 40);
    assert_eq!(window.surface().exclusive_zone, 40);

    // The same value again is no change.
    tall.set(true);
    h.runtime.flush();
    assert!(!window.take_surface_changed());
}

#[test]
fn bindings_first_run_at_the_next_flush() {
    let h = Harness::new();
    let window = Window::new(
        &h.cx,
        h.panel().child(Text::new(move || String::from("bound"))),
    );
    let text = |window: &Window| match super::node_at(window.view().root(), &[0]).content() {
        crate::Content::Text(text) => text.content().to_owned(),
        _ => panic!("not text"),
    };
    assert_eq!(text(&window), "");
    h.runtime.flush();
    assert_eq!(text(&window), "bound");
}

/// A widget of the user's own that runs the reactive flush while it is
/// described.
struct Flushes;

impl crate::Widget for Flushes {
    fn into_element(self: Box<Self>) -> crate::Element {
        crabshell_reactive::Runtime::current().flush();
        Box::new(Spacer::new()).into_element()
    }
}

#[test]
fn descriptions_run_before_the_window_is_built() {
    let mut h = Harness::new();
    let label = h.cx.signal(String::from("x"));
    // The text's binding is queued by the time `Flushes` is described only
    // if descriptions run while the tree is being built.
    let row = Row::new()
        .child(Text::new(move || label.get()))
        .child(Button::new(Flushes));
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    // Run during the build, the binding would find the tree borrowed, panic
    // and be disabled.
    let view = window.view();
    let crate::Content::Text(text) = super::node_at(view.root(), &[0, 0]).content() else {
        panic!("a text node has text content");
    };
    assert_eq!(text.content(), "x");
}

#[test]
fn an_icon_name_can_be_a_string() {
    let mut h = Harness::new();
    let name = String::from("audio-volume-high");
    let window = h.window(h.panel().child(Icon::new(name)), 100.0, 30.0);
    let view = window.view();
    let crate::Content::Icon(icon) = super::node_at(view.root(), &[0]).content() else {
        panic!("an icon node has icon content");
    };
    assert_eq!(
        icon.source,
        &crate::IconSource::Name(String::from("audio-volume-high"))
    );
}

#[test]
fn a_child_context_belongs_to_its_parent() {
    let root = Cx::new(Runtime::current().new_owner());
    let parent = root.child();
    let child = parent.child().named("child");
    let value = child.signal(1);
    parent.owner().dispose();
    assert_eq!(value.try_get(), None);
}

/// A widget of the user's own whose description reads a signal.
struct ReadsSignal(crabshell_reactive::Signal<u32>);

impl crate::Widget for ReadsSignal {
    fn into_element(self: Box<Self>) -> crate::Element {
        Box::new(Icon::new("x").size(self.0.get())).into_element()
    }
}

#[test]
fn a_window_built_in_an_effect_adds_no_dependency_to_it() {
    let h = Harness::new();
    let size = h.cx.signal(4_u32);
    let runs = super::Counter::default();
    let counted = runs.clone();
    let cx = h.cx.clone();
    let windows = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let kept = std::rc::Rc::clone(&windows);
    h.cx.effect(move || {
        counted.hit();
        let panel = PanelWindow::new(&Output::new("DP-1")).child(ReadsSignal(size));
        kept.borrow_mut().push(Window::new(&cx, panel));
    });
    h.runtime.flush();
    assert_eq!(runs.get(), 1);
    size.set(5);
    h.runtime.flush();
    assert_eq!(runs.get(), 1);
}
