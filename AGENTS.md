# Crabshell

Crabshell is a full Rust replacement for Quickshell: every feature Quickshell has, Crabshell gets. A user's whole desktop shell is a Rust crate. The first milestone, v0.1, is a bar for Sway and other wlroots compositors, a checkpoint that proves the approach; the design doc's "Beyond v0.1" phases bring the rest, from notifications and lock screens to Hyprland. Don't design anything as if a bar were the whole scope.

The spec is the design doc, [docs/design.md](docs/design.md). Before changing a crate, read the doc's Overview, "Writing a config", and that crate's section with its "Rules settled while building …" list. When the doc and the code disagree, stop and ask.

## What we never compromise on

Review every change against all four rules before you commit it. Run each rule's check on the new code, then give one line per rule in your report: held, with the proof, or broken. A rule the change can't reach still gets a line saying why. A broken rule blocks the commit.

### 1. Lighter than Quickshell

This is the point of the project. Idle RSS stays at or under 30 MB on one 1080p output and under a quarter of Quickshell's, which used 125 MiB for the same bar. The first frame lands within 100 ms of startup.

**Check.** A change that can move memory or startup (startup code, fonts, caches, buffers, a new dependency) runs `measure_the_counter_bar` on the base commit in a second checkout and on the change. Report both runs' RSS and first frame from `target/tmp/headless/measurements.txt`.

### 2. Idle means asleep

Nothing dirty means no commit and no wakeup. A clock ticking once a minute is the only periodic wakeup a v0.1 bar gets. A timer, poll or animation that fires while nothing changes is a bug.

**Check.** Any change under `crates/` runs the `crabshell` headless suite, where `an_idle_counter_bar_wakes_nothing` counts the bar's context switches over a minute. A new wakeup source the counter bar doesn't use, such as a timer, a socket or a D-Bus match, gets its own idle test.

### 3. A config is plain Rust

A config is a normal Cargo crate that calls `crabshell::run`, using the API in the doc's "Writing a config". Keep the code and that section in sync. Every widget property takes a value or a closure, signals are `Copy` handles, and `Style` is a `const` value.

**Check.** Name each public item the change adds or alters and confirm it keeps those three rules. Put the matching "Writing a config" edit in your report, or say the section still matches.

### 4. One bad binding never takes the shell down

User code (bindings, effects, event handlers, `per_output` builders, listeners) runs inside `catch_unwind`, with every `RefCell` borrow released first. A panic is logged with its widget path, such as `PanelWindow[DP-1]/Row/Button[2]/Text.text`, and the rest keeps running. A panic in the root closure passed to `run` exits with status 1. A panic in the toolkit's own Wayland or rendering code ends the process too, and the supervisor (Phase 5) will restart it.

**Check.** List every new place the toolkit calls user code. Each one needs its `catch_unwind`, no borrow held across the call, and a test that panics there and asserts the logged widget path and that the shell keeps running.

## A note from the maintainer

I'm new to Rust and review every change through agents, so make your work reviewable: one crate per task, about 500 changed lines per commit, and a plain-words reason for each Rust choice in your report (`Rc` or `Arc`, a lifetime, `Box<dyn Trait>` or generics). Build the smallest thing that fully does the job; _yagni_ beats architecture that only looks impressive.

Treat this file as good defaults. What I say in the conversation overrides it.

## A small glossary

- **config**: a user's crate that calls `crabshell::run`. **shell**: the binary built from it. **supervisor**: the `crabshell` CLI that builds, runs and restarts the shell (Phase 5).
- **output**: a monitor (`wl_output`), named like `DP-1`. **window**: one layer-shell surface, built from a `PanelWindow`.
- **owner**: the reactive-core scope holding signals, memos and effects. Disposing it frees everything inside.
- **phase**, **step**: the roadmap in the design doc. A step is one crate's build, review, fix and commit.

## The three ways to hurt yourself

