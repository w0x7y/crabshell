//! How long draws of the rime port's animated windows take, at 1920 by
//! 1080 and scale 1: a dashboard opening, the click catcher with its
//! shadow, a workspace bloom and a lock screen fading in. Each scene copies
//! the styles the port uses, not its exact layout. A full draw of each at
//! a few points of its animation is timed, and then the animation itself,
//! frame after frame into one buffer as the platform draws it: the first
//! frame, then the median and the slowest of the rest. Run it in a release
//! build:
//! `cargo test --release -p crabshell-render --lib time_the_animated_scenes -- --ignored --nocapture`.

use std::rc::Rc;

use crabshell_reactive::Signal;
use crabshell_ui::{
  CaptureFrame, CaptureSource, Color, Column, Fit, Font, Image, ImageFilters, PanelWindow, Rect,
  Region, Row, ScreencopyView, Shadow, Stack, Style, Text, Window,
};

use super::{Buffer, Scene, write_png};
use crate::DamageHistory;

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

/// The window's animation drawn frame after frame into one buffer, with
/// `progress` stepping from 0.05 to 0.95: the first frame's time, then
/// the median and the slowest of the rest, in ms.
fn animate(scene: &mut Scene, window: &Window, progress: Signal<f32>) -> (f64, f64, f64) {
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(window, 1.0);
  let mut times: Vec<f64> = (0..16)
    .map(|step| {
      progress.set(0.05 + 0.06 * step as f32);
      scene.frame(window);
      let start = std::time::Instant::now();
      scene.draw(window, &mut history, &mut buffer);
      start.elapsed().as_secs_f64() * 1000.0
    })
    .collect();
  let first = times.remove(0);
  times.sort_by(f64::total_cmp);
  (first, times[times.len() / 2], times[times.len() - 1])
}

