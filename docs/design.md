# Crabshell, a full Rust replacement for Quickshell

Nov 10, 2026 · @Idan Gilboa

Build a new shell toolkit in Rust instead of porting Quickshell. Configs are compiled Rust crates, the toolkit draws on the CPU, and the first release is a bar for Sway and other wlroots compositors. The overview is for you. Every section after it is the spec that AI agents build from.

## Overview

Nothing from Quickshell carries over. Users write their shell as a Rust crate that links the toolkit, and a supervisor rebuilds and restarts the shell when they save. The project is called Crabshell. The `crabshell` crate and every `crabshell-*` name were free on crates.io and the AUR on 2026-11-10.

| Decision           | Choice                                     | What it costs                                                                        |
| ------------------ | ------------------------------------------ | ------------------------------------------------------------------------------------ |
| Config language    | Plain Rust, compiled                       | First build takes 1-5 min, then 2-10 s per edit. Every user needs a Rust toolchain.  |
| Config API         | Builder methods and fine-grained signals   | Users learn closures and `move`. Signals are `Copy`, so nobody clones handles.       |
| Reload             | Supervisor rebuilds, saves state, restarts | The bar blinks for about 100 ms per reload.                                          |
| Styling            | Typed Rust styles only                     | Every color change is a rebuild.                                                     |
| Rendering          | Own CPU renderer, GPU later                | No blur or shadows in v1.                                                            |
| v1 scope           | Bar with workspaces, clock, tray, volume   | No notifications, launcher, lock screen or OSDs.                                     |
| Compositors        | Sway and other wlroots compositors         | No Hyprland, niri, KDE or X11 in v1.                                                 |
| C libraries        | Fine when the system ships them            | Links libpulse and fontconfig at runtime. Wayland itself is pure Rust.               |
| Quickshell configs | No compatibility, no migration             | Users rewrite their configs by hand.                                                 |
| License            | MIT or Apache-2.0                          | Agents write fresh code and never translate Quickshell's LGPL-3.0 source.            |
| Team               | You plus AI agents, you new to Rust        | The spec spells out every rule, and each phase ends in a check you can run yourself. |

### Better than Quickshell

- **Memory.** No QML engine, no JavaScript engine and no GPU context in v1. The target is 30 MB or less at idle for the v1 bar, checked in Phase 0.
- **Idle CPU.** With nothing changing on screen, the clock's once-a-minute tick is the only wakeup.
- **Startup.** The shell is a compiled binary. The first frame should land within 100 ms of launch.
- **Crashes.** Rust rules out memory bugs in the toolkit. A panic inside one config closure disables that binding and logs it, and the rest of the bar keeps running. If the process dies anyway, the supervisor restarts it and falls back to the last build that worked.
- **Config errors.** The compiler catches them before the shell runs, and rust-analyzer gives autocomplete and inline errors.

### Worse than Quickshell

- **Reload is slower.** Quickshell reloads QML in well under a second. Here every edit costs a 2-10 s rebuild plus a restart. Startup gets faster, but this design misses your "faster reload" goal.
- **Configs need Rust.** That is a higher bar than QML for users, and for you while you learn.
- **No ecosystem.** There are no existing configs to copy from.

**Timeline.** My guess is 4 to 6 months of focused work to reach v1, longer if part time. The Phases section breaks that down. Treat it as a guess, not a plan.

## At a glance

Your config compiles into the shell; `crabshell run` rebuilds and restarts it. Two processes, and everything inside the shell runs on one thread.

```text
+---------------------------------+               +---------------------------------+
| Config crate                    |    on save    | crabshell run, the supervisor   |
| ~/.config/crabshell/src/main.rs | ------------> | watches src/, runs cargo build  |
| plain Rust and typed styles     |               | restarts on crash, keeps        |
+---------------------------------+               | last-good                       |
                |                                 +---------------------------------+
                | compiled into the binary                        |
                |                                                 | starts it, socket: save, ready
+---------------|-------------------------------------------------|-----------------+
| Shell process,| built from the config crate                     |                 |
|               v                                                 v                 |
|  +--------------------------+                 +--------------------------+        |
|  | Config code              |                 | crabshell-services       |        |
|  | builder calls, closures  |                 | Sway IPC, tray (D-Bus),  |        |
|  | bindings read signals    |                 | pipewire-pulse, clock    |        |
|  +--------------------------+                 +--------------------------+        |
|               | creates bindings                          | set() on updates      |
|               |        +--------------------------+       |                       |
|               +------> | crabshell-reactive       | <-----+                       |
|                        | signals, memos, effects  |                               |
|                        | flushes once per turn    |                               |
|                        +--------------------------+                               |
|                                     | effects mark widgets dirty                  |
|                                     v                                             |
|                        +--------------------------+                               |
|          +-----------> | crabshell-ui             |                               |
|          |             | widgets and taffy layout |                               |
|          |             | dirty flags per widget   |                               |
|          |             +--------------------------+                               |
|          |                          | dirty areas to repaint                      |
|          | pointer                  v                                             |
|          | events      +--------------------------+                               |
|          |             | crabshell-render         |                               |
|          |             | tiny-skia into shm       |                               |
|          |             | repaints dirty areas     |                               |
|          |             +--------------------------+                               |
|          |                          | buffers and damage                          |
|          |                          v                                             |
|          |             +--------------------------+                               |
|          +------------ | crabshell-wayland        |                               |
|                        | calloop, layer-shell,    |                               |
|                        | popups, pointer, frames  |                               |
|                        +--------------------------+                               |
+-------------------------------------^-|-------------------------------------------+
                                      | | buffers out, input events in
                                      | v
                        +--------------------------+
                        | Sway compositor          |
                        | layer-shell, shm, popups |
                        +--------------------------+
```

The supervisor only builds the config crate and swaps the running shell. Everything else, from a service update to the pixels on screen, happens on the shell's one thread.

## Writing a config

A config is a normal Cargo project in `~/.config/crabshell/`, created from a template by `crabshell init`. Build output goes to `~/.cache/crabshell/target`, so the config directory stays small.

```
~/.config/crabshell/
├── Cargo.toml
└── src/
    └── main.rs
```

The template's `Cargo.toml` compiles dependencies once with full optimization and the config itself at `opt-level = 1`, so edits rebuild fast and the bar still runs fast.

```toml
[package]
name = "my-shell"
version = "0.1.0"
edition = "2024"

[dependencies]
crabshell = "0.1"

[profile.dev]
opt-level = 1

[profile.dev.package."*"]
opt-level = 3
```

The whole v1 bar as a user writes it:

```rust
use crabshell::prelude::*;

const BAR: Style = Style::new()
    .background(Color::rgb(0x1e, 0x1e, 0x2e))
    .color(Color::rgb(0xcd, 0xd6, 0xf4))
    .font(Font::new("Inter", 13.0));
const WS: Style = Style::new().padding(2, 8).radius(4.0);
const WS_FOCUSED: Style = WS.background(Color::rgb(0x45, 0x47, 0x5a));

fn main() {
    crabshell::run(|cx| cx.per_output(bar));
}

fn bar(cx: &Cx, output: &Output) -> PanelWindow {
    let clock = cx.clock(Precision::Minute);
    let workspaces = cx.sway().workspaces();
    let sink = cx.audio().default_sink();
    let output_name = output.name();

    PanelWindow::new(output)
        .anchors(Edge::Top | Edge::Left | Edge::Right)
        .height(30)
        .exclusive_zone(30)
        .style(BAR)
        .child(
            Row::new()
                .gap(8)
                .padding(4, 8)
                .child(Row::new().gap(4).children_keyed(
                    move || {
                        workspaces.get().into_iter()
                            .filter(|w| w.output == output_name)
                            .collect::<Vec<_>>()
                    },
                    |w| w.id,
                    |_cx, ws| {
                        Button::new(Text::new(move || ws.get().name))
                            .style(move || if ws.get().focused { WS_FOCUSED } else { WS })
                            .on_click(move |cx| cx.sway().focus_workspace(&ws.get().name))
                    },
                ))
                .child(Spacer::new())
                .child(Tray::new().icon_size(18))
                .child(
                    Button::new(Text::new(move || match sink.get() {
                        Some(s) if s.muted => "muted".to_string(),
                        Some(s) => format!("vol {}%", s.volume_percent),
                        None => "no audio".to_string(),
                    }))
                    .on_click(|cx| cx.audio().toggle_mute())
                    .on_scroll(|cx, steps| cx.audio().change_volume(steps * 5)),
                )
                .child(Text::new(move || clock.get().strftime("%a %d %b  %H:%M").to_string())),
        )
}
```

