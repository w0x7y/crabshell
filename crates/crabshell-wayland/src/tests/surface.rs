//! A window's layer-surface settings and configured size.

use crabshell_ui::{Anchors, Edge, Layer};
use smithay_client_toolkit::shell::wlr_layer::{self, Anchor};

use crate::surface::{anchor, configured_size, layer};

#[test]
fn a_configure_size_of_zero_means_the_requested_size() {
    assert_eq!(configured_size((1920, 30), (0, 30)), Some((1920, 30)));
    assert_eq!(configured_size((1920, 0), (0, 30)), Some((1920, 30)));
    assert_eq!(configured_size((0, 0), (200, 30)), Some((200, 30)));
}

#[test]
fn a_size_that_cant_be_drawn_is_refused() {
    assert_eq!(configured_size((0, 30), (0, 30)), None);
    assert_eq!(configured_size((1920, 0), (0, 0)), None);
    assert_eq!(configured_size((16_385, 30), (0, 30)), None);
    assert_eq!(configured_size((16_384, 30), (0, 30)), Some((16_384, 30)));
}

#[test]
fn anchors_and_layers_map_to_layer_shell() {
    assert_eq!(
        anchor(Edge::Top | Edge::Left | Edge::Right),
        Anchor::TOP | Anchor::LEFT | Anchor::RIGHT
    );
    assert_eq!(anchor(Anchors::from(Edge::Bottom)), Anchor::BOTTOM);
    assert_eq!(anchor(Anchors::NONE), Anchor::empty());
    assert_eq!(layer(Layer::Background), wlr_layer::Layer::Background);
    assert_eq!(layer(Layer::Bottom), wlr_layer::Layer::Bottom);
    assert_eq!(layer(Layer::Top), wlr_layer::Layer::Top);
    assert_eq!(layer(Layer::Overlay), wlr_layer::Layer::Overlay);
}

#[test]
fn a_zero_side_needs_anchors_on_both_edges_across_it() {
    use crabshell_reactive::Runtime;
    use crabshell_ui::{Cx, SurfaceConfig, Text, Window};

    use crate::surface::size_problem;

    // A top bar anchored only to the top asks for a width of 0, which
    // layer-shell answers with a protocol error that ends the connection.
    let runtime = Runtime::current();
    let cx = Cx::new(runtime.new_owner());
    let window = Window::new(
        &cx,
        super::panel("HEADLESS-1")
            .anchors(Edge::Top)
            .height(30)
            .child(Text::new("hi")),
    );
    runtime.flush();
    assert_eq!(
        size_problem(&window.surface()),
        Some("a width of 0 needs anchors on both the left and right edges")
    );
    let config = |anchors: Anchors, width, height| SurfaceConfig {
        anchors,
        width,
        height,
        exclusive_zone: 0,
        layer: Layer::Top,
    };
    assert_eq!(
        size_problem(&config(Edge::Top | Edge::Left | Edge::Right, 0, 30)),
        None
    );
    assert_eq!(size_problem(&config(Edge::Top.into(), 200, 30)), None);
    assert_eq!(
        size_problem(&config(Edge::Left | Edge::Top | Edge::Right, 0, 0)),
        Some("a height of 0 needs anchors on both the top and bottom edges")
    );
    assert_eq!(
        size_problem(&config(Edge::Left | Edge::Top | Edge::Bottom, 40, 0)),
        None
    );
    assert_eq!(
        size_problem(&config(Anchors::NONE, 0, 0)),
        Some("a width of 0 needs anchors on both the left and right edges")
    );
}

/// A shown window with a label, and frames that drew it and saw the frame
/// callback.
struct Shown {
    window: crabshell_ui::Window,
    label: crabshell_reactive::Signal<String>,
    frames: crate::surface::Frames,
    text: crabshell_ui::TextSystem,
    renderer: crabshell_render::Renderer,
    pixels: Vec<u8>,
    age: crabshell_render::BufferAge,
}

