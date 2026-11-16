//! Repainting only what changed gives the same pixels as drawing the whole
//! window, in a buffer that kept up and in buffers that skipped frames.

use crabshell_reactive::Signal;
use crabshell_ui::{Align, Color, Font, Icon, Row, Stack, Style, Text, TextAlign, Window};

use super::{Buffer, Scene, fixture};
use crate::{BufferAge, Canvas, DamageHistory, PixelFormat, PixelRect};

/// What the changing window's bindings read.
#[derive(Clone, Copy)]
struct Knobs {
  red: Signal<bool>,
  label: Signal<String>,
  big: Signal<bool>,
  faded: Signal<bool>,
  border: Signal<f32>,
}

/// A window whose every kind of property can change: a paint change, text
/// that moves its neighbors, an icon's size, opacity and a border. Its
/// background is translucent, so cleared areas show if they are wrong.
fn changing_window(scene: &mut Scene) -> (Window, Knobs) {
  let knobs = Knobs {
    red: scene.cx.signal(false),
    label: scene.cx.signal(String::from("12:34")),
    big: scene.cx.signal(false),
    faded: scene.cx.signal(false),
    border: scene.cx.signal(1.0),
  };
  let Knobs {
    red,
    label,
    big,
    faded,
    border,
  } = knobs;
  let panel = scene
    .panel()
    .style(
      Style::new()
        .background(Color::rgba(30, 30, 46, 200))
        .color(Color::hex("#cdd6f4")),
    )
    .child(
      Row::new()
        .padding(4, 6)
        .gap(6)
        .align(Align::Center)
        .child(Row::new().style(move || {
          let color = if red.get() { "#f38ba8" } else { "#a6e3a1" };
          Style::new()
            .background(Color::hex(color))
            .radius(4.0)
            .padding(6, 8)
        }))
        .child(Text::new(move || label.get()))
        .child(
          Icon::new(fixture("shapes.svg").as_path()).size(move || if big.get() { 24 } else { 16 }),
        )
        .child(
          Stack::new()
            .style(move || Style::new().opacity(if faded.get() { 0.4 } else { 1.0 }))
            .child(
              Row::new().style(
                Style::new()
                  .background(Color::hex("#89b4fa"))
                  .padding(6, 10),
              ),
            )
            .child(Text::new("ab")),
        )
        .child(Row::new().style(move || {
          Style::new()
            .border(border.get(), Color::hex("#fab387"))
            .radius(3.0)
            .padding(6, 6)
        }))
        .child(Text::new("right side")),
    );
  (scene.window(panel, 260.0, 32.0), knobs)
}

/// Each step changes something.
fn steps(knobs: Knobs) -> Vec<Box<dyn Fn()>> {
  vec![
    Box::new(move || knobs.red.set(true)),
    Box::new(move || knobs.label.set(String::from("12:345678"))),
    Box::new(move || knobs.big.set(true)),
    Box::new(move || knobs.faded.set(true)),
    Box::new(move || knobs.border.set(3.0)),
    Box::new(move || knobs.label.set(String::from("9"))),
    Box::new(move || {
      knobs.big.set(false);
      knobs.red.set(false);
    }),
    Box::new(move || knobs.faded.set(false)),
  ]
}

#[test]
fn repainting_the_damage_matches_drawing_everything() {
  for scale in [1.0, 1.5, 2.0] {
    let mut scene = Scene::new();
    let (window, knobs) = changing_window(&mut scene);
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    scene.draw(&window, &mut history, &mut buffer);
    for (number, step) in steps(knobs).iter().enumerate() {
      step();
      scene.frame(&window);
      let report = scene.draw(&window, &mut history, &mut buffer);
      assert!(
        !report.is_empty(),
        "step {number} at {scale} reported nothing"
      );
      let full = scene.full(&window, scale);
      assert!(
        buffer.pixels == full.pixels,
        "step {number} at scale {scale}: the repaint differs from a full draw"
      );
    }
  }
}