1. **Running the installed `sway`.** SwayFX holds `cap_sys_nice` and switches itself to realtime. T3 Code passes its children a realtime CPU limit of 0, so the kernel SIGKILLs `/usr/bin/sway` about 0.1 s in. A plain copy has no file capabilities, so the headless test helper starts a copy, and the suites run straight from your shell.
2. **Touching the live session.** The desktop Sway runs the maintainer's real session. Send it nothing: no `swaymsg` without your private `SWAYSOCK`. Your shell inherits the desktop's `SWAYSOCK`, so run the headless suites under `env -u SWAYSOCK`; the helper's `Sway::start()` refuses otherwise. Leave the pointer, outputs and other processes alone, and stop only the PIDs you started.
3. **Blessing goldens to go green.** Setting `CRABSHELL_BLESS` to any value rewrites the golden PNGs in render's and crabshell's `tests/golden`. Bless only for an intended pixel change, and name each file and the reason in your report. A font mismatch is a setup problem: the goldens need Noto Sans as fontconfig's `sans-serif`, plus DejaVu Sans and DejaVu Sans Bold.

## Hit every layer

The most common defect here is a change that works where you tested it and breaks the layer next door. Before calling work done, walk this list and say which entries applied:

- **Crates.** reactive → ui → render → wayland → crabshell. A ui change reaches render's painted areas and wayland's damage. A render change reaches the goldens.
- **Scales.** 1, 1.25, 1.5, 1.75 and 2. A buffer is logical × scale, rounded half away from zero.
- **Outputs.** Added, removed, re-announced under the same name, and windows the compositor closes.
- **Owners.** Everything a change creates is disposed with its owner, and `Runtime::node_count()` returns to where it started.
- **Damage.** Only the changed area repaints, a buffer that missed frames catches up, and more than half dirty repaints whole.
- **Outside data.** Icons, theme files, tray pixmaps, D-Bus replies, IPC JSON and compositor events come from other programs. Log and skip bad input, and keep to the size caps and refusals in render's settled rules.
- **Docs.** A rule settled during the work belongs in that crate's "Rules settled" list.

## Verifying

- Before every commit, run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`.
- Write the test first and watch it fail. Reactive and layout logic get unit tests, drawing gets goldens, and platform behavior gets headless Sway tests.
- Prove each new test bites: break the line it guards, watch it go red, restore it, then `touch` the restored file so cargo rebuilds it.
- The Sway-backed tests in each crate's `tests/headless.rs` are `#[ignore]`, and `CRABSHELL_MEASURE=1` makes `measure_the_counter_bar` record the performance numbers. Run them from your shell. Each test starts its own headless Sway from a copy of `sway` in a private runtime dir, with its own sockets, so nothing reaches the desktop Sway:

  ```sh
  env -u SWAYSOCK cargo test -p crabshell-wayland --test headless -- --ignored --test-threads=1
  env -u SWAYSOCK cargo test -p crabshell --test headless -- --ignored --test-threads=1
  env -u SWAYSOCK CRABSHELL_MEASURE=1 cargo test -p crabshell --test headless measure_the_counter_bar -- --ignored
  ```

  Afterwards `pgrep -a sway` shows only the desktop Sway, the oldest.

## Formatting

Everything is indented with 2 spaces, the maintainer's editor settings. Format every file you touch before you commit:

- Rust: `cargo fmt --all`. `rustfmt.toml` sets `tab_spaces = 2`, and the 2024 edition sets the rest, import order included.
- Python: `ruff format --config indent-width=2 baseline/`.
- Markdown and YAML: `prettier --tab-width 2 --write $(git ls-files '*.md' '*.yml')`.
- TOML and QML have no formatter. Keep each file's existing indentation.

## Commits and pushes

- Commit and push to `main` when the maintainer asks. Pull requests are welcome too.
- Each commit builds and passes on its own. When you split work into several commits, check each in a clean worktree.
- Write plain imperative subjects in US English, like `Add crabshell-render with damage planning`.