API rules agents must keep:

- `crabshell::run` takes the root closure, starts the event loop and never returns.
- `cx.per_output(f)` calls `f` for every output that exists now or appears later. When an output goes away, its window and everything created inside `f` are dropped.
- `Signal<T>` (read and write) and `ReadSignal<T>` (what services hand out) are `Copy` handles, so closures capture them without cloning. `get()` returns a clone and subscribes the closure it runs in. `with(|v| ...)` reads by reference. `set()` and `update()` write.
- Every widget property takes a plain value or a closure. `.height(30)` sets it once. `.height(move || ...)` is a binding that re-runs when a signal it read changes.
- `children_keyed(items, key, view)` builds one child per item and reuses it while its key stays in the list. `view` receives a `Memo<T>` that updates when that item's value changes.
- Event handlers (`on_click`, `on_right_click`, `on_middle_click`, `on_scroll`) receive `&Cx` and run on the main thread.
- `Style` is `Copy` and every builder method is a `const fn`, so a theme is a set of constants. `Text` inherits `color` and `font` from the nearest ancestor that sets them. Nothing else inherits.
- `cx.persist("key", default)` returns a `Signal<T>` for any `T: Serialize + DeserializeOwned`. Its value survives reloads. If an edit changes the type so the saved value no longer fits, it falls back to the default and logs once.
- A service that isn't running gives `None` or an empty list. A missing Sway socket or audio server logs once and never panics.
- Containers store children as `Box<dyn Widget>`. Deeply nested `impl Widget` types slow compiles, and compile time is reload time.

API rules settled while building `crabshell` (Phase 2, step 4):

- `cx.per_output` and `cx.set_icon_theme` come from the `ShellExt` trait, which the prelude brings in. Only the crate that defines a type can add methods to it, so Phase 3's services add `cx.sway()` and the rest the same way.
- `f` returns anything that implements `IntoPanels`: a `PanelWindow`, or a `Vec<PanelWindow>` for several windows or none.
- Call `per_output` from `root` or from inside another `per_output`'s `f`. A builder ends when the context it was registered with is disposed, and its windows go with it. Called from a plain effect, it adds another builder on every run.
- A panic in `f` is logged with the output and the call site, for example `per_output[DP-1] (called at src/main.rs:4)`. Whatever `f` built is dropped, and that output gets nothing until it is announced again. When the compositor closes a window, that output's bar is dropped the same way and logged at info.
- `cx.set_icon_theme` repaints every window whole.
- `crabshell::run` exits with 1 (`EXIT_ROOT_PANICKED`) when `root` panics, and with 2 (`EXIT_NO_COMPOSITOR`) when there is no compositor, a required protocol is missing, or the connection is lost. A logout is not a crash.
- `CRABSHELL_LOG` sets the log level, such as `debug`. Unset or empty means `info`, and a value that doesn't parse is warned about once.
- `cx.provide::<T>()` and `cx.service::<T>()` use one services map shared by every child context. `Shell` drives the loop by hand for tests, and dropping it frees everything it made.
- The prelude holds the widgets, styles, `Cx`, `Output`, the signal types, `ShellExt` and `IntoPanels`. Since Phase 3 it also holds `ServicesExt` (for `cx.clock()`, `cx.sway()` and `cx.audio()`), `Precision`, `Workspace` and `Sink`. Since Phase 4 it also holds `PopupWindow`, `Menu`, `MenuEntry`, `MenuToggle`, `Tray`, and `TrayHost` with its item and menu types, for `cx.tray()`.

API rules settled while building `crabshell` (Phase 3, step 4):

- `Shell::new` calls `crabshell_services::provide` before `root`. Each service starts on first use, so a config that never calls `cx.sway()` opens no Sway connection.
- `crabshell::jiff` re-exports jiff, since `cx.clock()` hands out a `jiff::Zoned`.
- The shell logs `workspace switch on screen in X ms` at the first commit from any window after Sway's event. A switch no commit shows within 1 s is dropped. The log reads the Sway service only if a config started it.
- Linking libpulse costs every config about 2.1 MiB of RSS, even one with no volume widget.
- The headless suites run under `env -u SWAYSOCK`, and the test helper refuses otherwise. Every shell a test starts gets the private `SWAYSOCK` and a `PULSE_SERVER` that is private or doesn't exist. No in-process test shell calls `cx.sway()` or `cx.audio()`.
- Sway deletes an empty workspace once it isn't shown, so a test that needs two workspaces uses two outputs.

API rules settled while building `crabshell` (Phase 5, step 1):

- `cx.persist` comes from `ShellExt`. Its signal lives under the root owner, so it outlasts the context that asked and any output. The same key gives the same signal. The same key with another type is logged and gets a signal under the asking context that isn't kept.
- `run` reads `$XDG_RUNTIME_DIR/crabshell/state.json` before `root` and removes it, so a shell started later, as after a crash, starts fresh. Without `XDG_RUNTIME_DIR`, nothing is saved, and that is logged.
- On `save`, the values are written to a new file beside `state.json` that then replaces it, in a directory only the user can read (a looser one is tightened, a symlink refused), and the shell exits with 0. A value whose `Serialize` fails or panics is logged and left out. A saved value whose `Deserialize` fails or panics is logged, and the default is used.
- The shell sends `ready` once every panel it shows has committed a frame. A panel waiting for its output, or refused by layer-shell, isn't waited for, and a shell that shows no panel never says `ready`. An unknown command is logged and skipped. If the supervisor hangs up, the shell runs on alone.
- `Shell::new` starts with no saved values. Only `run` reads the file and follows the supervisor.
- The counter example persists each output's count, so a reload keeps it.

## Process model and reload

Two processes run. The `crabshell` CLI, installed once per machine, is the supervisor. The shell is the user's own compiled binary, which links the `crabshell` library.

v1 has three CLI commands:

- `crabshell init` copies the template into `~/.config/crabshell/`. It refuses if the directory already exists.
- `crabshell run` builds the config, starts the shell, watches for edits and supervises.
- `crabshell check` runs `cargo check` and prints errors, for editors and scripts.

What `crabshell run` does:

1. Sets `CARGO_TARGET_DIR=~/.cache/crabshell/target` and runs `cargo build` in the config directory.
2. Listens on a Unix socket in `$XDG_RUNTIME_DIR/crabshell/`, then starts the built binary with the socket's path in `CRABSHELL_CONTROL`. The shell connects and writes `ready` once every panel it shows has committed its first frame. (A path rather than an inherited descriptor: adopting a raw descriptor number is `unsafe` in Rust, and the crates forbid `unsafe`.)
3. Watches `src/` and `Cargo.toml` with the `inotify` crate, debounced by 300 ms. (`notify` is CC0-1.0, which the license policy doesn't allow.)
4. On a change, runs `cargo build` again. If the build fails, the running shell stays up. The supervisor prints the full error and sends the first error line as a desktop notification over `org.freedesktop.Notifications`.
5. If the build succeeds, the supervisor writes `save` to the socket. The shell writes every `cx.persist` value to `$XDG_RUNTIME_DIR/crabshell/state.json` and exits with code 0. The supervisor then starts the new binary, which reads that file on startup.
6. If the shell exits with any other code or dies from a signal, the supervisor restarts it after 1 s, doubling the wait each time up to 30 s. Three crashes within 60 s of a new build make it start `~/.cache/crabshell/last-good` instead and send a notification.
7. A binary that has run for 60 s without crashing is copied to `last-good`.

Panic handling inside the shell:

- Builds use `panic = "unwind"`. The toolkit wraps every binding, effect, event handler and service callback in `catch_unwind`.
- A panicking binding is disabled and logged with its widget path, for example `PanelWindow[DP-1]/Row/Button[2]/Text.text`. The rest of the bar keeps running.
- The reactive core releases every `RefCell` borrow before it calls user code, so a caught panic never leaves the runtime borrowed. A reactive-core test checks this.
- A panic in the toolkit's own Wayland or rendering code is not caught. The process exits and the supervisor restarts it.

Logging goes through `tracing` to stderr. The supervisor passes the shell's stderr through to its own, so `journalctl` sees both when `crabshell run` runs as a systemd user service.

The blink during a reload lasts as long as shell startup, which targets 100 ms. Starting the new shell before stopping the old one would remove the blink. But both bars would reserve screen space for a moment and every window would jump, so that handoff waits until after v1.

## Reactive core

The `crabshell-reactive` crate is a small single-threaded signal system of about 600 to 1,000 lines. Build it in-house, because frame scheduling and widget invalidation hook straight into it. If it stalls, Leptos's `reactive_graph` crate is the fallback.

| Type            | Created with                                   | What it is                                                               |
| --------------- | ---------------------------------------------- | ------------------------------------------------------------------------ |
| `Signal<T>`     | `cx.signal(value)`                             | A value you set                                                          |
| `ReadSignal<T>` | `signal.read_only()`, or returned by a service | A read-only view of a signal                                             |
| `Memo<T>`       | `cx.memo(\|\| ...)`, needs `T: PartialEq`      | A derived value, recomputed only when read while stale                   |
| `Effect`        | `cx.effect(\|\| ...)`                          | Code that reruns when what it read changes. Every widget binding is one. |

Semantics:

- Every handle is a `Copy` key into a thread-local `slotmap` arena, plus `PhantomData<fn(T) -> T>`. The runtime lives on the main thread, and handles are `!Send`.
- `get()` or `with()` inside a memo, effect or binding records a dependency. `get_untracked()` reads without recording one. Dependencies are recorded again on every run, so a branch that stops reading a signal stops depending on it.
- `set()` and `update()` store the value, mark direct dependents stale and queue effects. They never run effects on the spot. A signal notifies on every `set`, even when the value didn't change.
- A memo recomputes only when something reads it while it's stale. If the new value equals the old one, its dependents are not notified.
- `runtime.flush()` runs once per event-loop iteration, before layout and painting. It runs queued effects in creation order. Each effect runs at most once per flush, even with diamond-shaped dependencies.
- An effect that writes a signal it depends on is queued again. After 100 rounds in one flush, the runtime logs the cycle with the effect's widget path, disables that effect and finishes the flush.
- Every node belongs to an owner. Each widget opens one, and dropping the widget disposes its owner and everything created under it. `get()` on a disposed handle panics with a clear message, which the binding's `catch_unwind` turns into a log line. `try_get()` returns `None` instead.
- User closures run with no runtime borrow held. A panic in one disables that node and leaves the runtime usable.
- The core has no async. Services deliver updates on the main thread and call `set()` like any other code.

Required tests:

- [ ] Diamond. Signal `a` feeds memos `b` and `c`, and one effect reads both. One `set` on `a` runs the effect exactly once.
- [ ] A memo that recomputes to an equal value doesn't rerun its dependents.
- [ ] After a branch switch, setting the signal the old branch read doesn't rerun the effect.
- [ ] Disposing an owner frees every arena slot it created. Count slots before and after.
- [ ] A self-feeding effect stops after 100 rounds, and the other effects in the same flush still run.
- [ ] A panicking effect is disabled, the next `set` and `flush` work, and no `RefCell` is left borrowed.
- [ ] `try_get()` on a disposed handle returns `None`.

Added after the Phase 1 review on 2026-11-10:

- The `fn(T) -> T` marker stops Rust from quietly converting a `Signal<A>` into a `Signal<B>`. Every handle implements `PartialEq`, `Eq`, `Hash` and `Debug` by key, with no bounds on `T`.
- `owner.effect_scoped(|scope| ...)` gives each run a fresh child owner. The runtime disposes it before the next run and when the effect is disposed, so children rebuilt on every run don't pile up. A plain `effect` frees nothing between runs.
- `untrack(|| ...)` runs code without recording dependencies, so a widget built inside a binding doesn't make that binding depend on what the widget reads.
- `try_set()` and `try_update()` return `false` and do nothing on a disposed handle, for callbacks that may outlive their window.
- A memo must not write signals. A write or a flush from inside a memo panics, and the memo is disabled and logged.
- A panic in the `Drop` of a user value or closure is caught and logged like any other panic in user code.
- `flush()` repeats rounds until nothing is queued. An effect created during a flush first runs later in that same flush; otherwise a new effect first runs at the next flush.
- At thread exit, once the runtime is torn down, every handle acts as disposed: `set()`, `update()` and `dispose()` do nothing, and `try_get()` returns `None`.

More required tests:

- [ ] An effect whose captured value panics when dropped is disabled, and later flushes still run every other effect.
- [ ] Exiting with an owner still alive, by returning from `main` or calling `process::exit(0)`, gives exit status 0.
- [ ] Rebuilding children 10 times with `effect_scoped` leaves the arena's slot count where it was after the first run.
- [ ] Disposing 50 child owners that read one long-lived signal returns that signal's subscriber count to where it started.

## Widgets, layout and styles

v1 ships ten widgets. Nine are in `crabshell-ui`. `Tray` is in `crabshell-services`, because it needs the tray service and `crabshell-ui` can't depend on services; the prelude re-exports it. Users build everything else by combining them.

| Widget          | What it does                                                                                                                  |
| --------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `PanelWindow`   | A layer-shell surface on one output with anchors, size, exclusive zone and layer. It takes no keyboard focus in v1.           |
| `PopupWindow`   | An `xdg_popup` anchored to a widget's rectangle. Tray menus use it.                                                           |
| `Row`, `Column` | Flex layout with `gap`, `padding`, `align` and `justify`.                                                                     |
| `Stack`         | Children drawn on top of each other in one cell.                                                                              |
| `Spacer`        | Takes the leftover space in a `Row` or `Column`.                                                                              |
| `Text`          | One line by default, with `ellipsis` and `max_lines`.                                                                         |
| `Icon`          | A freedesktop icon name, an SVG or PNG path, or raw ARGB pixels from a tray item.                                             |
| `Button`        | A clickable box with `hover_style` and `pressed_style`, plus `on_click`, `on_right_click`, `on_middle_click` and `on_scroll`. |
| `Menu`          | A vertical list of buttons built from a DBusMenu layout, with separators, checkmarks and submenus as nested popups.           |
| `Tray`          | One icon button per tray item, each opening its `Menu`. Built only from the widgets above.                                    |

### Layout

- `taffy` computes flexbox layout. `Row` and `Column` map to a flex direction, and `Stack` maps to a one-cell grid.
- `Text` and `Icon` report their size through taffy's measure function. Text measuring uses the same `cosmic-text` buffer the renderer draws from, so measured and drawn sizes can't disagree.
- Every unit is a logical pixel. The renderer multiplies by the output's scale.
- A `PanelWindow` anchored left and right gets its width from the compositor's `configure` event. Layout runs again after every configure.

### Invalidation

- Each property is either a layout property or a paint property. Text content, font, padding, gap, size and children are layout. Colors, background, border, opacity and radius are paint.
- A binding that changes a paint property marks only its own widget for repaint. A layout change marks the whole window for layout, then every widget whose rectangle moved is repainted at both its old and new position.
- When nothing is dirty there is no layout, no paint and no commit.

### Styles

- `Style` has `background`, `color`, `font` (family, size, weight), `padding`, `radius`, `border` (width, color) and `opacity`. Every field is optional, and an unset field falls back to the widget's default.
- `Text` inherits `color` and `font` from the nearest ancestor that sets them. Nothing else inherits, and there are no selectors or classes.
- Colors are sRGB with alpha. `Color::rgb`, `Color::rgba` and `Color::hex` are all `const fn`.

### Input

- Pointer events come from `wl_pointer`. Hit testing walks the tree from the top and picks the deepest `Button` under the pointer.
- The cursor shape is set through `wp_cursor_shape_v1`, with a pointer cursor over buttons.
- `on_scroll` receives whole steps. Mouse wheels use `axis_value120`. Touchpad scrolling is summed and becomes one step per 15 logical pixels.
- v1 has no keyboard input, so it doesn't need libxkbcommon.

Rules settled while building `crabshell-ui` (Phase 2, step 1):

- `Color::hex` takes `"#rrggbb"` or `"#rrggbbaa"`. A bad string in a `const` is a compile error.
- Widget paths look like `PanelWindow[DP-1]/Row/Button[2]/Text.text`. Each container numbers its children in creation order, single and keyed alike, and never reuses a number.
- `Row` and `Column` default to align stretch and justify start. Text sits vertically centered in its box.
- Text defaults to black `sans-serif` at 13 px, with a line height of `round(1.2 × size)`. `max_lines` defaults to 1 with no ellipsis. More lines wrap at words, and a word too long for a line breaks.
- A row's or column's own `padding` wins over its style's padding.
- The hover style goes over the base style, and the pressed style over both. Pressed shows only while the button is held and the pointer is still over it.
- Scroll steps are positive for up and don't pass to ancestor buttons. A partial touchpad scroll resets when the hovered button changes or the gesture ends (`axis_stop`).
- Event handlers are `Fn`. A handler that panics is logged with its path and disabled, like a binding.
- The exclusive zone defaults to the height for a top or bottom bar, the width for a left or right one, and 0 otherwise. Width and height default to 0, which lets the compositor decide. `surface()` is valid only after the first flush.
- The window's child is exactly the window's size. Content that doesn't fit overflows and is not clipped, as in CSS. Damage covers everything a widget paints, including text lines taller than their box, so the renderer must not clip text either.
- `into_element` describes the whole subtree as plain data, untracked, before the window's tree is borrowed.
- Child contexts come from the parent `Cx` through `Cx::child`, so the services map reaches every widget. `cx.per_output` reaches the platform the same way.
- Font fallback asks fontconfig per character, family and weight. fontconfig's face index carries a variable font's named instance in its upper 16 bits, so mask it with `0xFFFF` before passing it to fontdb.
- `Icon::new` accepts a theme name as `&str` or `String`, a file path, or raw ARGB pixels. The default size is 16.
- Every crate may use `tracing` for logging.

Rules settled while building `crabshell-ui` (Phase 4, step 3):

- A widget opens a popup from its own context, such as a handler's, with `cx.open_popup(|cx| PopupWindow::new()...)`. The popup belongs to that widget and closes when the widget goes.
- A popup is the size of its content, in whole logical pixels and at least 1×1.
- `cx.close_popup()` closes the popup it is called in, every popup it opened, and every popup that opened it.
- The popup's builder runs untracked. A panic in it is logged with a path ending in `/PopupWindow`, and nothing opens.
- `Menu` keys its entries by `(id, separator)`. A separator is 2 px tall. Checkmarks and radio marks reserve the width of the widest mark, so labels line up.
- `MenuEntry` and `MenuToggle` live in `crabshell-ui`, and `crabshell-services` re-exports them.

## Rendering

v1 draws on the CPU with `tiny-skia`, straight into shared-memory buffers, and repaints only what changed. For a bar that uses less memory than a GPU context. A 1920×30 bar buffer is 225 KiB.

### Buffers

- Each window gets two or three ARGB8888 `wl_shm` buffers from sctk's `SlotPool`. A buffer is reused once the compositor releases it.
- `tiny_skia::PixmapMut::from_bytes` wraps the buffer's memory, so drawing writes into it with no copy.
- Buffer size is the logical size multiplied by the output scale, rounded halfway away from zero, as wp_fractional_scale_v1 specifies.

### Damage tracking

- Each frame collects the dirty rectangles from invalidation. The renderer clips to their union, repaints the widgets that intersect it, and reports each rectangle with `wl_surface.damage_buffer`.
- Each buffer remembers how many frames ago it was last drawn. Before reusing one, the renderer also repaints whatever changed in the frames that buffer missed.
- When the dirty rectangles cover more than half the window, repaint the whole window. It's simpler and about as fast.

### Frame pacing

- After each commit, request a `wl_surface.frame` callback. Draw only when the callback has arrived and something is dirty.
- Nothing dirty means no commit, so an idle bar never wakes up to draw.

### Scale

- Use `wp_fractional_scale_v1` with `wp_viewporter` when the compositor offers them. Otherwise fall back to integer `wl_surface.set_buffer_scale`.

### Text

- `cosmic-text` shapes and lays out text, and rasterizes glyphs through `swash` with its own glyph cache.
- Don't scan every system font at startup. On a machine with thousands of font files, that alone could miss the 100 ms startup target. Resolve the families the config names through fontconfig and load only those files into `cosmic-text`'s font database. Missing glyphs use fontconfig's fallback list. In Phase 0, fontconfig's startup took about 15 of the 17.7 ms to first frame, and its cache files held about 4 MiB of memory.
- Glyphs are drawn as grayscale alpha masks. There is no subpixel antialiasing in v1.

### Icons and images

- Icon names resolve through the freedesktop icon theme spec with an in-house lookup. The config picks a theme with `cx.set_icon_theme("...")`, and the default is `hicolor`.
- `resvg` renders SVG icons. It already draws with tiny-skia, so it renders into the same pixmap. PNGs load with the `png` crate.
- Rasterized icons are cached by name, size and scale.

### Shapes

Solid fills, rounded rectangles and borders. No blur, shadows or gradients in v1.

### Later GPU backend

Keep every drawing call inside `crabshell-render`, behind plain functions. A GPU backend using `vello` on `wgpu` replaces that crate's internals later. Don't add a renderer trait until a second backend exists.

Rules settled while building `crabshell-render` (Phase 2, step 2):

- Buffers use `ARGB8888`. Damaged areas are drawn in 64-pixel tiles of a fixed grid, each into a reused RGBA scratch pixmap, then copied into the buffer with red and blue swapped. This replaces drawing straight into the buffer: it keeps glyphs, resvg and png in tiny-skia's order, at the cost of one copy of the changed pixels.
- A pixel is always drawn in the same tile, whatever the damage. tiny-skia cuts a curve where it leaves the pixmap, which changes the antialiasing beside the cut, so drawing each damaged area as one pixmap made a repaint differ from a full draw.
- Damage: logical rectangles round outward to pixels, plus a 1 px margin. Overlapping rectangles merge. Each window keeps 4 frames of history. A buffer older than that, a new buffer, a buffer last drawn by another window, or a new size or scale is repainted whole. The reported damage is this frame's change. More than half dirty reports the whole buffer.
- `draw` returns `Result<_, CanvasError>`. The caller commits after every successful draw, in order, or the reported damage is wrong.
- Shapes snap to whole pixels. The radius is clamped to half the shorter side. A border is drawn inside the edge, its width rounded down with a 1 px minimum.
- Opacity draws a widget and its children as one layer, sized to the subtree's bounds (`NodeView::bounds()`). Opacity 0 skips the subtree.
- A widget's painted area includes an icon square larger than its box, and text's line box plus 1 em on each side and half an em above and below for ink.
- Bitmap color glyphs are premultiplied when drawn. COLR glyphs already are.
- Icon names resolve through an in-house lookup that follows the freedesktop icon theme spec. `freedesktop-icons` pulls in an MPL-2.0 dependency, so it isn't used. Extensions are tried PNG, then SVG. XPM is logged once and skipped. A fractional scale looks up the next whole scale.
- Limits on outside files: SVG up to 1 MiB, with no links to outside files and no filters. PNG up to 4 MiB and 1024 px a side. Tray pixmaps up to 512 px a side. `index.theme` up to 1 MiB. Anything else must be a regular file. Icon and theme names with `/`, `..`, `.` or NUL are refused.
- Caches: 128 rasterized icons, dropping those unused for 16 draws. 1,024 theme lookups, least recently used first. Glyph images are cleared past 2,048. Icon directory listings are kept until restart.

## Wayland platform

`crabshell-wayland` uses `smithay-client-toolkit` (sctk) on a single-threaded `calloop` event loop. Everything that touches widgets or signals runs on that one thread.

| Protocol                                                 | Used for                                         |
| -------------------------------------------------------- | ------------------------------------------------ |
| `wl_compositor`, `wl_shm`                                | Surfaces and CPU buffers                         |
| `wl_output` version 4                                    | Output names such as `DP-1`, plus mode and scale |
| `zwlr_layer_shell_v1`                                    | `PanelWindow`                                    |
| `xdg_wm_base`, through `zwlr_layer_surface_v1.get_popup` | `PopupWindow`                                    |
| `wp_fractional_scale_v1`, `wp_viewporter`                | Fractional scaling                               |
| `wp_cursor_shape_v1`                                     | Cursor shapes                                    |
| `wl_seat`, pointer only                                  | Clicks, hover and scroll                         |

### Event loop

Each iteration of the loop:

1. Dispatch every ready source: Wayland, timers, D-Bus, the Sway IPC socket and the audio channel.
2. Run `runtime.flush()` so queued effects run.
3. Lay out every window marked for layout.
4. Draw each window that is dirty and has received its frame callback.
5. Flush the Wayland connection and sleep until a source is ready.

How each source plugs into calloop:

- **Wayland.** sctk's `WaylandSource`.
- **Timers.** calloop's `Timer`.
- **D-Bus.** zbus futures run on `calloop::futures::executor`. zbus 6 drives its socket on a helper thread from its own zruntime crate, which wakes the future when a reply arrives. Phase 0 proved it: a ListNames call took about 2.2 ms with no tokio.
- **Sway IPC.** A non-blocking `UnixStream` registered as a calloop `Generic` source.
- **Audio.** libpulse's threaded mainloop runs on its own thread and sends updates over a `calloop::channel`.

### Outputs

- `cx.per_output` builds the windows for an output when the compositor announces it. When the output is removed, its windows and their owners are dropped.
- A scale or mode change sends the output's windows a new configure, and they lay out again.
- An output that is unplugged and plugged back in gets fresh windows. `cx.persist` values live in a process-wide map keyed by name, so they survive this as well as reloads.

### Layer surfaces

- `PanelWindow` defaults to layer `top`, keyboard interactivity `none`, and an exclusive zone equal to its height on its anchored edge.
- When the compositor sends `closed`, drop the window.

### Popups

- A `PopupWindow` positions itself with `xdg_positioner`, anchored to the rectangle of the widget that opened it. It flips to the other side when it would leave the output.
- It grabs the pointer using the serial of the last button press or release on the shell's windows, usually the click that opened it. The compositor sends `popup_done` on a click outside, and the popup closes and drops its owner.

### Testing without a screen

Integration tests run Sway headless with `WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 sway -c test.conf`. They add outputs with `swaymsg create_output` and take screenshots with `grim`. The tests are marked `#[ignore]` and run with `--ignored --test-threads=1`.

A headless Sway started from T3 Code dies about 0.1 s in. SwayFX carries the `cap_sys_nice` file capability and switches itself to realtime scheduling. T3 Code and everything it starts have a realtime CPU limit of 0, so the kernel kills it with SIGKILL. That was also the Phase 0 failure, not the sandbox. A plain copy of the binary has no file capabilities and never goes realtime, so the test helper copies the `sway` it finds on PATH into the test's private runtime directory and starts the copy.

Agents run the suites straight from their shell:

```bash
env -u SWAYSOCK cargo test -p crabshell-wayland --test headless -- --ignored --test-threads=1
env -u SWAYSOCK cargo test -p crabshell --test headless -- --ignored --test-threads=1
```

- The desktop Sway gets nothing. The private Sway, `grim` and the shells under test start with a cleared environment, and every `swaymsg` names the private `SWAYSOCK`. The helper refuses to start while the test process has `SWAYSOCK` set, because an in-process shell would hand it to its services. Afterwards `pgrep -a sway` shows only the desktop Sway.
- SwayFX renders on the GPU whatever `WLR_RENDERER` says.
- Each test runs its own Sway, from its own copy of the binary, in a short private `XDG_RUNTIME_DIR` with `swaybg_command -`. Socket paths are limited to 108 bytes. A failing test prints Sway's exit status and log.
- A headless seat has no pointer device. A virtual pointer from a second client gives it one. Clicks then work both through `swaymsg seat - cursor set/press/release` and through the virtual pointer.

The `crabshell` suite runs the same way with `-p crabshell` and shares the wayland test helper. `CRABSHELL_BLESS=1` rewrites its goldens, and `CRABSHELL_MEASURE=1` runs the 60 s measurement. Its goldens need Noto Sans as fontconfig's `sans-serif`, and the test checks that first.

One wayland test runs on a headless labwc instead of Sway, to check popups and the pointer grab on another wlroots compositor, so the suite needs `labwc` installed too. The helper starts it like Sway: in a private runtime directory, with an empty config and a cleared environment.

Rules settled while building `crabshell-wayland` (Phase 2, step 3):

- A side of 0 needs anchors on both edges across it, as layer-shell requires. Otherwise the window is refused and logged once with its path, and the other windows keep running. A settings change that breaks the rule keeps the last valid settings.
- `wl_compositor` must be version 4 or later, for `damage_buffer`.
- Buffer size is logical × scale rounded halfway away from zero, computed in 120ths and capped at 16384 px.
- The first frame is drawn at the output's whole scale until the compositor sends a preferred fractional scale. A pure scale change repaints whole without a new layout, since layout is in logical pixels. A mode change arrives as a configure and lays out again.
- A configure side of 0 takes the requested size for that side. If both are 0, the configure is ignored and logged.
- Each window has its own `SlotPool` with up to three buffers, and reuses the free buffer drawn most recently.
- A failed draw or attach resets the window's damage history and asks for a redraw, so the next frame repaints whole.
- Windows are dropped by the output object they were shown on, then `Event::OutputRemoved` is sent. A window whose output isn't announced yet waits, and logs that once.
- `Event::WindowClosed` is sent after the compositor's `closed`. A panic in the `on_event` listener is logged, and the listener is kept.
- After a layout, the cursor shape is sent again for any pointer over that window if it changed. Without `wp_cursor_shape_v1`, the cursor is never set.
- Horizontal scroll and buttons other than left, right and middle are ignored. A whole scroll notch counts as 120. `axis_stop` calls `Window::pointer_scroll_end`, which drops a partial scroll step.
- `crabshell-wayland` depends on `crabshell-reactive` directly, for `Runtime::flush`. `wayland-protocols` is pinned at 0.32.13 with the client and staging features, the same copy sctk uses.
- Invalidation: a node that draws nothing itself (no background, border, text or icon) adds no damage when it moves or resizes. The window always counts as drawing. A paint change counts a node as drawing until its next layout.

Rules settled while building `crabshell-wayland` (Phase 4, step 4):

- `xdg_wm_base` is required, bound at up to version 6.
- Popups are taken after the flush and shown in the same loop iteration, laid out first so their size is known. A popup is created once its opener has committed a frame: through `get_popup` on a panel's layer surface, or as the child of another popup.
- A popup goes below its widget in a bar along the top or bottom, and beside it in a vertical bar or another popup. It flips and slides on both axes to stay on the output.
- The anchor rectangle is the widget's rectangle in whole pixels, at least 1×1, clamped inside the parent.
- A popup grabs the pointer only if its opener is a panel or grabbed too, since xdg-shell forbids a grab under a popup that didn't grab.
- Only one popup chain is open at a time: opening a popup closes every popup that isn't its opener or one of its openers. Popups are destroyed topmost first.
- `popup_done`, `close_popup`, or the opening widget going away drops a popup and the popups it opened.
- A popup whose content changes size sends the new size with `reposition` once it is mapped.
- `Platform::windows()` and `remove_window` cover popups. `WindowInfo::position` is a popup's place relative to its opener.

## Services

v1 has four services in `crabshell-services`. Each one owns its connection, runs on the main thread, and hands out `ReadSignal`s plus a few methods. Every service is created on first use and shared by all bars, so two outputs never mean two connections.

| Service    | Signals and methods                                                                                                                                                                                                                                                                               | Source                                                             | Crate                         |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ | ----------------------------- |
| Workspaces | `workspaces()` gives `Vec<Workspace>` with id, name, output, focused, visible, urgent. `focus_workspace(name)`.                                                                                                                                                                                   | Sway IPC at `$SWAYSOCK`                                            | Own framing with `serde_json` |
| Tray       | `items()` gives `Vec<TrayItem>` with id (bus name and path), app_id, title, status, icon_name, icon_pixmaps, attention_icon_name, icon_theme_path, tooltip, item_is_menu and menu. `activate`, `secondary_activate`, `scroll`, `menu()`, `open_menu`, `open_submenu`, `close_menu`, `menu_event`. | StatusNotifierItem and `com.canonical.dbusmenu` on the session bus | `zbus`                        |
| Audio      | `default_sink()` gives `Option<Sink>` with name, volume_percent, muted. `change_volume(percent)`, `toggle_mute()`.                                                                                                                                                                                | PulseAudio protocol, served by pipewire-pulse on PipeWire systems  | `libpulse-binding`            |
| Clock      | `clock(precision)` gives a `jiff::Zoned`                                                                                                                                                                                                                                                          | System time and time zone                                          | `jiff`                        |

### Workspaces

- Write the IPC framing in-house. Each message is a 14-byte header (`i3-ipc`, a length, a type) plus JSON, about 150 lines of code, and owning it avoids pulling in another async runtime.
- On start, send `SUBSCRIBE` to `workspace` events, then `GET_WORKSPACES`, so no change between the two goes unseen. On every event, send `GET_WORKSPACES` again and replace the whole list. That is simpler than applying each event's changes and costs one small JSON reply.
- If the socket closes, set the list to empty and reconnect every 2 s. A first connect that fails, as with a stale `SWAYSOCK` under another compositor, logs once and never retries, so it never wakes the bar.
- Other wlroots compositors don't speak Sway IPC. Before Phase 3, check which of your target compositors implement `ext-workspace-v1`. If none do, v1 shows workspaces on Sway only, and other compositors get the bar without them. Checked on 2026-11-11: labwc has it since 0.8.3, Sway and Wayfire only on their `master` branches, and river not at all. So v1 shows workspaces on Sway only, and an `ext-workspace-v1` backend waits until after v1.

### Tray

- If nobody owns `org.kde.StatusNotifierWatcher` on the session bus, Crabshell takes the name and runs the watcher itself. Otherwise it uses the existing watcher.
- Crabshell registers itself as `org.kde.StatusNotifierHost-<pid>`.
- Items register with either a bus name or only an object path. Electron and Ayatana apps send only a path. Accept both, using the sender's unique name when only a path arrives and `/StatusNotifierItem` when only a name arrives.
- Read `Id`, `Title`, `Status`, `IconName`, `IconPixmap`, `IconThemePath`, `AttentionIconName`, `ToolTip`, `Menu` and `ItemIsMenu`. Re-read on `NewIcon`, `NewTitle`, `NewStatus` and `NewToolTip`.
- A left click calls `Activate(x, y)`, or opens the menu when `ItemIsMenu` is true. A right click opens the menu, and a middle click calls `SecondaryActivate`.
- To open a menu, call `AboutToShow`, then `GetLayout(0, -1, names)`, where `names` lists the seven properties a menu shows: `type`, `label`, `enabled`, `visible`, `toggle-type`, `toggle-state` and `children-display`. An empty list would also bring every entry's `icon-data`. Read it again on `LayoutUpdated` and `ItemsPropertiesUpdated`. A click sends `Event(id, "clicked", ...)`.
- `IconPixmap` is ARGB32 in network byte order. Convert it to premultiplied RGBA and pick the size closest to the icon size.
- When Crabshell owns the watcher, a reload restarts it, and items must register again. Most apps re-register when the watcher's name owner changes. Phase 4 checks this with real apps.

### Audio

- Run libpulse's threaded mainloop and subscribe to sink and server events.
- On a server event, read the default sink name, then that sink's info. `volume_percent` is the average across channels, with 100% equal to `PA_VOLUME_NORM`.
- `change_volume` clamps to 0-100% in v1.
- If the server goes away, the signal becomes `None` and the service retries every 2 s for up to a minute after startup or after a working connection drops. Then it logs once and stops until the shell restarts.

### Clock

- `Precision::Second` or `Precision::Minute`. Each tick computes the next boundary from wall-clock time, so drift never builds up. Bars on every output share one clock per precision.
- Timers pause during suspend, so after resume the clock could show the wrong time for up to a minute. Listen for logind's `PrepareForSleep(false)` on the system bus and re-sync right away.
- Re-read the time zone when `org.freedesktop.timedate1` reports a `Timezone` change. Also re-read it on every tick: without timedated, as under OpenRC, nothing announces a changed `/etc/localtime`.

Rules settled while building `crabshell-services` (Phase 3, steps 1 to 3):

- `provide(cx, &handle)` runs once in `Shell::new`. It keeps the root `Owner` and a weak loop handle, never a `Cx` or a strong handle, so the loop and the compositor connection still go when the shell drops.
- Each service is its own services-map entry, starts on first use, and creates its signals under the root owner. Without `provide`, it warns once and hands out a still value under the caller's owner.
- One calloop futures executor runs every service's D-Bus futures. Nothing a service does at startup delays the first frame.
- Clock: the Unix timestamp is truncated to the precision, and a timer up to 50 ms early counts as on the boundary. A re-sync sets the signal and replaces the timer. logind and timedated are reached through match rules only, so nothing is bus-activated. With no system bus the clock logs once and keeps ticking.
- Sway: `focus_workspace(name)` sends `workspace "<name>"`, with `"` written as `"'"'"` and `$` as `$$`. It refuses Sway's keywords, newlines, NUL, and a backslash before `"`, `'`, `;` or `,`. A call while disconnected is dropped.
- Sway: a frame over 4 MiB, a bad magic, or more than 4 MiB of unsent bytes drops the connection. Bad JSON is logged and skipped. An unchanged list isn't set again. `changed_at()` is when the event behind the current list was read.
- Audio: every libpulse call runs on the main thread under the mainloop lock. Callbacks only send plain data, tagged with a connection generation, over one calloop channel. libpulse connects with `NOAUTOSPAWN` on the loop's next turn.
- Audio: `volume_percent` is the channel average, rounded as pactl prints it. A volume or mute step builds on the last one sent until a read echoes it, another sink is read, or 250 ms pass, because pipewire-pulse answers a read right after a set with the old value.
- Tests give every `Config` field explicitly, so none reaches the real `SWAYSOCK` or audio server. Audio's connected path is tested against a fake server, and `#[ignore]` tests run it against a private pipewire-pulse.

Rules settled while building `crabshell-services` (Phase 4, steps 1 and 2):

- Tray: the shell queues for `org.kde.StatusNotifierWatcher` and takes over as the watcher when the bus hands it the name.
- Tray: zbus has no object server here, so the watcher's methods and properties, `Introspect` included, are answered by hand on the main thread. A call that expects no reply is still acted on.
- Tray: watcher signals count only as broadcasts while another program owns the watcher name. An item's signals count only from its current owner and object path.
- Tray: pixmaps are at most 512 px a side and 1 MiB per item in total, and the smallest are kept first.
- Tray: calls to apps and to another watcher time out after 25 s, on a timer of the loop's own that goes as soon as the call ends. zbus's `method_timeout` left its timer behind, which woke zbus's thread once, 25 s after every call. Calls the bus answers itself have no timeout. `Activate` is sent at (0, 0). `Event` sends data 0 and timestamp 0. A `Menu` of `/NO_DBUSMENU` means no menu.
- Tray: only one menu is open at a time. Its match rule exists only while it is open, and a failed read closes it. A menu keeps at most 1024 entries, and submenus are read as they open.

Rules settled while building the `Tray` widget (Phase 4, step 5):

- `Tray::new()` takes `icon_size` (16 by default) and `menu_style`, the menu popup's style (light text on dark gray by default). Both take a value or a closure.
- It is built from `crabshell-ui`'s widgets. `cx.tray()` needs a context, which a keyed child's view gets, so the tray is one such child holding a row of the items. `Prop::shared` lets one property feed many widgets.
- A click opens the menu popup at once, so it grabs the pointer with the click's serial. The popup is clear and empty until the app's layout arrives. It closes once the tray stops following its opening: the app's menu went, a read failed (the first one too), or the app left. `menu_style` is the `Menu`'s style, so submenus get it too.
- When the popup goes, the tray stops following the app's menu, unless the menu was opened again since. `open_menu` numbers each opening for that.
- A widget doesn't know its output's scale, so pixmaps are picked for twice the icon size. An item with no icon shows `image-missing`.

## Crates

One Cargo workspace with seven crates. Each crate owns one area, so an agent can work inside one without touching the others.

| Crate                | Owns                                                                  | Depends on           | Main external crates                                                 |
| -------------------- | --------------------------------------------------------------------- | -------------------- | -------------------------------------------------------------------- |
| `crabshell-reactive` | Signals, memos, effects, owners, flush                                | none                 | `slotmap`                                                            |
| `crabshell-ui`       | `Cx`, widgets, styles, layout, text layout, hit testing, invalidation | reactive             | `taffy`, `cosmic-text`, `fontconfig`                                 |
| `crabshell-render`   | Drawing into pixmaps, damage tracking, glyphs, icons                  | ui                   | `tiny-skia`, `resvg`, `png`, `swash`                                 |
| `crabshell-wayland`  | Event loop, windows, popups, outputs, pointer, shm buffers            | reactive, ui, render | `smithay-client-toolkit`, `calloop`, `wayland-protocols`             |
| `crabshell-services` | Workspaces, tray, audio, clock                                        | reactive, ui         | `zbus`, `serde_json`, `libpulse-binding`, `jiff`, `calloop`, `serde` |
| `crabshell`          | Public API, `run()`, `prelude`, `cx.persist`                          | all of the above     | `serde`, `serde_json`, tracing-subscriber                            |
| `crabshell-cli`      | The `crabshell` binary with `init`, `run`, `check` and the supervisor | none                 | `inotify`, `zbus`                                                    |

`Cx` lives in `crabshell-ui` and carries a map of services keyed by type. `crabshell-services` adds `cx.sway()`, `cx.audio()`, `cx.tray()` and `cx.clock()` through an extension trait, so `crabshell-ui` never depends on services.

### Dependency policy

- Pin every external version once, in the workspace `Cargo.toml` under `[workspace.dependencies]`.
- Add a crate only when it saves more than about 200 lines or implements a protocol or file format. Add it to the table above first.
- Rust crates must be MIT, Apache-2.0, BSD, ISC, Zlib or Unicode licensed. `cargo deny check licenses` enforces this.
- System C libraries (libpulse, fontconfig) are linked dynamically. sctk speaks the Wayland protocol in pure Rust, so libwayland-client isn't linked.
- Use edition 2024 on the latest stable toolchain. No nightly features.

## Performance targets

Every target is a guess until Phase 0 measures it. The baseline is Quickshell running the same bar, with workspaces, tray, volume and clock, on the same machine and output. Use Quickshell's `Quickshell.I3` module for workspaces, and check that it connects to Sway's socket. Don't compare against numbers found online.

| Metric                                        | Target                                                     | How to measure                                                                  |
| --------------------------------------------- | ---------------------------------------------------------- | ------------------------------------------------------------------------------- |
| Idle memory (RSS), v1 bar on one 1080p output | 30 MB or less, and at most a quarter of Quickshell's       | `ps -o rss= -p <pid>` after 60 s idle                                           |
| Idle wakeups                                  | 1 per minute with a minute clock and nothing else changing | `perf stat -e 'syscalls:sys_exit_epoll_wait' -p <pid> -- sleep 300`             |
| Startup to first frame                        | 100 ms or less                                             | The shell logs the time from `main` to its first committed buffer               |
| Workspace switch to screen                    | Within one frame, 16 ms at 60 Hz                           | The shell logs the time from the IPC event to the commit                        |
| Rebuild after a one-line edit                 | 10 s or less                                               | The supervisor logs each build's duration                                       |
| Reload gap with no bar on screen              | 200 ms or less                                             | The supervisor logs the time from the old shell's exit to the new one's `ready` |

Record each measured number in this table next to its target when a phase finishes.

### Phase 0 results

Measured on 2026-11-10 on the Gentoo laptop under SwayFX, output eDP-1 at 1920×1080 and scale 1, median of 3 runs. Full data is in `spike/RESULTS.md`.

| Metric                              | Crabshell spike | Quickshell 0.3.2, same bar | Spike vs Quickshell |
| ----------------------------------- | --------------- | -------------------------- | ------------------- |
| Memory (RSS) 10 s after first frame | 13.2 MiB        | 125.2 MiB                  | 10.5%               |
| Proportional memory (PSS)           | 6.2 MiB         | 56.5 MiB                   | 11.0%               |
| Private dirty memory                | 1.3 MiB         | 30.3 MiB                   | 4.4%                |
| Threads                             | 2               | 20                         |                     |
| Idle wakeups over 5 s               | 0               | 0                          |                     |
| Launch to first frame               | 22 ms           | 219 ms                     | about 10× faster    |

**Verdict: go.** The spike's RSS is 10.5% of Quickshell's for the identical bar, against the target of at most 25%. Quickshell drew on the GPU through OpenGL, its default. Its PSS is lower than it would be alone because it shares Qt and Mesa with the user's running shell, so RSS is the headline number. The two bars match visually. Zoomed in, Qt uses subpixel antialiasing and places the text 1 px higher.

The results are in commits `16b596f` and `4a87ea8` on `main` in `~/GitRepo/crabshell`.

Not verified in Phase 0: exit on the compositor's `closed` event, and drawing at scales other than 1.0. Both were verified in Phase 2, step 3. The nested headless Sway was killed by a realtime CPU limit, not the sandbox; see Testing without a screen.

### Phase 2 results

Measured on 2026-11-11 with the example counter bar, release build. It has no services yet, so Phase 3 measures the full v1 bar against Quickshell's 125.2 MiB.

| Metric                       | Target         | Headless Sway, 1920×1080, scale 1 | Laptop eDP-1 under SwayFX, 1920×1080, scale 1 |
| ---------------------------- | -------------- | --------------------------------- | --------------------------------------------- |
| Startup to first frame       | 100 ms or less | 8.6 ms                            | 13.0 ms                                       |
| Idle memory (RSS) after 60 s | 30 MB or less  | 13.6 MiB                          | 13.2 MiB                                      |
| Idle wakeups with no clock   | 0              | 0 context switches in 60 s        | 0 context switches in 62 s                    |

The laptop run used the build from before the step 4 review fixes.

### Phase 3 results

Measured on 2026-11-11 with `examples/bar.rs`: workspaces, the minute clock and the volume button, with no tray. Release build on headless Sway, 1920×1080 at scale 1, with audio connected to a private pipewire-pulse.

| Metric                       | Target                                           | v1 bar, headless Sway                                    |
| ---------------------------- | ------------------------------------------------ | -------------------------------------------------------- |
| Startup to first frame       | 100 ms or less                                   | 14.2 ms                                                  |
| Idle memory (RSS) after 60 s | 30 MB or less, at most a quarter of Quickshell's | 17.9 MiB, 14% of Quickshell's 125.2 MiB                  |
| Idle wakeups                 | 1 per minute with a minute clock                 | 2 context switches at each minute boundary, none between |
| Workspace switch to screen   | within one frame, 16 ms at 60 Hz                 | median 0.6 ms, slowest 0.9 ms                            |

The counter bar, which uses no services, went from 13.4 to 15.5 MiB once `crabshell` linked libpulse. Its first frame didn't change.

### Phase 4 results

Measured on 2026-11-13 with `examples/bar.rs`, now with the tray: workspaces, the tray, the volume button and the minute clock. Release build on headless Sway, 1920×1080 at scale 1, with audio on a private pipewire-pulse and the tray running the watcher on a private session bus. Two runs.

| Metric                       | Target                                           | v1 bar, headless Sway                                         |
| ---------------------------- | ------------------------------------------------ | ------------------------------------------------------------- |
| Startup to first frame       | 100 ms or less                                   | 14.3 and 15.8 ms                                              |
| Idle memory (RSS) after 60 s | 30 MB or less, at most a quarter of Quickshell's | 18.7 and 18.8 MiB, 15% of Quickshell's 125.2 MiB              |
| Idle wakeups                 | 1 per minute with a minute clock                 | 2 or 3 context switches at each minute boundary, none between |
| Workspace switch to screen   | within one frame, 16 ms at 60 Hz                 | median 0.5 ms                                                 |

The tray adds about 0.8 MiB. The counter bar, with no tray, stays at 15.4 to 15.5 MiB.

## Phases

Seven phases totaling 16 to 26 weeks of focused work, all guesses. A phase is done only when its exit check passes. Phase 0 is a go/no-go gate for the whole project.

1. **Phase 0, spike (1-2 weeks).** A static text bar on Sway built from one layer-shell surface, one shm buffer, tiny-skia, and cosmic-text with fontconfig loading. Also prove one zbus call runs on calloop's executor.
   - Exit: the bar shows in a headless Sway screenshot and on your real session. RSS and first-frame time are recorded in Performance targets, next to a measured Quickshell baseline.
   - Gate: if RSS isn't clearly below Quickshell's, stop and rethink before Phase 1. Passed on 2026-11-10 with 13.2 MiB against Quickshell's 125.2 MiB for the same bar.
2. **Phase 1, reactive core (2-3 weeks).** `crabshell-reactive` as specified above.
   - Exit: every required reactive-core test passes. Passed on 2026-11-10: all 11 required tests pass after two review rounds, in commits 8dc878e, 1f07600 and aa045c0.
3. **Phase 2, widgets, layout and rendering (4-6 weeks).** Every widget except `PopupWindow`, `Menu` and `Tray`, plus styles, damage tracking, fractional scale, pointer input, and `per_output` with hotplug.
   - Exit: an example bar with a counter button matches golden screenshots on headless Sway at scales 1, 1.5 and 2. With no clock, idle wakeups are zero. `swaymsg output HEADLESS-2 unplug` drops that output's window, and the arena's slot count returns to where it started. Passed on 2026-11-11 after a review per step, in commits a0f47f6 to 5d37b6f (crabshell-ui), c2bd7c5 to b90142c (crabshell-render), e4e681e to d08b0a3 (crabshell-wayland) and 697280e to 56bcd7d (crabshell).
4. **Phase 3, clock, workspaces and audio (2-4 weeks).**
   - Exit: a workspace switch repaints within one frame, per the log. Scrolling changes the volume, and `pactl get-sink-volume @DEFAULT_SINK@` agrees. After a suspend and resume, the clock is right within 1 s. You check that last one by hand. Passed on 2026-11-11 after a review per step, in commits 11dc5e7 to 7c66869. The switch and volume checks run as headless tests against a private Sway and a private pipewire-pulse, and the maintainer checked suspend and resume, workspaces and volume on the laptop's real session.
5. **Phase 4, tray and popups (4-6 weeks).** Watcher, host, DBusMenu, `PopupWindow`, `Menu` and `Tray`.
   - Exit: tests with a fake tray item pass on a private `dbus-daemon`. By hand, `nm-applet --indicator`, `blueman-applet`, KeePassXC and one Electron app such as Discord all show icons, their menus open, clicks reach the app, and all of them survive a reload. Passed on 2026-11-13, except surviving a reload, which needs Phase 5's supervisor and is checked with it: the fake-item tests pass, and the maintainer checked nm-applet, blueman-applet, KeePassXC and Discord on the laptop's session, beside Quickshell's watcher and as the watcher. Commits 59477fd to de5710a, after a review per step, with popups also tested on headless labwc.
6. **Phase 5, CLI and reload (2-3 weeks).** `init`, `run`, `check`, the watcher, `cx.persist`, crash restarts and `last-good`.
   - Exit: a scripted test checks four cases. A syntax error leaves the old shell running and sends a notification. Fixing it brings up the new shell within build time plus 200 ms. After `kill -9` the shell is back within 2 s. A config that panics at startup three times falls back to `last-good`.
7. **Phase 6, release (1-2 weeks).** Template, README, API docs on docs.rs, published crates, and measured numbers in the performance table.
   - Exit: on a clean user account, you go from `cargo install crabshell-cli` to a running bar in under 15 minutes by following the README.

## Rules for AI agents

Read the Overview and the section for your crate before writing code. If this doc and the code disagree, stop and ask instead of picking one.

### Scope

- One task touches one crate plus its tests. A public API change also updates "Writing a config" in this doc.
- Keep each change under about 500 changed lines, so a Rust beginner can review it.
- Don't add a widget, service or option this doc doesn't list. Propose it under Risks and open questions instead.

### Licensing

- Never read, copy or translate Quickshell's source. It's LGPL-3.0 and this project is MIT or Apache-2.0.
- Work from the specs instead: the Wayland protocol XML files, the freedesktop StatusNotifierItem spec, the DBusMenu interface XML, `sway-ipc(7)` and the PulseAudio API docs.

### Code

- No `unsafe` in Crabshell's own crates. If a task seems to need it, stop and explain why in the change description.
- No `unwrap()` or `expect()` on data from outside the process, such as D-Bus replies, IPC JSON, files or compositor events. Log it and carry on. `expect` is fine for internal invariants, with a message that names the invariant.
- Before calling a task done, run `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test --workspace`.

### Tests

- Every behavior change comes with a test.
- Reactive and layout logic get unit tests.
- Rendering gets golden PNGs. Render to an offscreen pixmap and compare with a per-pixel tolerance of 2 out of 255. Update goldens only on purpose, and say so in the change description.
- Workspaces are tested against a fake Sway socket. The tray is tested with a fake item on a private `dbus-daemon --session`. Audio is tested against a fake server, and against a private pipewire-pulse in `#[ignore]` tests run by hand.
- Integration tests run on headless Sway, as described under Wayland platform.

### Reporting

- End every task with what changed, how you tested it, and what the human must check by hand on a real screen.
- Explain Rust choices in plain words for a beginner. When you pick `Rc` over `Arc`, add a lifetime, or choose `Box<dyn Trait>` over generics, say why in a sentence or two.

## Risks and open questions

The biggest risk is the reload loop. A 2-10 s rebuild per edit is the price of compiled configs, and it is the one goal this design doesn't meet.

| Risk                                      | Why it matters                                                              | Mitigation                                                                                                                         |
| ----------------------------------------- | --------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| Reload misses the goal                    | 2-10 s per edit, against Quickshell's sub-second reload                     | Fast dev profile, a small config crate, build time measured in Phase 5. Try Dioxus's experimental subsecond hot-patching after v1. |
| You review Rust you can't fully judge yet | Bugs in the reactive core or event loop hide well                           | Small changes, required tests, headless Sway screenshots, and plain-language explanations from agents                              |
| Tray apps bend the spec                   | Electron, Qt and Ayatana apps each implement StatusNotifierItem differently | Phase 4 gets 4-6 weeks and tests real apps                                                                                         |
| Text looks worse than Qt's                | v1 has grayscale antialiasing only                                          | Compare screenshots side by side in Phase 0                                                                                        |
| Font loading slows startup                | Scanning every system font can blow the 100 ms target                       | Load only the files fontconfig matches                                                                                             |
| Sway-first shrinks the audience           | Most Quickshell users run Hyprland or niri                                  | Keep compositor IPC inside `crabshell-services`, so adding Hyprland later touches one module                                       |
| Configs need a Rust toolchain             | A higher entry bar and a slow first build                                   | Template and README, with the full first run timed in Phase 6                                                                      |
| No accessibility in v1                    | Screen readers can't read the bar                                           | Add AccessKit (AT-SPI) right after v1                                                                                              |

Open questions:

- [x] Project name: Crabshell.
- [ ] Which other wlroots compositors to test (river, labwc, Wayfire), and which of them implement `ext-workspace-v1`.
- [ ] Should a reload start the new shell before stopping the old one, to remove the blink? That needs a fix for both bars reserving space at once.
- [ ] Stay on libpulse after v1, or switch to native PipeWire?
- [ ] When to add Hyprland and niri, and in which order.
- [ ] Should the shell exit with status 1 when `f` panics for every output at startup? Today it keeps running with no bar, so Phase 5's fallback to last-good after three crashes never fires for that config.
- [ ] Install cargo-deny and add a deny.toml. Until then the license policy is checked by hand with `cargo metadata`.

## Sources

- [Quickshell source](https://github.com/quickshell-mirror/quickshell) at commit `9b0f339` from 2026-10-21, counted locally. About 62k lines of C++ exposing 241 QML types, licensed LGPL-3.0.
- Everything else comes from memory and wasn't checked against current docs: crate names and APIs, protocol versions, Sway commands and toolchain behavior. Phase 0 checked the stack's crates against their source and found four changes: sctk 0.21 uses delegate_dispatch2! and has no fractional-scale helper, cosmic-text 0.19 changed set_text, and zbus 6 replaced async-io with zruntime. Pinned versions are in the repo's Cargo.toml.