impl Shown {
    fn new() -> Shown {
        use crabshell_reactive::Runtime;
        use crabshell_ui::{Cx, Row, Size, Spacer, Text, TextSystem, Window};

        let runtime = Runtime::current();
        let cx = Cx::new(runtime.new_owner());
        let label = cx.signal(String::from("12:34"));
        let window = Window::new(
            &cx,
            super::panel("HEADLESS-1")
                .anchors(Edge::Top | Edge::Left | Edge::Right)
                .height(30)
                // In a row, the label has a box of its own; the window's one
                // child always fills the window.
                .child(
                    Row::new()
                        .child(Text::new(move || label.get()))
                        .child(Spacer::new()),
                ),
        );
        let mut text = TextSystem::new();
        window.set_size(Size::new(200.0, 30.0));
        runtime.flush();
        window.layout(&mut text);
        let mut shown = Shown {
            window,
            label,
            frames: crate::surface::Frames::new(crate::scale::Scale::Integer(1)),
            text,
            renderer: crabshell_render::Renderer::new(),
            pixels: vec![0; 200 * 30 * 4],
            age: crabshell_render::BufferAge::default(),
        };
        shown.frames.pacing.configure();
        assert!(shown.draw().is_some(), "the first frame draws");
        shown.frames.pacing.committed();
        shown.frames.pacing.frame_done();
        shown
    }

    /// Draws into the 200 by 30 buffer.
    fn draw(&mut self) -> Option<Vec<crabshell_render::PixelRect>> {
        let canvas = (self.pixels.as_mut_slice(), 200, 30);
        self.frames.draw(
            &self.window,
            &mut self.renderer,
            &mut self.text,
            canvas,
            &mut self.age,
        )
    }

    /// Changes the label and lays the window out again.
    fn relabel(&mut self, label: &str) {
        self.label.set(String::from(label));
        crabshell_reactive::Runtime::current().flush();
        self.window.layout(&mut self.text);
    }
}

#[test]
fn a_failed_draw_asks_for_another_frame() {
    let mut shown = Shown::new();
    shown.relabel("12:35");
    assert!(shown.frames.pacing.should_draw(shown.window.has_damage()));
    // The renderer refuses a buffer of the wrong size. The damage it took
    // is gone, so the frames must ask for a redraw themselves.
    let mut too_small = [0; 4];
    let drawn = shown.frames.draw(
        &shown.window,
        &mut shown.renderer,
        &mut shown.text,
        (&mut too_small, 200, 30),
        &mut shown.age,
    );
    assert!(drawn.is_none());
    assert!(!shown.window.has_damage());
    assert!(
        shown.frames.pacing.should_draw(false),
        "the next iteration draws again"
    );
    // And repaints everything, since the lost damage could be anywhere.
    assert_eq!(
        shown.draw(),
        Some(vec![crabshell_render::PixelRect::new(0, 0, 200, 30)])
    );
}

#[test]
fn a_lost_frame_asks_for_another_that_repaints_everything() {
    let mut shown = Shown::new();
    // Narrower, as a clock gets: the spacer after it grows, but draws
    // nothing, so only the label is repainted.
    shown.relabel("1:05");
    let report = shown.draw().expect("drawn");
    assert!(
        report.iter().map(|rect| rect.area()).sum::<u64>() < 200 * 30,
        "only the label: {report:?}"
    );
    // The buffer couldn't be attached: the frame never reached the screen.
    shown.frames.lost();
    assert!(shown.frames.pacing.should_draw(false));
    assert_eq!(
        shown.draw(),
        Some(vec![crabshell_render::PixelRect::new(0, 0, 200, 30)])
    );
}

#[test]
fn a_new_scale_draws_without_damage() {
    use crate::scale::Scale;

    let mut shown = Shown::new();
    assert!(!shown.frames.pacing.should_draw(false), "idle");
    shown.frames.set_scale(Scale::Integer(1));
    assert!(
        !shown.frames.pacing.should_draw(false),
        "the same scale is no change"
    );
    shown.frames.set_scale(Scale::Fractional(180));
    assert!(shown.frames.pacing.should_draw(false));
    assert_eq!(shown.frames.scale, Scale::Fractional(180));
}

#[test]
fn a_new_icon_theme_repaints_the_whole_window() {
    let mut shown = Shown::new();
    assert!(!shown.frames.pacing.should_draw(false), "idle");
    // Any icon may look different, and nothing in the window changed.
    shown.frames.repaint_whole();
    assert!(shown.frames.pacing.should_draw(false));
    assert_eq!(
        shown.draw(),
        Some(vec![crabshell_render::PixelRect::new(0, 0, 200, 30)])
    );
}