#[test]
fn buffers_that_skip_frames_catch_up() {
  for scale in [1.0, 1.5] {
    let mut scene = Scene::new();
    let (window, knobs) = changing_window(&mut scene);
    let mut history = DamageHistory::new();
    let mut buffers = vec![Buffer::new(&window, scale); 3];
    // Buffer 0 draws the first frame; then the order skips around, so
    // buffers fall one, two or more frames behind.
    scene.draw(&window, &mut history, &mut buffers[0]);
    let order = [1, 2, 0, 0, 1, 2, 2, 0];
    for ((number, step), &index) in steps(knobs).iter().enumerate().zip(&order) {
      step();
      scene.frame(&window);
      scene.draw(&window, &mut history, &mut buffers[index]);
      let full = scene.full(&window, scale);
      assert!(
        buffers[index].pixels == full.pixels,
        "step {number} into buffer {index} at scale {scale} differs from a full draw"
      );
    }
  }
}

#[test]
fn the_report_is_what_changed_since_the_last_frame() {
  let mut scene = Scene::new();
  let (window, knobs) = changing_window(&mut scene);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  let first = scene.draw(&window, &mut history, &mut buffer);
  assert_eq!(first, vec![PixelRect::new(0, 0, 260, 32)]);

  knobs.red.set(true);
  scene.frame(&window);
  let report = scene.draw(&window, &mut history, &mut buffer);
  // The 16 by 12 box at 6, 10, plus a pixel around it.
  assert_eq!(report, vec![PixelRect::new(5, 9, 18, 14)]);

  // Nothing changed: nothing to repaint or report.
  assert!(scene.draw(&window, &mut history, &mut buffer).is_empty());
}

#[test]
fn argb_buffers_get_red_and_blue_swapped() {
  let mut scene = Scene::new();
  let (window, _) = changing_window(&mut scene);
  let rgba = scene.full(&window, 1.5);
  let mut bgra = Buffer::new(&window, 1.5);
  scene
    .renderer
    .draw(
      &window.view(),
      &[],
      &mut scene.text,
      &mut DamageHistory::new(),
      Canvas {
        pixels: &mut bgra.pixels,
        width: bgra.width,
        height: bgra.height,
        scale: 1.5,
        format: PixelFormat::Argb8888,
        age: &mut bgra.age,
      },
    )
    .expect("the canvas fits");
  let swapped: Vec<u8> = bgra
    .pixels
    .as_chunks::<4>()
    .0
    .iter()
    .flat_map(|&[b, g, r, a]| [r, g, b, a])
    .collect();
  assert!(swapped == rgba.pixels);
  assert!(bgra.pixels != rgba.pixels, "the scene has colors to swap");
}

#[test]
fn text_taller_than_its_box_is_drawn_whole() {
  let mut scene = Scene::new();
  // 9 pixels of padding in a 30-pixel window leave 12 for a 16-pixel line.
  let panel = scene
    .panel()
    .style(
      Style::new()
        .background(Color::rgb(0, 0, 0))
        .color(Color::rgb(255, 255, 255)),
    )
    .child(Row::new().padding(9, 4).child(Text::new("Hgjy")));
  let window = scene.window(panel, 100.0, 30.0);
  let buffer = scene.full(&window, 1.0);
  let ink = |rows: std::ops::Range<u32>| {
    rows
      .flat_map(|y| (0..100).map(move |x| (x, y)))
      .any(|(x, y)| buffer.pixel(x, y)[0] > 64)
  };
  // The descenders reach below the box, which ends at row 21.
  assert!(ink(21..30), "no ink below the text's box");
  assert!(ink(9..21));
}

