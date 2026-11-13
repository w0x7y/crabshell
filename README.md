<p align="center"><img src="docs/site/logo.svg" width="150" alt="the crabshell logo, a pixel snail shell"></p>

# crabshell

a full rust replacement for quickshell: everything quickshell does, crabshell will do. your whole desktop shell is a rust crate, with bars, popups, notifications, launchers, lock screens, osds and the rest. the first milestone, v0.1, is a bar for sway and other wlroots compositors, a checkpoint that proves the approach works before the rest of quickshell's features follow

status: v0.1 is on crates.io, and phase 7 is done on main. bars draw, take clicks, follow monitors as they come and go, and show the clock, sway's workspaces, the volume and the tray with its menus. `crabshell run` rebuilds your bar when you save it and swaps the running one in about a second. on main, windows also take the keyboard, with text fields, input methods and the clipboard, and there are floating windows, scrolling lists, images and clipping

### ai disclosure

this project makes heavy use of ai agents. they write the code, the tests and most of the docs, and other agents review each step against [the design doc](docs/design.md). a person sets the direction, makes the design calls, reads the reviews and checks the results by hand on a real desktop

### get started

you need

~ rust 1.99 or newer\
~ sway, or another wlroots compositor with layer-shell, like labwc. only sway shows workspaces for now\
~ linux 5.3 or newer\
~ pkg-config, fontconfig and libpulse with their development files, like `pkg-config libfontconfig1-dev libpulse-dev` on debian or ubuntu\
~ pulseaudio or pipewire-pulse running, for the volume\
~ a d-bus session bus for the tray, and a notification daemon like mako or dunst for build errors

install the `crabshell` command, make a config, and run it

```sh
cargo install --locked crabshell-cli
crabshell init
crabshell run
```

`init` writes a config to `~/.config/crabshell`, a normal cargo project that depends on the `crabshell` crate. it never overwrites one that's already there. to build against a clone of this repository instead, run `crabshell init --path <clone>`. the first `crabshell run` compiles the whole toolkit, which takes a few minutes. then a 30 px bar shows up at the top of every monitor: sway's workspaces at the left, the tray, the volume and the clock at the right. stop any other bar first, or both will reserve space

to start it with sway instead, add this to your sway config. sway's `exec` may not see `~/.cargo/bin`, so give the full path, and make sure `cargo` is on the `PATH` your session starts with

```
exec ~/.cargo/bin/crabshell run
```

### edit it

with `crabshell run` running, from a terminal or from sway, change `~/.config/crabshell/src/main.rs`. when you save, it rebuilds, the running bar saves its state and exits, and the new one starts. a one-line edit takes about a second, and the bar is gone for about 20 ms. run only one `crabshell run` at a time, or you get two bars

if your edit doesn't build, the old bar stays up, the full error prints in the terminal and a notification shows its first line. if the new bar crashes, `crabshell run` starts it again, waiting longer each time, and after three quick crashes it goes back to the last build that ran for a minute, if there is one

`crabshell check` checks the config without running it, for editors and scripts

### a config

```rust
use crabshell::prelude::*;

fn main() {
  crabshell::run(|cx| cx.per_output(bar));
}

fn bar(cx: &Cx, output: &Output) -> PanelWindow {
  let count = cx.signal(0_u32);
  PanelWindow::new(output)
    .anchors(Edge::Top | Edge::Left | Edge::Right)
    .height(30)
    .child(
      Row::new()
        .padding(4, 8)
        .child(
          Button::new(Text::new(move || format!("clicked {} times", count.get())))
            .on_click(move |_| count.update(|n| *n += 1)),
        )
        .child(Spacer::new())
        .child(Text::new(output.name())),
    )
}
```

every widget property takes a value or a closure. a closure runs again when a signal it reads changes, and only what changed is redrawn. `per_output` builds windows for every monitor, now and later, and drops them when the monitor goes. `cx.persist` keeps a value across reloads. the full example, with styles and hover, is [examples/counter.rs](crates/crabshell/examples/counter.rs)

to try the examples without a config, from a clone of this repository

```sh
cargo run --release -p crabshell --example counter
cargo run --release -p crabshell --example bar
cargo run --release -p crabshell --example launcher
```