/// The dashboard panel `progress` of the way open: a clipped rounded
/// glass growing down, its body fading and rising, and six cards each
/// fading and rising.
fn dashboard(scene: &Scene, progress: Signal<f32>) -> PanelWindow {
  let card = |order: u32| {
    Column::new()
      .style(move || {
        let late = (progress.get() - 0.1 * order as f32).clamp(0.0, 1.0);
        Style::new()
          .width(433.0)
          .height(160.0)
          .padding(16, 18)
          .radius(14.0)
          .background(CARD)
          .opacity(late)
          .translate(0.0, 14.0 * (1.0 - late))
      })
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
    .style(move || {
      let progress = progress.get();
      Style::new()
        .width(884.0)
        .left(18.0)
        .top(18.0 - 14.0 * (1.0 - progress))
        .opacity((progress * 1.6).min(1.0))
    })
    .child(rows);
  scene.panel().child(
    Column::new()
      .style(move || {
        Style::new()
          .width(920.0)
          .height((561.0 * progress.get()).round())
          .radius(22.0)
          .background(GLASS)
          .clip(true)
      })
      .child(body),
  )
}

/// The full-screen catcher under a panel `progress` of the way open: a
/// clear scrim and a rectangular clip around the panel's cut-out shadow.
fn catcher(scene: &Scene, progress: Signal<f32>) -> PanelWindow {
  let shadow = Row::new().style(move || {
    let progress = progress.get();
    Style::new()
      .left(500.0)
      .top(0.0)
      .width(920.0)
      .height((561.0 * progress).round())
      .radius(22.0)
      .shadow(Shadow::new(0.0, 10.0, 32.0, SHADOW).cut_out())
      .opacity((progress * 1.5).min(1.0))
  });
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

/// Where a new window's hole is `progress` of the way through, growing
/// from 0.6 of piece `(x, y, width, height)` around its middle.
fn hole((x, y, width, height): (f32, f32, f32, f32), progress: f32) -> Rect {
  let grow = 0.6 + 0.4 * progress;
  Rect::new(
    x + width * (1.0 - grow) / 2.0,
    y + height * (1.0 - grow) / 2.0,
    width * grow,
    height * grow,
  )
}

/// The bloom's screenshot frame, before its pieces move.
fn still(scene: &Scene) -> PanelWindow {
  let full = Style::new().left(0.0).top(0.0).width(1920.0).height(1032.0);
  let still = ScreencopyView::new(CaptureSource::Output(String::from("DP-1"))).style(full);
  scene
    .panel()
    .child(Stack::new().child(Row::new().child(still)))
}

/// A workspace bloom `progress` of the way through: with `holes`, the
/// wallpaper with holes for the new windows and their shadows, growing;
/// with `pieces`, the old windows cut from the screenshot, shrinking and
/// fading with their own shadows.
fn bloom(
  scene: &Scene,
  wallpaper: &std::path::Path,
  progress: Signal<f32>,
  (holes, pieces): (bool, bool),
) -> PanelWindow {
  let full = Style::new().left(0.0).top(0.0).width(1920.0).height(1032.0);
  let look = Shadow::new(0.0, 8.0, 24.0, SHADOW).cut_out();
  let mut stack = Stack::new();
  if holes {
    let cover = Image::new(wallpaper.to_owned())
      .width(1920)
      .height(1032)
      .fit(Fit::Cover)
      .style(full)
      .mask_region(move || {
        let progress = progress.get();
        let all = Region::rect(0, 0, 1920, 1032);
        Some(PIECES.iter().fold(all, |region, &piece| {
          let hole = hole(piece, progress);
          region.subtract(
            Region::rect(
              hole.x as i32,
              hole.y as i32,
              hole.width as u32,
              hole.height as u32,
            )
            .radius(10.0),
          )
        }))
      });
    let new_shadows = PIECES.iter().fold(Stack::new(), |stack, &piece| {
      stack.child(Row::new().style(move || {
        let progress = progress.get();
        let hole = hole(piece, progress);
        Style::new()
          .left(hole.x)
          .top(hole.y)
          .width(hole.width)
          .height(hole.height)
          .radius(10.0)
          .shadow(look)
          .opacity(progress)
      }))
    });
    stack = stack.child(Row::new().child(cover)).child(new_shadows);
  }
  if pieces {
    let olds = PIECES
      .iter()
      .fold(Stack::new(), |stack, &(x, y, width, height)| {
        let shot = ScreencopyView::new(CaptureSource::Output(String::from("DP-1")))
          .crop(Some(Rect::new(x, y, width, height)))
          .style(Style::new().width(width).height(height));
        stack.child(
          Stack::new()
            .style(move || {
              let progress = progress.get();
              Style::new()
                .left(x)
                .top(y)
                .width(width)
                .height(height)
                .scale(1.0 - 0.2 * progress)
                .opacity(1.0 - progress)
            })
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
    stack = stack.child(olds);
  }
  scene.panel().child(stack)
}

/// The lock screen `fade` of the way in: one full-screen layer holding
/// the blurred wallpaper, a translucent scrim and the clock, which with
/// `rising` fades in and rises over the same time, as the port's does.
fn lock(
  scene: &Scene,
  wallpaper: &std::path::Path,
  fade: Signal<f32>,
  rising: bool,
) -> PanelWindow {
  let wall = Image::new(wallpaper.to_owned())
    .width(2040)
    .height(1200)
    .fit(Fit::Cover)
    .blur(32.0)
    .saturation(0.2)
    .style(Style::new().left(-60.0).top(-60.0));
  let clock = Text::new("12:34").style(move || {
    let style = Style::new()
      .font(Font::new("sans-serif", 160.0))
      .color(Color::rgb(255, 255, 255));
    let fade = fade.get();
    match rising {
      true => style.opacity(fade).translate(0.0, -40.0 * (1.0 - fade)),
      false => style,
    }
  });
  let content = Column::new()
    .style(Style::new().width(1920.0).height(1080.0))
    .child(Row::new().style(Style::new().height(200.0)))
    .child(clock);
  scene.panel().child(
    Stack::new()
      .style(move || {
        Style::new()
          .width(1920.0)
          .height(1080.0)
          .opacity(fade.get())
      })
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

  type Build<'a> = &'a dyn Fn(&Scene, Signal<f32>) -> PanelWindow;
  let open = |scene: &mut Scene, size: (f32, f32), build: Build<'_>, at: f32| {
    let progress = scene.cx.signal(at);
    let panel = build(scene, progress);
    let window = scene.window(panel, size.0, size.1);
    for capture in window.captures() {
      window.show_capture(capture.id, frame.clone());
    }
    scene.frame(&window);
    (window, progress)
  };
  let report = |name: &str, size: (f32, f32), build: Build<'_>, at: f32| {
    let mut scene = Scene::new();
    let (window, _) = open(&mut scene, size, build, at);
    println!("{name}: {:.1} ms", time(&mut scene, &window));
  };
  let frames = |name: &str, size: (f32, f32), build: Build<'_>| {
    let mut scene = Scene::new();
    let (window, progress) = open(&mut scene, size, build, 0.0);
    let (first, median, worst) = animate(&mut scene, &window, progress);
    println!(
      "{name} frames: first {first:.1} ms, then median {median:.1} ms, slowest {worst:.1} ms"
    );
  };
  let dashboard_size = (920.0, 595.0);
  let screen = (1920.0, 1080.0);
  let bloom_size = (1920.0, 1032.0);
  for progress in [0.3, 0.6, 0.9] {
    let name = format!("dashboard at {progress}");
    report(&name, dashboard_size, &dashboard, progress);
  }
  for progress in [0.3, 0.9] {
    report(
      &format!("catcher at {progress}"),
      screen,
      &catcher,
      progress,
    );
  }
  report("bloom at 0", bloom_size, &|scene, _| still(scene), 0.0);
  let whole = |scene: &Scene, progress| bloom(scene, &wallpaper, progress, (true, true));
  for progress in [0.3, 0.7] {
    report(
      &format!("bloom at {progress}"),
      bloom_size,
      &whole,
      progress,
    );
  }
  let fading = |scene: &Scene, fade| lock(scene, &wallpaper, fade, false);
  for fade in [0.3, 0.7] {
    report(&format!("lock at {fade}"), screen, &fading, fade);
  }
  frames("dashboard panel", dashboard_size, &dashboard);
  frames("catcher", screen, &catcher);
  let pieces = |scene: &Scene, progress| bloom(scene, &wallpaper, progress, (false, true));
  frames("bloom pieces", bloom_size, &pieces);
  let holes = |scene: &Scene, progress| bloom(scene, &wallpaper, progress, (true, false));
  frames("bloom holes", bloom_size, &holes);
  frames("bloom", bloom_size, &whole);
  frames("lock fade", screen, &fading);
  let rising = |scene: &Scene, fade| lock(scene, &wallpaper, fade, true);
  frames("lock fade with the clock rising", screen, &rising);
  // The bloom's first frame: a new screenshot every draw, kept again.
  let mut scene = Scene::new();
  let panel = still(&scene);
  let window = scene.window(panel, 1920.0, 1032.0);
  let id = window.captures()[0].id;
  let fastest = (0..5)
    .map(|_| {
      let rgba: Rc<[u8]> = Rc::from(frame.rgba.to_vec());
      window.show_capture(
        id,
        CaptureFrame {
          rgba,
          ..frame.clone()
        },
      );
      scene.frame(&window);
      let start = std::time::Instant::now();
      scene.full(&window, 1.0);
      start.elapsed().as_secs_f64() * 1000.0
    })
    .fold(f64::INFINITY, f64::min);
  println!("bloom with a new screenshot: {fastest:.1} ms");
  // The lock's wallpaper, as the worker makes it: read, scaled to the box
  // and blurred, at the box's size and at half of it.
  for decode_size in [None, Some((1020, 600))] {
    let filters = ImageFilters {
      saturation: 0.2,
      blur: 32.0,
      decode_size,
      ..ImageFilters::default()
    };
    let start = std::time::Instant::now();
    crate::images::filtered(&wallpaper, (2040, 1200), Fit::Cover, &filters, 1.0)
      .expect("the wallpaper");
    let took = start.elapsed().as_secs_f64() * 1000.0;
    println!("lock wallpaper made at {decode_size:?}: {took:.0} ms");
  }
  std::fs::remove_dir_all(&dir).ok();
}