#[test]
fn a_canvas_of_the_wrong_size_draws_nothing() {
  let mut scene = Scene::new();
  let (window, _) = changing_window(&mut scene);
  let mut pixels = vec![7; 100];
  let mut age = BufferAge::default();
  let mut history = DamageHistory::new();
  let result = scene.renderer.draw(
    &window.view(),
    &[],
    &mut scene.text,
    &mut history,
    Canvas {
      pixels: &mut pixels,
      width: 260,
      height: 32,
      scale: 1.0,
      format: PixelFormat::Abgr8888,
      age: &mut age,
    },
  );
  let error = result.expect_err("a canvas of the wrong size is an error");
  assert!(error.to_string().contains("260x32"), "{error}");
  assert!(pixels.iter().all(|&byte| byte == 7));
  // The failed draw is no frame: the next one, on a good buffer, is the
  // history's first and repaints everything.
  let mut buffer = Buffer::new(&window, 1.0);
  let report = scene.draw(&window, &mut history, &mut buffer);
  assert_eq!(report, vec![PixelRect::new(0, 0, 260, 32)]);
}

/// Pixels that differ by more than 2/255, and their bounding box.
fn stale(drawn: &Buffer, full: &Buffer) -> (usize, Option<(u32, u32, u32, u32)>) {
  let mut count = 0;
  let mut area: Option<(u32, u32, u32, u32)> = None;
  for y in 0..drawn.height {
    for x in 0..drawn.width {
      let (a, b) = (drawn.pixel(x, y), full.pixel(x, y));
      if a.iter().zip(b).any(|(a, b)| a.abs_diff(b) > 2) {
        count += 1;
        area = Some(match area {
          None => (x, y, x, y),
          Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        });
      }
    }
  }
  (count, area)
}

#[test]
fn an_icon_bigger_than_its_box_leaves_nothing_behind() {
  use crabshell_ui::IconSource;

  for scale in [1.0, 1.25, 1.5, 2.0] {
    let mut scene = Scene::new();
    let first = scene.cx.signal(true);
    let (svg, png) = (fixture("shapes.svg"), fixture("disc.png"));
    // 10 pixels of padding in a 36-pixel bar leave 16 for a 24-pixel
    // icon, which overflows its box.
    let panel = scene
      .panel()
      .style(Style::new().background(Color::rgb(0, 0, 0)))
      .child(
        Row::new().padding(10, 4).child(
          Icon::new(move || {
            IconSource::File(if first.get() {
              svg.clone()
            } else {
              png.clone()
            })
          })
          .size(24),
        ),
      );
    let window = scene.window(panel, 60.0, 36.0);
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    scene.draw(&window, &mut history, &mut buffer);
    first.set(false);
    scene.frame(&window);
    scene.draw(&window, &mut history, &mut buffer);
    let (count, area) = stale(&buffer, &scene.full(&window, scale));
    assert_eq!(count, 0, "at {scale}: stale pixels in {area:?}");
  }
}

#[test]
fn glyph_ink_outside_the_line_leaves_nothing_behind() {
  for (from, to) in [("\u{1ED6}", "O"), ("\u{1EB6}", "A"), ("\u{303}x", "x")] {
    for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
      let mut scene = Scene::new();
      let label = scene.cx.signal(String::from(from));
      let panel = scene
        .panel()
        .style(
          Style::new()
            .background(Color::rgb(0, 0, 0))
            .color(Color::rgb(255, 255, 255)),
        )
        .child(
          Row::new()
            .padding(8, 8)
            .align(Align::Center)
            .child(Text::new(move || label.get())),
        );
      let window = scene.window(panel, 60.0, 40.0);
      let mut history = DamageHistory::new();
      let mut buffer = Buffer::new(&window, scale);
      scene.draw(&window, &mut history, &mut buffer);
      label.set(String::from(to));
      scene.frame(&window);
      scene.draw(&window, &mut history, &mut buffer);
      let (count, area) = stale(&buffer, &scene.full(&window, scale));
      assert_eq!(
        count, 0,
        "{from:?} to {to:?} at {scale}: stale pixels in {area:?}"
      );
    }
  }
}

