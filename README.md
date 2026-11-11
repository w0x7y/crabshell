# crabshell

a full rust replacement for quickshell. your whole desktop shell is a rust crate: bars now, then popups, menus, the tray and the rest. the first release targets sway and other wlroots compositors

![the counter example bar at scale 2](crates/crabshell/tests/golden/counter@2x.png)

the counter example, drawn at scale 2

status: early. bars draw, take clicks, follow monitors as they come and go, and show the clock, sway's workspaces and the volume. the tray comes next, and nothing is on crates.io yet

vibe coded: ai coding agents write the code, and other agents review each step against a design doc

### try it

needs rust 1.99 or newer, fontconfig, and sway or another wlroots compositor

```sh
git clone https://github.com/w0x7y/crabshell
cd crabshell
cargo run --release -p crabshell --example counter
```

a 30 px bar shows up at the top of every monitor. click the button to count

```sh
cargo run --release -p crabshell --example bar
```

the v1 bar from the design doc, so far without the tray: each monitor's sway workspaces at the left, the volume and the clock at the right. click a workspace to switch to it, scroll over the volume to change it and click it to mute. [examples/bar.rs](crates/crabshell/examples/bar.rs)

### a bar

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

every widget property takes a value or a closure. a closure runs again when a signal it reads changes, and only what changed is redrawn. `per_output` builds windows for every monitor, now and later, and drops them when the monitor goes. the full example, with styles and hover, is [examples/counter.rs](crates/crabshell/examples/counter.rs)

### numbers

the counter example on a 1920×1080 screen under swayfx

~ 13 MiB of memory\
~ first frame in 13 ms\
~ zero wakeups while idle

for comparison, quickshell drawing the [phase 0 test bar](spike/RESULTS.md) uses 125 MiB and needs 219 ms to its first frame

### what's in it

~ [crabshell](crates/crabshell/), `run()`, `per_output`, the prelude and the examples\
~ [crabshell-reactive](crates/crabshell-reactive/), signals, memos, effects and owners\
~ [crabshell-ui](crates/crabshell-ui/), widgets, styles, layout, text and hit testing\
~ [crabshell-render](crates/crabshell-render/), drawing on the cpu with tiny-skia, damage tracking, glyphs and icons\
~ [crabshell-wayland](crates/crabshell-wayland/), the event loop, layer-shell windows, buffers, scaling, the pointer and monitors\
~ [crabshell-services](crates/crabshell-services/), the clock, sway's workspaces and audio\
~ [spike](spike/) and [baseline](baseline/), the phase 0 prototype and the quickshell bar it was measured against

### tests

```sh
cargo test --workspace
```

ci runs fmt, clippy and these tests on every push. the headless sway tests only run on your machine

the headless sway tests are skipped by default. run them from any terminal, without your session's `SWAYSOCK` so nothing can reach your sway. they refuse to start with it set

```sh
env -u SWAYSOCK cargo test -p crabshell-wayland --test headless -- --ignored --test-threads=1
env -u SWAYSOCK cargo test -p crabshell --test headless -- --ignored --test-threads=1
```

they need `sway`, `swaymsg` and `grim`, plus `pipewire` and `pactl` for the audio ones. each test runs its own private sway from a copy of `sway`. the copy drops swayfx's `cap_sys_nice`, so it never goes realtime and a realtime cpu limit of 0 can't kill it. the audio ones also run their own pipewire-pulse, so your volume never moves. `CRABSHELL_BLESS=1` rewrites the golden images, which expect noto sans as your `sans-serif`

_license: [mit](./LICENSE-MIT) or [apache-2.0](./LICENSE-APACHE), your choice_