the launcher is an overlay with a search field over a scrolling list of apps and an about window: type to filter, up and down to pick, enter to launch

### docs

~ [docs/site](docs/site/index.html), getting started, writing a config, how it fits together, the roadmap and every quickshell feature with what crabshell has today. open `docs/site/index.html` in a browser\
~ [the design doc](docs/design.md), every decision and every rule settled while building each crate

### numbers

measured on headless sway, from [the design doc](docs/design.md#performance-targets)

~ the v0.1 bar, one 1920×1080 output: 18.7 MiB of memory, first frame in 10.8 ms\
~ the counter example: 15.8 MiB, zero wakeups while idle\
~ a reload, one 1280×720 output: 0.7 s to rebuild after a one-line edit, then 13 to 19 ms with no bar

for comparison, quickshell drawing the [phase 0 test bar](spike/RESULTS.md) uses 125 MiB and needs 219 ms to its first frame

### where it's going

v0.1 is the template bar plus `crabshell run`, on crates.io. phase 7 added floating windows and keyboard input. after it come animations and effects, a notifications server, media players, battery, bluetooth and networking, launchers and ipc, lock screens and greeters, then hyprland, niri and more compositors, until every quickshell feature is covered. the plan is in [the design doc](docs/design.md#beyond-v01), and [docs/site/roadmap.html](docs/site/roadmap.html) shows where each phase stands

### what's in it

~ [crabshell](crates/crabshell/), `run()`, `per_output`, `cx.floating`, `cx.persist`, the prelude and the examples\
~ [crabshell-reactive](crates/crabshell-reactive/), signals, memos, effects and owners\
~ [crabshell-ui](crates/crabshell-ui/), widgets, styles, layout, text, text editing, focus and hit testing\
~ [crabshell-render](crates/crabshell-render/), drawing on the cpu with tiny-skia, damage tracking, clipping, glyphs, icons and images\
~ [crabshell-wayland](crates/crabshell-wayland/), the event loop, layer-shell and floating windows, popups, buffers, scaling, the pointer, the keyboard, input methods, the clipboard and monitors\
~ [crabshell-services](crates/crabshell-services/), the clock, sway's workspaces, audio and the tray so far\
~ [crabshell-cli](crates/crabshell-cli/), the `crabshell` command: `init`, `check`, and `run`, which rebuilds and supervises your bar\
~ [spike](spike/) and [baseline](baseline/), the phase 0 prototype and the quickshell bar it was measured against

### tests

```sh
cargo test --workspace
```

they need `dbus-daemon` and `dbus-monitor` for the tray and notification tests, and the golden images expect noto sans as your `sans-serif`, plus dejavu sans. ci runs fmt, clippy and these tests on every push. the headless sway tests only run on your machine

the headless sway tests are skipped by default. run them from any terminal, without your session's `SWAYSOCK` so nothing can reach your sway. they refuse to start with it set

```sh
env -u SWAYSOCK cargo test -p crabshell-wayland --test headless -- --ignored --test-threads=1
env -u SWAYSOCK cargo test -p crabshell --test headless -- --ignored --test-threads=1
env -u SWAYSOCK cargo test -p crabshell-cli --test exit_check -- --ignored --nocapture
```

they need `sway`, `swaymsg`, `grim` and `labwc`, `wl-copy` and `wl-paste` for the clipboard, plus `pipewire` and `pactl` for the audio ones. each test runs its own private sway from a copy of `sway`. the copy drops swayfx's `cap_sys_nice`, so it never goes realtime and a realtime cpu limit of 0 can't kill it. the audio ones also run their own pipewire-pulse, so your volume never moves. the last one builds a config from the template and checks reloads, crashes and the fallback to the last good build. `CRABSHELL_BLESS=1` rewrites the golden images

### formatting

2 spaces everywhere. `cargo fmt --all` reads it from [rustfmt.toml](rustfmt.toml), and the python, markdown, yaml and the docs site get

```sh
ruff format --config indent-width=2 baseline/
prettier --tab-width 2 --write $(git ls-files '*.md' '*.yml' '*.html' '*.css' '*.js')
```

_license: [mit](./LICENSE-MIT) or [apache-2.0](./LICENSE-APACHE), your choice_
