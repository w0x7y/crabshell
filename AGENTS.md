# Crabshell

Crabshell is a full Rust replacement for Quickshell. A user's whole desktop shell is a Rust crate: bars first, then popups, menus, the tray and the rest. v1 targets Sway and other wlroots compositors.

The spec is the design doc, https://claude.ai/artifact/7HPbf3YkX5dkkwvLTqv5oe, a Claude Docs document; read it through the docs connector and strip the XML to text. Before changing a crate, read the doc's Overview, "Writing a config", and that crate's section with its "Rules settled while building …" list. When the doc and the code disagree, stop and ask.

## What we never compromise on

### 1. Lighter than Quickshell

This is the point of the project. Idle RSS stays at or under 30 MB on one 1080p output and under a quarter of Quickshell's, which used 125 MiB for the same bar. The first frame lands within 100 ms of startup. Measure before and after any change to startup, fonts, caches or buffers.

### 2. Idle means asleep

Nothing dirty means no commit and no wakeup. A clock ticking once a minute is the only periodic wakeup a v1 bar gets. A timer, poll or animation that fires while nothing changes is a bug.

### 3. A config is plain Rust

A config is a normal Cargo crate that calls `crabshell::run`, using the API in the doc's "Writing a config". Keep the code and that section in sync. Every widget property takes a value or a closure, signals are `Copy` handles, and `Style` is a `const` value.

### 4. One bad binding never takes the shell down

User code (bindings, effects, event handlers, `per_output` builders, listeners) runs inside `catch_unwind`. A panic is logged with its widget path, such as `PanelWindow[DP-1]/Row/Button[2]/Text.text`, and the rest keeps running. A panic in the root closure passed to `run` exits with status 1. A panic in the toolkit's own Wayland or rendering code ends the process too, and the supervisor (Phase 5) will restart it.

## A note from the maintainer

I'm new to Rust and review every change through agents, so make your work reviewable: one crate per task, about 500 changed lines per commit, and a plain-words reason for each Rust choice in your report (`Rc` or `Arc`, a lifetime, `Box<dyn Trait>` or generics). Build the smallest thing that fully does the job; _yagni_ beats architecture that only looks impressive.

Treat this file as good defaults. What I say in the conversation overrides it.

## A small glossary

- **you**: the agent reading this file.
- **maintainer**: the person who owns this repo and directs the work.
- **config**: a user's crate that calls `crabshell::run`. **shell**: the binary built from it. **supervisor**: the `crabshell` CLI that builds, runs and restarts the shell (Phase 5).
- **output**: a monitor (`wl_output`), named like `DP-1`. **window**: one layer-shell surface, built from a `PanelWindow`.
- **owner**: the reactive-core scope holding signals, memos and effects. Disposing it frees everything inside.
- **damage**: the rectangles of a window that changed and get repainted.
- **golden**: a reference PNG that a test compares against at 2/255 per channel.
- **phase**, **step**: the roadmap in the design doc. A step is one crate's build, review, fix and commit.

## The three ways to hurt yourself

1. **Starting Sway from your shell.** SwayFX holds `cap_sys_nice` and switches itself to realtime. T3 Code passes its children a realtime CPU limit of 0, so the kernel SIGKILLs any Sway you start about 0.1 s in. Run headless suites through the desktop Sway with the recipe under Verifying.
2. **Touching the live session.** The desktop Sway runs the maintainer's real session. Send it exactly one command, that `exec`, with no `;` or `,`, since Sway splits commands on both. Leave the pointer, outputs and other processes alone, and stop only the PIDs you started.
3. **Blessing goldens to go green.** Setting `CRABSHELL_BLESS` to any value rewrites the golden PNGs in render's and crabshell's `tests/golden`. Bless only for an intended pixel change, and name each file and the reason in your report. A font mismatch is a setup problem: the goldens need Noto Sans as fontconfig's `sans-serif`, plus DejaVu Sans and DejaVu Sans Bold.

## Hit every layer

The most common defect here is a change that works where you tested it and breaks the layer next door. Before calling work done, walk this list and say which entries applied:

- **Crates.** reactive → ui → render → wayland → crabshell. A ui change reaches render's painted areas and wayland's damage. A render change reaches the goldens.
- **Scales.** 1, 1.25, 1.5, 1.75 and 2. A buffer is logical × scale, rounded half away from zero.
- **Outputs.** Added, removed, re-announced under the same name, and windows the compositor closes.
- **Owners.** Everything a change creates is disposed with its owner, and `Runtime::node_count()` returns to where it started.
- **Damage.** Only the changed area repaints, a buffer that missed frames catches up, and more than half dirty repaints whole.
- **Idle.** No new wakeups.
- **Outside data.** Icons, theme files, tray pixmaps and compositor events come from other programs. The size caps and refusals are in render's settled rules.
- **Docs.** A public API change updates "Writing a config". A rule settled during the work goes in that crate's "Rules settled" list.

## Running it

- Rust 1.99 or newer and fontconfig. `cargo build --workspace`.
- `cargo run --release -p crabshell --example counter` inside Sway shows the example bar on every output. `CRABSHELL_LOG=debug` raises the log level.
- The shell logs `first frame committed … ms after the shell started`, timed from `crabshell::run`. For memory, take `ps -o rss= -p <pid>` after 60 s idle.

## Verifying

