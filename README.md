<p align="center"><img src="docs/site/logo.svg" width="150" alt="the crabshell logo, a pixel snail shell"></p>

# crabshell

a rust replacement for quickshell: your whole desktop shell is a rust crate, and everything quickshell does, crabshell will do. v0.1, a bar for sway and other wlroots compositors, is on crates.io. main also has floating windows and keyboard input, animations and blur, desktop services like bluetooth, networks, media players and notifications, launchers, a lock screen, greeter and polkit agent, and hyprland's workspaces, monitors and windows. a check against every quickshell type comes next. [the roadmap](docs/site/roadmap.html) shows each phase, and [the features page](docs/site/features.html) goes through every quickshell type

### ai disclosure

ai agents write the code, the tests and most of the docs, and other agents review each step against [the design doc](docs/design.md). a person sets the direction, makes the design calls, reads the reviews and checks the results by hand on a real desktop

### get started

you need

~ rust 1.99 or newer\
~ sway, hyprland or another compositor with layer-shell, like labwc. the template bar shows sway's workspaces, and the hyprland example hyprland's\
~ linux 5.3 or newer\
~ pkg-config, and fontconfig, libpulse, libxkbcommon and libpam with their development files: `pkg-config libfontconfig1-dev libpulse-dev libxkbcommon-dev libpam0g-dev` on debian or ubuntu\
~ pulseaudio or pipewire-pulse for the volume, a d-bus session bus for the tray, and a notification daemon like mako for build errors

```sh
cargo install --locked crabshell-cli
crabshell init
crabshell run
```

`init` writes a cargo project to `~/.config/crabshell` and never overwrites one. `crabshell init --path <clone>` builds against a clone of this repository instead. the first run compiles the toolkit, which takes a few minutes, then a bar shows up at the top of every monitor with sway's workspaces, the tray, the volume and the clock. stop any other bar first

to start it with sway, add `exec ~/.cargo/bin/crabshell run` to your sway config, with `cargo` on the `PATH` your session starts with

### edit it

change `~/.config/crabshell/src/main.rs` while `crabshell run` runs. each save rebuilds it and swaps the bar in about a second. if the edit doesn't build, the old bar stays and the error shows in the terminal and a notification. a crashing bar restarts, and after three quick crashes `crabshell run` goes back to the last build that worked. `crabshell check` checks a config without running it

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

every widget property takes a value or a closure. a closure runs again when a signal it reads changes, and only what changed is redrawn. `per_output` builds windows for every monitor, now and later. the [examples](crates/crabshell/examples/) show the rest. from a clone, `cargo run --release -p crabshell --example launcher` runs one, and so do `counter`, `bar`, `effects` and `desktop`

### docs

~ [the site](docs/site/index.html), with getting started, writing a config, how it fits together, the roadmap and the features. open it in a browser\
~ [the design doc](docs/design.md), with every decision and every rule settled while building each crate

### numbers

on headless sway with one 1920×1080 output, the counter example uses 16.7 MiB and draws its first frame in 9.1 ms, and the v0.1 bar uses 20.2 MiB. neither wakes while idle, except the bar's clock once a minute. on a 1280×720 output, a reload rebuilds a one-line edit in 0.7 s and leaves no bar for 13 to 19 ms. quickshell drawing the [phase 0 test bar](spike/RESULTS.md) used 125 MiB and took 219 ms to its first frame. the [design doc](docs/design.md#performance-targets) has every phase's numbers

### crates

~ [crabshell](crates/crabshell/), `run()`, `per_output`, windows, the session lock, the prelude and the examples\
~ [crabshell-reactive](crates/crabshell-reactive/), signals, memos, effects and owners\
~ [crabshell-ui](crates/crabshell-ui/), widgets, styles, layout, text and focus\
~ [crabshell-render](crates/crabshell-render/), drawing on the cpu with tiny-skia, and damage tracking\
~ [crabshell-wayland](crates/crabshell-wayland/), the event loop, windows, input and monitors\
~ [crabshell-services](crates/crabshell-services/), workspaces, audio, the tray, d-bus services, programs, files and logins\
~ [crabshell-cli](crates/crabshell-cli/), the `crabshell` command: `init`, `check`, `ipc` and `run`\
~ [spike](spike/) and [baseline](baseline/), the phase 0 prototype and the quickshell bar it was measured against

### tests

```sh
cargo test --workspace
env -u SWAYSOCK cargo test -p crabshell-wayland --test headless -- --ignored --test-threads=1
env -u SWAYSOCK cargo test -p crabshell --test headless -- --ignored --test-threads=1
env -u SWAYSOCK cargo test -p crabshell-services --lib sway::tests::headless -- --ignored --test-threads=1
env -u SWAYSOCK cargo test -p crabshell-cli --test exit_check -- --ignored --nocapture
```

the first needs `dbus-daemon` and `dbus-monitor`, and its golden images expect noto sans as your `sans-serif`, plus dejavu sans. ci runs it with fmt and clippy on every push. the others run only on your machine. each test starts a private sway from a copy of `sway`, so nothing reaches your session, and they refuse to start with `SWAYSOCK` set. they need `sway`, `swaymsg`, `grim`, `labwc`, `wl-copy`, `wl-paste`, `pipewire` and `pactl`. `CRABSHELL_BLESS=1` rewrites the golden images

### formatting

2 spaces everywhere. `cargo fmt --all` reads [rustfmt.toml](rustfmt.toml), and the rest gets

```sh
ruff format --config indent-width=2 baseline/
prettier --tab-width 2 --write $(git ls-files '*.md' '*.yml' '*.html' '*.css' '*.js')
```

_license: [mit](./LICENSE-MIT) or [apache-2.0](./LICENSE-APACHE), your choice_
