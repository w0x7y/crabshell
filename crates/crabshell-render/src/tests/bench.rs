//! How long full draws of the rime port's animated windows take, at
//! 1920 by 1080 and scale 1: a dashboard opening, the click catcher with
//! its shadow, a workspace bloom and a lock screen fading in. Each scene
//! copies the styles the port uses, not its exact layout. Run it in a
//! release build:
//! `cargo test --release -p crabshell-render --lib time_the_animated_scenes -- --ignored --nocapture`.

use std::rc::Rc;

use crabshell_ui::{
  CaptureFrame, CaptureSource, Color, Column, Fit, Font, Image, PanelWindow, Rect, Region, Row,
  ScreencopyView, Shadow, Stack, Style, Text, Window,
};

use super::{Scene, write_png};

const GLASS: Color = Color::rgba(30, 30, 46, 200);
const CARD: Color = Color::rgba(255, 255, 255, 18);
const SHADOW: Color = Color::rgba(0, 0, 0, 128);

/// The fastest of a few full draws of `window` at scale 1, in ms.
fn time(scene: &mut Scene, window: &Window) -> f64 {
  scene.full(window, 1.0);
  (0..5)
    .map(|_| {
      let start = std::time::Instant::now();
      scene.full(window, 1.0);
      start.elapsed().as_secs_f64() * 1000.0
    })
    .fold(f64::INFINITY, f64::min)
}

/// The dashboard panel `progress` of the way open: a clipped rounded
/// glass growing down, its body fading and rising, and six cards each
/// fading and rising.
fn dashboard(scene: &Scene, progress: f32) -> PanelWindow {
  let rise = 14.0 * (1.0 - progress);
  let card = |order: u32| {
    let late = (progress - 0.1 * order as f32).clamp(0.0, 1.0);
    Column::new()
      .style(
        Style::new()
          .width(433.0)
          .height(160.0)
          .padding(16, 18)
          .radius(14.0)
          .background(CARD)
          .opacity(late)
          .translate(0.0, 14.0 * (1.0 - late)),
      )
      .child(Text::new("Card").style(Style::new().color(Color::rgb(220, 220, 240))))
  };
  let rows = (0..3).fold(Column::new().gap(16), |column, row| {
    column.child(
      Row::new()
        .gap(18)
        .child(card(row * 2))
        .child(card(row * 2 + 1)),
    )
  });
  let body = Column::new()
    .style(
      Style::new()
        .width(884.0)
        .left(18.0)
        .top(18.0 - rise)
        .opacity((progress * 1.6).min(1.0)),
    )
    .child(rows);
  scene.panel().child(
    Column::new()
      .style(
        Style::new()
          .width(920.0)
          .height((561.0 * progress).round())
          .radius(22.0)
          .background(GLASS)
          .clip(true),
      )
      .child(body),
  )
}

/// The full-screen catcher under a panel `progress` of the way open: a
/// clear scrim and a rectangular clip around the panel's cut-out shadow.
fn catcher(scene: &Scene, progress: f32) -> PanelWindow {
  let shadow = Row::new().style(
    Style::new()
      .left(500.0)
      .top(0.0)
      .width(920.0)
      .height((561.0 * progress).round())
      .radius(22.0)
      .shadow(Shadow::new(0.0, 10.0, 32.0, SHADOW).cut_out())
      .opacity((progress * 1.5).min(1.0)),
  );
  scene.panel().child(
    Stack::new()
      .style(Style::new().width(1920.0).height(1080.0))
      .child(Row::new().style(Style::new().background(SHADOW).opacity(0.0)))
      .child(
        Stack::new()
          .style(
            Style::new()
              .left(0.0)
              .top(44.0)
              .width(1920.0)
              .height(1036.0)
              .clip(true),
          )
          .child(shadow),
      ),
  )
}

/// The windows the bloom cuts from the screenshot, as x, y, width and
/// height.
const PIECES: [(f32, f32, f32, f32); 3] = [
  (10.0, 10.0, 940.0, 1012.0),
  (970.0, 10.0, 940.0, 500.0),
  (970.0, 522.0, 940.0, 500.0),
];