#[test]
fn squeezed_aligned_and_decorated_text_paints_inside_its_bounds() {
  let styles = [
    Style::new().line_height(0.05),
    Style::new().line_height(0.05).text_align(TextAlign::End),
    Style::new()
      .underline(true)
      .strikethrough(true)
      .line_height(0.3),
    Style::new().text_align(TextAlign::Center).underline(true),
  ];
  for style in styles {
    for scale in [1.0, 1.5, 2.0] {
      let mut scene = Scene::new();
      let panel = scene
        .panel()
        .style(Style::new().color(Color::rgb(255, 255, 255)))
        .child(
          Row::new().padding(30, 30).align(Align::Center).child(
            Text::new("Agjy\nqÅ").max_lines(2).style(
              style
                .width(120.0)
                .font(Font::new("sans-serif", 20.0).weight(700)),
            ),
          ),
        );
      let window = scene.window(panel, 200.0, 120.0);
      let buffer = scene.full(&window, scale);
      let view = window.view();
      let bounds = super::node_at(view.root(), &[0, 0]).bounds();
      let (left, top) = ((bounds.x * scale).floor(), (bounds.y * scale).floor());
      let (right, bottom) = (
        (bounds.right() * scale).ceil(),
        (bounds.bottom() * scale).ceil(),
      );
      for y in 0..buffer.height {
        for x in 0..buffer.width {
          if buffer.pixel(x, y)[3] == 0 {
            continue;
          }
          let (x, y) = (x as f32, y as f32);
          assert!(
            x >= left && x < right && y >= top && y < bottom,
            "{style:?} at {scale}: ({x}, {y}) is outside {bounds:?}"
          );
        }
      }
    }
  }
}

/// A small deterministic random number generator.
struct Random(u64);

impl Random {
  fn below(&mut self, n: u64) -> u64 {
    self.0 = self
      .0
      .wrapping_mul(6_364_136_223_846_793_005)
      .wrapping_add(1_442_695_040_888_963_407);
    (self.0 >> 33) % n
  }
}

#[test]
fn random_changes_and_buffer_orders_never_leave_stale_pixels() {
  let labels = ["12:34", "9", "12:345678", "a much longer label here", ""];
  let scales = [1.0_f32, 1.25, 1.5, 1.75, 2.0];
  for seed in 0..12_u64 {
    for count in [2_usize, 3] {
      let mut random = Random(seed * 7 + count as u64);
      let mut scene = Scene::new();
      let (window, knobs) = changing_window(&mut scene);
      let mut scale = scales[random.below(5) as usize];
      let mut history = DamageHistory::new();
      let mut buffers = vec![Buffer::new(&window, scale); count];
      for step in 0..60 {
        match random.below(8) {
          0 => knobs.red.set(random.below(2) == 0),
          1 => knobs
            .label
            .set(String::from(labels[random.below(5) as usize])),
          2 => knobs.big.set(random.below(2) == 0),
          3 => knobs.faded.set(random.below(2) == 0),
          4 => knobs
            .border
            .set([0.0, 0.5, 1.0, 3.0][random.below(4) as usize]),
          5 if random.below(6) == 0 => {
            scale = scales[random.below(5) as usize];
            buffers = vec![Buffer::new(&window, scale); count];
          }
          6 if random.below(6) == 0 => {
            let width = [200.0, 260.0, 333.0][random.below(3) as usize];
            window.set_size(crabshell_ui::Size::new(width, 32.0));
            scene.frame(&window);
            buffers = vec![Buffer::new(&window, scale); count];
          }
          _ => {}
        }
        scene.frame(&window);
        // Buffers come back in any order; the first one often, so
        // the others fall far behind.
        let index = if random.below(4) == 0 {
          0
        } else {
          random.below(count as u64) as usize
        };
        scene.draw(&window, &mut history, &mut buffers[index]);
        let (stale, area) = stale(&buffers[index], &scene.full(&window, scale));
        assert_eq!(
          stale, 0,
          "seed {seed}, {count} buffers, step {step}, scale {scale}: stale pixels in {area:?}"
        );
      }
    }
  }
}