- Before every commit, run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`. CI runs the same three on pushes to `main` and on pull requests.
- Write the test first and watch it fail. Reactive and layout logic get unit tests, drawing gets goldens, and platform behavior gets headless Sway tests.
- Prove each new test bites: break the line it guards, watch it go red, restore it, then `touch` the restored file so cargo rebuilds it.
- The Sway-backed tests in each crate's `tests/headless.rs` are `#[ignore]`, and `CRABSHELL_MEASURE=1`, placed inside the `exec` string before `~/.cargo/bin/cargo`, makes `measure_the_counter_bar` record the performance numbers. Run them through the desktop Sway, where `<repo>` is the checkout's absolute path and `<crate>` is `crabshell-wayland` or `crabshell`:

  ```sh
  SOCK=/run/user/$(id -u)/sway-ipc.$(id -u).$(pgrep -xo sway).sock
  rm -f <repo>/target/headless.status
  SWAYSOCK=$SOCK swaymsg exec 'cd <repo> && ~/.cargo/bin/cargo test -p <crate> --test headless -- --ignored --test-threads=1 > target/headless.log 2>&1 && echo passed > target/headless.status || echo failed > target/headless.status'
  ```

  `exec` returns at once, so wait for `target/headless.status`, then read it and `target/headless.log`. For a second checkout, set `CARGO_TARGET_DIR` and use absolute paths for both files.

## Commits and pushes

- `origin` is the private repo `w0x7y/crabshell`, branch `main`. Commit and push when the maintainer asks. Pull requests are welcome, and CI runs on them.
- Keep commits small, each one building and passing on its own. When you split work into several commits, check each in a clean worktree.
- Write plain imperative subjects in US English, like `Add crabshell-render with damage planning`.

## Documentation

- The design doc holds the decisions and settled rules. Put proposed doc changes in your report, and edit the doc only when asked.
- A code comment says why, in one line, when the code can't.
- `README.md` uses the maintainer's lowercase style. Match it when you edit.

## Plans and work artifacts

Keep plans, review proof tests and scratch files in your scratchpad or `/tmp`, outside the repo. The commit history is the record.

## How it works

Everything runs on one thread. A calloop loop dispatches ready sources, runs `runtime.flush()`, lays out the windows marked for layout, draws each dirty window whose frame callback has arrived, flushes the Wayland connection and sleeps. The reactive core keeps signals, memos and effects in a thread-local arena under owners. ui turns widgets into a taffy layout with cosmic-text text and records layout or paint damage. render draws the damaged area with tiny-skia into an RGBA scratch pixmap, then copies it into an ARGB8888 shm buffer with red and blue swapped. wayland owns the layer-shell surfaces, buffers, fractional scale, pointer input and output hotplug. `crabshell` ties it together with `run()` and `per_output`.

## Where code lives

- `crates/crabshell-reactive`: signals, memos, effects, owners and `flush`.
- `crates/crabshell-ui`: `Cx`, widgets, styles, layout, text, hit testing and invalidation.
- `crates/crabshell-render`: drawing, damage history, glyphs, icons and the in-house icon theme lookup. Goldens are in `tests/golden`.
- `crates/crabshell-wayland`: the event loop, layer-shell windows, shm buffers, scale, pointer and outputs. The headless tests and shared Sway helper are in `tests/`.
- `crates/crabshell`: `run`, `Shell`, `ShellExt::per_output`, the prelude and `examples/counter.rs`. The Phase 2 exit-criteria tests are in `tests/headless.rs` with goldens in `tests/golden`, and the exit-code tests are in `tests/exit.rs`.
- `spike/` and `baseline/`: the frozen Phase 0 prototype, the Quickshell bar it was measured against, and `baseline/harness.py`, which measured both.
- Coming next: `crabshell-services` (clock, Sway workspaces, audio, tray) and `crabshell-cli` (the supervisor).

## Taste

- Every crate root (each `src/lib.rs`, and `spike/src/main.rs`) has `#![forbid(unsafe_code)]`, and tests and examples stay free of `unsafe` too. If a task seems to need it, stop and explain why.
- Bad outside data (compositor events, D-Bus replies, IPC JSON, files) is logged and skipped. `expect` is for internal invariants, with a message naming the invariant.
- Dependencies come from the doc's crate table, pinned once as `=x.y.z` under `[workspace.dependencies]`, and licensed MIT, Apache-2.0, BSD, ISC, Zlib or Unicode. That's why the icon theme lookup is in-house: `freedesktop-icons` pulls in an MPL-2.0 crate.
- Work from specs: the Wayland protocol XML, the freedesktop specs, sway-ipc(7) and the PulseAudio docs. Quickshell is LGPL-3.0, so its source stays unread.
- One thread means `Rc` and `RefCell`. Release every borrow before calling user code.
- Children are `Box<dyn Widget>`. Deeply nested `impl Widget` types slow compiles, and compile time is reload time.
- Drawing stays behind plain functions in `crabshell-render`. A renderer trait waits for a second backend.
- If a rule here fights the task, say so and get the maintainer's sign-off before breaking it.

## Gotchas

- sctk 0.21 uses `delegate_dispatch2!` and has no fractional-scale helper, so `wp_fractional_scale_v1` is bound through `wayland-protocols`.
- wl_shm ARGB8888 is BGRA in memory.
- In Phase 2, headless SwayFX ignored `WLR_RENDERER=pixman` and still rendered on the GPU.
- Load only the font files fontconfig matches into cosmic-text. `load_system_fonts` costs startup time.
- zbus 6 runs on `calloop::futures::executor`, with no tokio.
- Unix socket paths are capped at 108 bytes, so a private `XDG_RUNTIME_DIR` must stay short.