/// A workspace bloom `progress` of the way through, or its screenshot
/// frame at 0: the wallpaper with holes for the new windows, their
/// shadows, and the old windows cut from the screenshot, shrinking and
/// fading with their own shadows.
fn bloom(scene: &Scene, wallpaper: &std::path::Path, progress: f32) -> PanelWindow {
  let full = Style::new().left(0.0).top(0.0).width(1920.0).height(1032.0);
  if progress == 0.0 {
    let still = ScreencopyView::new(CaptureSource::Output(String::from("DP-1"))).style(full);
    return scene
      .panel()
      .child(Stack::new().child(Row::new().child(still)));
  }
  let holes = PIECES.iter().fold(
    Region::rect(0, 0, 1920, 1032),
    |region, &(x, y, width, height)| {
      let grow = 0.6 + 0.4 * progress;
      region.subtract(
        Region::rect(
          (x + width * (1.0 - grow) / 2.0) as i32,
          (y + height * (1.0 - grow) / 2.0) as i32,
          (width * grow) as u32,
          (height * grow) as u32,
        )
        .radius(10.0),
      )
    },
  );
  let cover = Image::new(wallpaper.to_owned())
    .width(1920)
    .height(1032)
    .fit(Fit::Cover)
    .style(full)
    .mask_region(holes);
  let look = Shadow::new(0.0, 8.0, 24.0, SHADOW).cut_out();
  let new_shadows = PIECES
    .iter()
    .fold(Stack::new(), |stack, &(x, y, width, height)| {
      let grow = 0.6 + 0.4 * progress;
      stack.child(
        Row::new().style(
          Style::new()
            .left(x + width * (1.0 - grow) / 2.0)
            .top(y + height * (1.0 - grow) / 2.0)
            .width(width * grow)
            .height(height * grow)
            .radius(10.0)
            .shadow(look)
            .opacity(progress),
        ),
      )
    });
  let olds = PIECES
    .iter()
    .fold(Stack::new(), |stack, &(x, y, width, height)| {
      let shot = ScreencopyView::new(CaptureSource::Output(String::from("DP-1")))
        .crop(Some(Rect::new(x, y, width, height)))
        .style(Style::new().width(width).height(height));
      stack.child(
        Stack::new()
          .style(
            Style::new()
              .left(x)
              .top(y)
              .width(width)
              .height(height)
              .scale(1.0 - 0.2 * progress)
              .opacity(1.0 - progress),
          )
          .child(
            Row::new().style(
              Style::new()
                .left(0.0)
                .top(0.0)
                .width(width)
                .height(height)
                .radius(10.0)
                .shadow(look),
            ),
          )
          .child(
            Row::new()
              .style(Style::new().radius(10.0).clip(true))
              .child(shot),
          ),
      )
    });
  scene.panel().child(
    Stack::new()
      .child(Row::new().child(cover))
      .child(new_shadows)
      .child(olds),
  )
}

/// The lock screen `fade` of the way in: one full-screen layer holding
/// the blurred wallpaper, a translucent scrim and the clock.
fn lock(scene: &Scene, wallpaper: &std::path::Path, fade: f32) -> PanelWindow {
  let wall = Image::new(wallpaper.to_owned())
    .width(2040)
    .height(1200)
    .fit(Fit::Cover)
    .blur(32.0)
    .saturation(0.2)
    .style(Style::new().left(-60.0).top(-60.0));
  let content = Column::new()
    .style(Style::new().width(1920.0).height(1080.0))
    .child(Row::new().style(Style::new().height(200.0)))
    .child(
      Text::new("12:34").style(
        Style::new()
          .font(Font::new("sans-serif", 160.0))
          .color(Color::rgb(255, 255, 255)),
      ),
    );
  scene.panel().child(
    Stack::new()
      .style(Style::new().width(1920.0).height(1080.0).opacity(fade))
      .child(Row::new().child(wall))
      .child(
        Row::new().style(
          Style::new()
            .width(1920.0)
            .height(1080.0)
            .background(Color::rgba(30, 30, 46, 97)),
        ),
      )
      .child(content),
  )
}

#[test]
#[ignore = "a measurement, not a check"]
fn time_the_animated_scenes() {
  let dir = std::env::temp_dir().join(format!("crabshell-bench-{}", std::process::id()));
  let wallpaper = dir.join("wallpaper.png");
  let (width, height) = (3840u32, 2160u32);
  let pixels: Vec<u8> = (0..width * height)
    .flat_map(|at| {
      let (x, y) = (at % width, at / width);
      [(x / 15) as u8, (y / 9) as u8, ((x + y) / 23) as u8, 255]
    })
    .collect();
  write_png(&wallpaper, &pixels, width, height);
  let frame = CaptureFrame {
    width: 1920,
    height: 1032,
    logical_width: 1920,
    logical_height: 1032,
    rgba: Rc::from(
      (0..1920u32 * 1032)
        .flat_map(|at| [(at % 251) as u8, (at % 241) as u8, 90, 255])
        .collect::<Vec<u8>>(),
    ),
  };

  let report = |name: &str, size: (f32, f32), build: &dyn Fn(&Scene) -> PanelWindow| {
    let mut scene = Scene::new();
    let panel = build(&scene);
    let window = scene.window(panel, size.0, size.1);
    for capture in window.captures() {
      window.show_capture(capture.id, frame.clone());
    }
    scene.frame(&window);
    println!("{name}: {:.1} ms", time(&mut scene, &window));
  };
  for progress in [0.3, 0.6, 0.9] {
    report(
      &format!("dashboard at {progress}"),
      (920.0, 595.0),
      &|scene| dashboard(scene, progress),
    );
  }
  for progress in [0.3, 0.9] {
    report(
      &format!("catcher at {progress}"),
      (1920.0, 1080.0),
      &|scene| catcher(scene, progress),
    );
  }
  for progress in [0.0, 0.3, 0.7] {
    report(
      &format!("bloom at {progress}"),
      (1920.0, 1032.0),
      &|scene| bloom(scene, &wallpaper, progress),
    );
  }
  for fade in [0.3, 0.7] {
    report(&format!("lock at {fade}"), (1920.0, 1080.0), &|scene| {
      lock(scene, &wallpaper, fade)
    });
  }
  std::fs::remove_dir_all(&dir).ok();
}