#[test]
fn argb_repaints_match_a_full_argb_draw() {
  for scale in [1.0_f32, 1.25, 1.75] {
    let mut scene = Scene::new();
    let (window, knobs) = changing_window(&mut scene);
    let draw = |scene: &mut Scene,
                history: &mut DamageHistory,
                buffer: &mut Buffer,
                damage: &[crabshell_ui::Rect]| {
      scene
        .renderer
        .draw(
          &window.view(),
          damage,
          &mut scene.text,
          history,
          Canvas {
            pixels: &mut buffer.pixels,
            width: buffer.width,
            height: buffer.height,
            scale,
            format: PixelFormat::Argb8888,
            age: &mut buffer.age,
          },
        )
        .expect("the canvas fits");
    };
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    draw(&mut scene, &mut history, &mut buffer, &window.take_damage());
    knobs.red.set(true);
    knobs.border.set(3.0);
    scene.frame(&window);
    draw(&mut scene, &mut history, &mut buffer, &window.take_damage());
    let mut full = Buffer::new(&window, scale);
    draw(&mut scene, &mut DamageHistory::new(), &mut full, &[]);
    assert!(buffer.pixels == full.pixels, "at {scale}");
  }
}

/// Draws `damage` into `buffer`, which belongs to `history`.
pub(super) fn repaint(
  scene: &mut Scene,
  window: &Window,
  damage: &[crabshell_ui::Rect],
  history: &mut DamageHistory,
  buffer: &mut Buffer,
) {
  scene
    .renderer
    .draw(
      &window.view(),
      damage,
      &mut scene.text,
      history,
      Canvas {
        pixels: &mut buffer.pixels,
        width: buffer.width,
        height: buffer.height,
        scale: buffer.scale,
        format: PixelFormat::Abgr8888,
        age: &mut buffer.age,
      },
    )
    .expect("the test's canvas fits");
}

#[test]
fn a_repaint_whose_edge_cuts_a_rounded_corner_matches_drawing_everything() {
  let mut scene = Scene::new();
  let panel = scene
    .panel()
    .style(Style::new().background(Color::rgb(30, 30, 46)))
    .child(
      Row::new().padding(5, 10).align(Align::Start).child(
        Row::new().style(
          Style::new()
            .background(Color::hex("#89b4fa"))
            .border(1.0, Color::hex("#fab387"))
            .radius(6.0)
            .padding(10, 20),
        ),
      ),
    );
  let window = scene.window(panel, 100.0, 40.0);
  for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
    let full = scene.full(&window, scale);
    // The box's left corners span x 10 to 16; sweep the damage's right
    // edge through them.
    for right in 11..=16 {
      let mut history = DamageHistory::new();
      let mut buffer = Buffer::new(&window, scale);
      repaint(&mut scene, &window, &[], &mut history, &mut buffer);
      let damage = crabshell_ui::Rect::new(0.0, 0.0, right as f32, 40.0);
      repaint(&mut scene, &window, &[damage], &mut history, &mut buffer);
      assert!(
        buffer.pixels == full.pixels,
        "a repaint to x {right} at scale {scale} differs from a full draw"
      );
    }
  }
}