## Documentation

- The design doc holds the decisions and settled rules. Put proposed doc changes in your report, and edit `docs/design.md` only when asked.
- A code comment says why, in one line, when the code can't.
- `README.md` uses the maintainer's lowercase style. Match it when you edit.

## Plans and work artifacts

Keep plans, review proof tests and scratch files in your scratchpad or `/tmp`, outside the repo. The commit history is the record.

## How it works

Everything runs on one thread. A calloop loop dispatches ready sources, runs `runtime.flush()`, lays out the windows marked for layout, draws each dirty window whose frame callback has arrived, flushes the Wayland connection and sleeps. The reactive core keeps signals, memos and effects in a thread-local arena under owners. ui turns widgets into a taffy layout with cosmic-text text and records layout or paint damage. render draws the damaged area with tiny-skia into an RGBA scratch pixmap, then copies it into an ARGB8888 shm buffer, which is BGRA in memory, so red and blue swap. wayland owns the layer-shell surfaces, buffers, fractional scale, pointer input and output hotplug. `crabshell` ties it together with `run()` and `per_output`.

## Where code lives

- `crates/crabshell-reactive`: signals, memos, effects, owners and `flush`.
- `crates/crabshell-ui`: `Cx`, widgets, styles, layout, text, hit testing and invalidation.
- `crates/crabshell-render`: drawing, damage history, glyphs, icons and the in-house icon theme lookup. Goldens are in `tests/golden`.
- `crates/crabshell-wayland`: the event loop, layer-shell windows, shm buffers, scale, pointer and outputs. The headless tests and shared Sway helper are in `tests/`.
- `crates/crabshell`: `run`, `Shell`, `ShellExt::per_output`, `cx.persist`, the prelude and `examples/counter.rs`. Its headless tests, goldens and exit-code tests are in `tests/`.
- `crates/crabshell-cli`: the `crabshell` command (`init`, `check`, and `run` with the supervisor) and the config template in `template/`, which must match the design doc's "Writing a config".
- `spike/` and `baseline/`: the frozen Phase 0 prototype, the Quickshell bar it was measured against, and `baseline/harness.py`, which measured both.

## Taste

- Every crate root has `#![forbid(unsafe_code)]`, a new crate's too, and tests and examples stay free of `unsafe`. If a task seems to need it, stop and explain why.
- `expect` is for internal invariants, with a message naming the invariant.
- Dependencies come from the doc's crate table, pinned once as `=x.y.z` under `[workspace.dependencies]`, and licensed MIT, Apache-2.0, BSD, ISC, Zlib or Unicode. That's why the icon theme lookup is in-house: `freedesktop-icons` pulls in an MPL-2.0 crate.
- Work from specs: the Wayland protocol XML, the freedesktop specs, sway-ipc(7) and the PulseAudio docs. Quickshell is LGPL-3.0, so its source stays unread.
- One thread means `Rc` and `RefCell`.
- Children are `Box<dyn Widget>`. Deeply nested `impl Widget` types slow compiles, and compile time is reload time.
- Drawing stays behind plain functions in `crabshell-render`. A renderer trait waits for a second backend.
- If a rule here fights the task, say so and get the maintainer's sign-off before breaking it.

## Gotchas

- sctk 0.21 uses `delegate_dispatch2!` and has no fractional-scale helper, so `wp_fractional_scale_v1` is bound through `wayland-protocols`.
- In Phase 2, headless SwayFX ignored `WLR_RENDERER=pixman` and still rendered on the GPU.
- Load only the font files fontconfig matches into cosmic-text. `load_system_fonts` costs startup time.
- zbus 6 runs on `calloop::futures::executor`, with no tokio.
- Unix socket paths are capped at 108 bytes, so a private `XDG_RUNTIME_DIR` must stay short.