#[test]
fn turning_a_gradient_repaints_like_drawing_everything() {
  use crabshell_ui::Gradient;

  let mut scene = Scene::new();
  let angle = scene.cx.signal(0.0_f32);
  let panel = scene
    .panel()
    .style(Style::new().background(Color::rgb(30, 30, 46)))
    .child(
      Row::new()
        .padding(5, 50)
        .align(Align::Start)
        .child(Row::new().style(move || {
          Style::new()
            .gradient(
              Gradient::linear(angle.get(), Color::hex("#f38ba8"), Color::hex("#89b4fa"))
                .stop(0.5, Color::rgba(0, 0, 0, 0)),
            )
            .radius(8.0)
            .padding(12, 40)
        })),
    );
  let window = scene.window(panel, 200.0, 40.0);
  for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    repaint(&mut scene, &window, &[], &mut history, &mut buffer);
    angle.update(|angle| *angle += 70.0);
    scene.frame(&window);
    let damage = window.take_damage();
    assert!(!damage.is_empty(), "a gradient change repaints");
    repaint(&mut scene, &window, &damage, &mut history, &mut buffer);
    let full = scene.full(&window, scale);
    assert!(buffer.pixels == full.pixels, "at {scale}");
  }
}

#[test]
fn moving_a_shadow_repaints_like_drawing_everything() {
  // Blurred, and sharp, which a cut out draws another way.
  for blur in [2.0, 0.0] {
    shadow_repaints(blur);
  }
}

fn shadow_repaints(blur: f32) {
  use crabshell_ui::Shadow;

  let mut scene = Scene::new();
  let lift = scene.cx.signal(0.0_f32);
  let panel = scene
    .panel()
    .style(Style::new().background(Color::rgb(30, 30, 46)))
    .child(
      Row::new()
        .padding(20, 40)
        .align(Align::Start)
        .child(Row::new().style(move || {
          Style::new()
            .background(Color::hex("#89b4fa80"))
            .radius(6.0)
            .shadow({
              let shadow = Shadow::new(
                lift.get(),
                lift.get(),
                blur + blur * lift.get(),
                Color::rgba(0, 0, 0, 160),
              );
              // Cut out once lifted, so a repaint cuts it as a full draw does.
              if lift.get() > 0.0 {
                shadow.cut_out()
              } else {
                shadow
              }
            })
            .padding(10, 30)
        })),
    );
  let window = scene.window(panel, 200.0, 80.0);
  for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    repaint(&mut scene, &window, &[], &mut history, &mut buffer);
    lift.update(|lift| *lift = 9.0 - *lift);
    scene.frame(&window);
    let damage = window.take_damage();
    repaint(&mut scene, &window, &damage, &mut history, &mut buffer);
    let full = scene.full(&window, scale);
    assert!(buffer.pixels == full.pixels, "blur {blur} at {scale}");
  }
}

#[test]
fn repainting_inside_a_clip_matches_drawing_everything() {
  let mut scene = Scene::new();
  let label = scene.cx.signal(String::from("short"));
  let tall = scene.cx.signal(false);
  let clipped = scene.cx.signal(true);
  // The clipping row is inset from the window, so what it cuts off would
  // show around it, and draws nothing itself, so only its children
  // report damage.
  let panel = scene
    .panel()
    .style(Style::new().background(Color::rgb(30, 30, 46)))
    .child(
      Row::new().padding(8, 20).child(
        Row::new()
          .padding(3, 3)
          .gap(4)
          .align(Align::Start)
          .style(move || Style::new().radius(10.0).clip(clipped.get()))
          .child(Row::new().style(move || {
            Style::new()
              .background(Color::hex("#f38ba8"))
              .padding(if tall.get() { 30 } else { 4 }, 12)
          }))
          .child(Text::new(move || label.get())),
      ),
    );
  let window = scene.window(panel, 160.0, 40.0);
  let changes: [Box<dyn Fn()>; 5] = [
    Box::new(move || label.set(String::from("a label far too long to fit here"))),
    Box::new(move || tall.set(true)),
    Box::new(move || clipped.set(false)),
    Box::new(move || clipped.set(true)),
    Box::new(move || {
      label.set(String::from("short"));
      tall.set(false);
    }),
  ];
  for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    repaint(&mut scene, &window, &[], &mut history, &mut buffer);
    window.take_damage();
    for change in &changes {
      change();
      scene.frame(&window);
      let damage = window.take_damage();
      repaint(&mut scene, &window, &damage, &mut history, &mut buffer);
      let full = scene.full(&window, scale);
      assert!(
        buffer.pixels == full.pixels,
        "a repaint inside the clip at scale {scale} differs from a full draw"
      );
    }
  }
}

#[test]
fn scrolling_repaints_match_drawing_everything() {
  let mut scene = Scene::new();
  let offset = scene.cx.signal(0.0);
  let column = ["one", "two", "three", "four", "five", "six"]
    .into_iter()
    .fold(crabshell_ui::Column::new(), |column, label| {
      column.child(
        Row::new()
          .style(Style::new().background(Color::hex("#45475a")).padding(4, 6))
          .child(Text::new(label)),
      )
    });
  let panel = scene
    .panel()
    .style(
      Style::new()
        .background(Color::rgb(30, 30, 46))
        .color(Color::hex("#cdd6f4")),
    )
    .child(
      Row::new().padding(6, 10).align(Align::Start).child(
        crabshell_ui::ScrollView::new(column)
          .offset(offset)
          .style(Style::new().height(40.0).radius(6.0)),
      ),
    );
  let window = scene.window(panel, 120.0, 60.0);
  // The view ends at y 46; the rows below it are cut off, so the window's
  // background shows there.
  let full = scene.full(&window, 1.0);
  let at = ((52 * full.width + 20) * 4) as usize;
  assert_eq!(
    &full.pixels[at..at + 3],
    &[30, 30, 46],
    "rows below the view"
  );
  for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
    offset.set(0.0);
    scene.frame(&window);
    window.take_damage();
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    repaint(&mut scene, &window, &[], &mut history, &mut buffer);
    for step in [7.5, 30.0, 1000.0, 3.0] {
      offset.set(step);
      scene.frame(&window);
      let damage = window.take_damage();
      repaint(&mut scene, &window, &damage, &mut history, &mut buffer);
      assert!(
        buffer.pixels == scene.full(&window, scale).pixels,
        "scrolled to {step} at scale {scale}, a repaint differs from a full draw"
      );
    }
  }
}

#[test]
fn a_scroll_bar_repaints_where_it_moves_and_where_it_was_once_it_goes() {
  let mut scene = Scene::new();
  let offset = scene.cx.signal(0.0);
  let tall = scene.cx.signal(true);
  let column = crabshell_ui::Column::new()
    .child(Row::new().style(Style::new().height(20.0).background(Color::hex("#45475a"))))
    .child(Row::new().style(move || Style::new().height(if tall.get() { 80.0 } else { 0.0 })));
  let bar = Style::new()
    .width(4.0)
    .radius(2.0)
    .background(Color::hex("#f9e2af"));
  let panel = scene
    .panel()
    .style(Style::new().background(Color::rgb(30, 30, 46)))
    .child(
      Row::new().padding(5, 5).align(Align::Start).child(
        crabshell_ui::ScrollView::new(column)
          .offset(offset)
          .scroll_bar(bar)
          .style(Style::new().width(30.0).height(40.0)),
      ),
    );
  let window = scene.window(panel, 60.0, 50.0);
  for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
    offset.set(0.0);
    tall.set(true);
    scene.frame(&window);
    window.take_damage();
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    repaint(&mut scene, &window, &[], &mut history, &mut buffer);
    for (step, keep) in [(13.5, true), (60.0, true), (60.0, false)] {
      offset.set(step);
      tall.set(keep);
      scene.frame(&window);
      let damage = window.take_damage();
      repaint(&mut scene, &window, &damage, &mut history, &mut buffer);
      assert!(
        buffer.pixels == scene.full(&window, scale).pixels,
        "at {step}, tall {keep}, scale {scale}, a repaint differs from a full draw"
      );
    }
  }
}
