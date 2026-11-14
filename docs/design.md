# Crabshell, a full Rust replacement for Quickshell

Nov 10, 2026 · @Idan Gilboa

Build a new shell toolkit in Rust instead of porting Quickshell, and make it a full replacement: everything Quickshell can do, Crabshell does. Configs are compiled Rust crates and the toolkit draws on the CPU. The first milestone, v0.1, is a bar for Sway and other wlroots compositors. It is a checkpoint that proves the approach works, not the scope; the phases after it bring the rest of Quickshell's features (see "Beyond v0.1"). The overview is for you. Every section after it is the spec that AI agents build from.

## Overview

Nothing from Quickshell carries over. Users write their shell as a Rust crate that links the toolkit, and a supervisor rebuilds and restarts the shell when they save. The project is called Crabshell. The `crabshell` crate and every `crabshell-*` name were free on crates.io and the AUR on 2026-11-10.

| Decision           | Choice                                     | What it costs                                                                                                                                 |
| ------------------ | ------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------- |
| Config language    | Plain Rust, compiled                       | First build takes 1-5 min, then 2-10 s per edit. Every user needs a Rust toolchain.                                                           |
| Config API         | Builder methods and fine-grained signals   | Users learn closures and `move`. Signals are `Copy`, so nobody clones handles.                                                                |
| Reload             | Supervisor rebuilds, saves state, restarts | The shell blinks for about 100 ms per reload.                                                                                                 |
| Styling            | Typed Rust styles only                     | Every color change is a rebuild.                                                                                                              |
| Rendering          | Own CPU renderer, GPU later                | v0.1 had no gradients, shadows or blur. Phase 8 added gradients and shadows, and asks the compositor to blur.                                 |
| Scope              | Everything Quickshell does, in milestones  | v0.1 is a bar with workspaces, clock, tray and volume. Notifications, launchers, lock screens, OSDs and the rest come in the phases after it. |
| Compositors        | Sway and other wlroots compositors first   | Hyprland and the rest come after v0.1.                                                                                                        |
| C libraries        | Fine when the system ships them            | Links libpulse, fontconfig and libxkbcommon at runtime. Wayland itself is pure Rust.                                                          |
| Quickshell configs | No compatibility, no migration             | Users rewrite their configs by hand.                                                                                                          |
| License            | MIT or Apache-2.0                          | Agents write fresh code and never translate Quickshell's LGPL-3.0 source.                                                                     |
| Team               | You plus AI agents, you new to Rust        | The spec spells out every rule, and each phase ends in a check you can run yourself.                                                          |

### Better than Quickshell

- **Memory.** No QML engine, no JavaScript engine and no GPU context in v0.1. The target is 30 MB or less at idle for the v0.1 bar, checked in Phase 0, and every later feature keeps to the same budget.
- **Idle CPU.** With nothing changing on screen, the clock's once-a-minute tick is the only wakeup.
- **Startup.** The shell is a compiled binary. The first frame should land within 100 ms of launch.
- **Crashes.** Rust rules out memory bugs in the toolkit. A panic inside one config closure disables that binding and logs it, and the rest of the shell keeps running. If the process dies anyway, the supervisor restarts it and falls back to the last build that worked.
- **Config errors.** The compiler catches them before the shell runs, and rust-analyzer gives autocomplete and inline errors.

### Worse than Quickshell

- **Reload is slower.** Quickshell reloads QML in well under a second. Here every edit costs a 2-10 s rebuild plus a restart. Startup gets faster, but this design misses your "faster reload" goal.
- **Configs need Rust.** That is a higher bar than QML for users, and for you while you learn.
- **No ecosystem.** There are no existing configs to copy from.

**Timeline.** My guess is 4 to 6 months of focused work to reach v0.1, longer if part time. The Phases section breaks that down. The phases after v0.1, which bring the rest of Quickshell, aren't estimated yet. Treat it all as a guess, not a plan.

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
|          | pointer, keys            v                                             |
|          | and text    +--------------------------+                               |
|          |             | crabshell-render         |                               |
|          |             | tiny-skia into shm       |                               |
|          |             | repaints dirty areas     |                               |
|          |             +--------------------------+                               |
|          |                          | buffers and damage                          |
|          |                          v                                             |
|          |             +--------------------------+                               |
|          +------------ | crabshell-wayland        |                               |
|                        | calloop, layer-shell,    |                               |
|                        | windows, input, frames   |                               |
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

The template's `Cargo.toml` compiles dependencies once with full optimization and the config itself at `opt-level = 1`, so edits rebuild fast and the shell still runs fast.

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

The whole v0.1 bar as a user writes it:

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

API rules settled in Phase 7, windows and input:

- A property that the widget itself writes back takes a `Signal`, not a value or a closure: `TextInput::new(text)`, `ScrollView::offset` and `ListView::offset`. Typing and scrolling write the signal, and setting it changes what shows. Every other property still takes a value or a closure.
- `cx.floating(FloatingWindow::new()...)` comes from `ShellExt`. The window lives as long as the calling context, and its `visible` shows and hides it meanwhile. Without an `on_close`, a close from the compositor hides it until `visible` turns false and true again.
- `PanelWindow` takes `margins`, `keyboard_focus` (`None`, `Exclusive` or `OnDemand`), `namespace`, `mask` (a `Region`) and `on_key`. A namespace is read once, when the window is shown.
- Keys go to the focused text field, then the focused button's `on_key`, then the window's `on_key`; a handler returns `true` when it used the key. Then Tab and Shift+Tab move the focus between buttons and fields, and Enter or Space clicks the focused button. `Button` also gets `focus_style` and `on_press`, which runs while the pointer button is held, the time for `cx.start_move()` and `cx.start_resize(edges)` in a floating window.
- `Style` gets `clip`, `width` and `height`, and `Image` takes a file with `width`, `height` and `fit` (`Fill`, `Contain` or `Cover`). `PopupWindow` takes `edges`, `gravity` and `adjustment`, read when it opens.
- The prelude adds `KeyboardFocus`, `Margins`, `Region`, `Key`, `KeyEvent`, `Modifiers`, `Image`, `Fit`, `ScrollView`, `ListView`, `TextInput`, `FloatingWindow` and `Adjust`. The launcher example shows them together.

API rules settled in Phase 8, animation and effects:

- `cx.animate(target, transition)` returns a `ReadSignal` that follows the closure `target`, moving to each new value over the transition instead of jumping. It starts at the first value. The value implements `Animate`, as `f32`, `Point` and `Color` do.
- `Transition::new(ms).easing(Easing::InOutCubic)` is a `const` value. `Easing` has CSS's curves and a `Bezier(x1, y1, x2, y2)`, and the default is `OutCubic`.
- `Style` gets `transition`, `gradient` and `shadow`, all `const`. `Gradient::linear(angle, from, to)` or `Gradient::radial(from, to)` takes up to two more colors with `.stop(at, color)`. `Shadow::new(x, y, blur, color)` takes `.spread(n)`.
- A style change fades in over the new style's `transition`: background, gradient, shadow, color, border, radius and opacity. Other fields change at once.
- `PanelWindow::blur` and `FloatingWindow::blur` take an `Option<Region>` for the compositor to blur behind.
- The prelude adds `Animate`, `Easing`, `Transition`, `Gradient`, `GradientShape` and `Shadow`. The effects example shows them together.

API rules settled in Phase 9, desktop services:

- Each new service comes from `ServicesExt` as the v0.1 ones do: `cx.upower()`, `cx.power_profiles()`, `cx.bluetooth()`, `cx.network()`, `cx.mpris()` and `cx.notifications()`. Each hands out `ReadSignal`s of plain structs, such as `Vec<BluetoothDevice>`, and methods that take plain values, such as a device's path or a player's bus name.
- `cx.audio()` also gives `sinks()`, `sources()`, `default_source()` and `streams()`, with setters for defaults, volumes, mutes and moving a stream.
- `cx.idle_monitor(timeout)` comes from `ShellExt` and gives a `ReadSignal<bool>`, `true` while the user has been away for `timeout`. `PanelWindow::inhibit_idle` and `FloatingWindow::inhibit_idle` take a value or a closure.
- `Player::position_at(now)` works out where playback is, since MPRIS doesn't say as it moves.
- The prelude adds the services' types: `UPower`, `PowerDevice`, `DeviceKind`, `BatteryState`, `PowerProfiles`, `PowerProfile`, `Bluetooth`, `BluetoothAdapter`, `BluetoothDevice`, `Network`, `NetworkDevice`, `NetworkKind`, `NetworkState`, `WifiNetwork`, `Mpris`, `Player`, `PlaybackStatus`, `LoopStatus`, `Notifications`, `Notification`, `Urgency`, `Timeout`, `AudioNode` and `AudioStream`. The desktop example shows them together.

## Process model and reload

Two processes run. The `crabshell` CLI, installed once per machine, is the supervisor. The shell is the user's own compiled binary, which links the `crabshell` library.

v0.1 has three CLI commands:

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

Rules settled while building `crabshell-cli` (Phase 5, step 2):

- The config lives in `$XDG_CONFIG_HOME/crabshell` and builds in `$XDG_CACHE_HOME/crabshell/target`, falling back to `~/.config` and `~/.cache`. A relative path in either variable is ignored, as the XDG spec says.
- `crabshell init` refuses if the directory exists at all, even empty, so it never overwrites a config. It creates the directory with a call that fails if it appeared meanwhile, and removes a half-written config if a write fails, so a retry isn't blocked. Without `--path` the config depends on `crabshell` from crates.io. `crabshell init --path DIR` builds against the Crabshell checkout at `DIR` instead. A `DIR` without `crates/crabshell/Cargo.toml` is refused.
- The template in `crates/crabshell-cli/template/` is "Writing a config"'s two blocks word for word, and a test compares them, so the doc and the template can't drift apart.
- `crabshell check` runs `cargo check` on the config from the config's own directory, so another project's `.cargo/config.toml` or `rust-toolchain.toml` doesn't apply, with the cache's target directory. Its output passes straight through, and it exits with cargo's status. `CARGO`, when set, names the cargo to run.
- A bad command line, including an argument that isn't UTF-8, prints the usage and exits with 2. A command that fails explains why and exits with 1. A `--path` that isn't UTF-8 is refused, and control characters in it are escaped in the TOML.

Rules settled while building `crabshell run` (Phase 5, step 3):

- The supervisor runs on one thread, on a calloop loop like the shell's. It learns the shell exited from a pidfd (`rustix`'s `pidfd_open`, so Linux 5.3 or later), which sleeps until then instead of polling. A shell that is up and quiet wakes the supervisor once, when it turns 60 s old.
- The build's program is the one `executable` in cargo's `--message-format=json` output, so a config can name its package anything. A config that builds more than one program is refused.
- The control socket is `$XDG_RUNTIME_DIR/crabshell/control-<supervisor pid>.sock`, in a directory made private the way `persist::write` does it. It is removed when `run` ends. Without `XDG_RUNTIME_DIR`, `run` refuses to start. The supervisor logs `ready` with the milliseconds since the shell started, and doesn't wait for it.
- Exit status 0 or 2 stops `run` with 0, since 2 means the session is gone. Any other end restarts the shell. A program that can't be started counts as a crash.
- A crash counts toward the fallback only within 60 s of the newest build's first start. Starting `last-good` resets the wait to 1 s. `last-good` that keeps crashing restarts with the same growing wait. Only a build is ever copied to `last-good`, from the running shell's `/proc/<pid>/exe`, so it is the program that ran even if a newer build replaced the file. The copy goes through `last-good.partial` and a rename.
- If the first build fails, `run` starts `last-good` when there is one, and otherwise exits with 1.
- SIGINT and SIGTERM end the shell with SIGTERM, then `run` with 0. Waiting to restart, `run` ends at once. signalfd needs the two signals blocked, and a child inherits that mask, so they are unblocked while the shell spawns.
- Logs go to stderr with a `crabshell:` prefix. The shell's stdout and stderr pass straight through.

Rules settled while building the watcher and reload (Phase 5, step 4):

- The watcher starts before the first build, so an edit saved during it isn't lost. It sees `Cargo.toml` in the config directory and every file under `src/`, including directories made later and what a symlink there points at. A directory moved out of `src/` stops being watched, and a lost-events overflow rescans `src/`. Other files in the config directory, such as `Cargo.lock`, are ignored, and so are hidden files, `~` backups, Emacs's `#name#` auto-saves and Vim's `4913` probe. A build starts 300 ms after the last change.
- Cargo's progress goes to stderr as it builds. The compiler's warnings and errors print when it ends, and the notification's body is the first error's message, such as `this file contains an unclosed delimiter`. A build that fails before the compiler runs says `cargo failed; its message is above`.
- A build whose program cargo calls fresh, at the same path as the newest build, changes nothing, so the shell isn't reloaded. That holds while `last-good` runs too: the fresh build is the one that kept crashing.
- To reload, the supervisor writes `save` to the shell and starts the newest build when the shell exits, whatever its status. A shell that never connected gets SIGTERM instead, and one still up 5 s after the first `save` gets SIGKILL; an edit meanwhile only rebuilds. A shell waiting to restart after a crash is replaced at once. SIGTERM to the supervisor during a reload ends the old shell and starts nothing.
- A new build starts over: no crashes counted, and it replaces `last-good` if that was running.
- Each notification opens its own session-bus connection, which closes after the call, so an idle supervisor stays asleep. A server gets 25 s to answer, timed by a loop timer. A failure, such as no notification server, is logged. The body escapes `&`, `<` and `>`, since servers read it as markup. The supervisor notifies when an edit doesn't build, when the first build fails and `last-good` starts, and when it falls back to `last-good`.
- The supervisor logs each build's duration, and the time from the old shell's exit to the new one's `ready`.
- A build blocks the loop. A signal that arrives meanwhile is handled when it ends.
- Every crate's tests run their buses on a private `dbus-daemon` whose config lists no service directories, since `--session` would start real services, such as the desktop's notification daemon. `crabshell-cli`'s other tests get a session bus address that leads nowhere, and none of its tests passes on the desktop's `DISPLAY`, `XAUTHORITY`, `WAYLAND_DISPLAY` or `SWAYSOCK`: libpulse alone reads X's root window, which would start the desktop's Xwayland.

Panic handling inside the shell:

- Builds use `panic = "unwind"`. The toolkit wraps every binding, effect, event handler and service callback in `catch_unwind`.
- A panicking binding is disabled and logged with its widget path, for example `PanelWindow[DP-1]/Row/Button[2]/Text.text`. The rest of the shell keeps running.
- The reactive core releases every `RefCell` borrow before it calls user code, so a caught panic never leaves the runtime borrowed. A reactive-core test checks this.
- A panic in the toolkit's own Wayland or rendering code is not caught. The process exits and the supervisor restarts it.

Logging goes through `tracing` to stderr. The supervisor passes the shell's stderr through to its own, so `journalctl` sees both when `crabshell run` runs as a systemd user service.

The blink during a reload lasts as long as shell startup, which targets 100 ms. Starting the new shell before stopping the old one would remove the blink. But both bars would reserve screen space for a moment and every window would jump, so that handoff waits until after v0.1.

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

v0.1 shipped ten widgets, Phase 7 added five, and the phases after it add the rest a full shell needs (see "Beyond v0.1"). All but one are in `crabshell-ui`. `Tray` is in `crabshell-services`, because it needs the tray service and `crabshell-ui` can't depend on services; the prelude re-exports it. Users build everything else by combining them.

| Widget           | What it does                                                                                                                |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `PanelWindow`    | A layer-shell surface on one output with anchors, size, exclusive zone, layer, margins, keyboard focus and an input mask.   |
| `FloatingWindow` | An ordinary `xdg_toplevel` window with a title, size limits, maximized, fullscreen and `visible`, shown with `cx.floating`. |
| `PopupWindow`    | An `xdg_popup` anchored to a widget's rectangle. Tray menus use it.                                                         |
| `Row`, `Column`  | Flex layout with `gap`, `padding`, `align` and `justify`.                                                                   |
| `Stack`          | Children drawn on top of each other in one cell.                                                                            |
| `Spacer`         | Takes the leftover space in a `Row` or `Column`.                                                                            |
| `Text`           | One line by default, with `ellipsis` and `max_lines`.                                                                       |
| `Icon`           | A freedesktop icon name, an SVG or PNG path, or raw ARGB pixels from a tray item.                                           |
| `Button`         | A clickable box with `hover_style`, `pressed_style` and `focus_style`, plus click, press, scroll and key handlers.          |
| `Image`          | A PNG, JPEG, GIF, WebP or SVG file at a fixed size, filled, contained or covered.                                           |
| `ScrollView`     | Shows part of a taller child, cut to its box, scrolled by the wheel or by its offset signal.                                |
| `ListView`       | A scrolling list of rows of one height that builds only the rows in view, with a `current` row kept in view.                |
| `TextInput`      | A one-line text field with a caret, selection, placeholder, password dots, the clipboard and input methods.                 |
| `Menu`           | A vertical list of buttons built from a DBusMenu layout, with separators, checkmarks and submenus as nested popups.         |
| `Tray`           | One icon button per tray item, each opening its `Menu`. Built only from the widgets above.                                  |

### Layout

- `taffy` computes flexbox layout. `Row` and `Column` map to a flex direction, and `Stack` maps to a one-cell grid.
- `Text` and `Icon` report their size through taffy's measure function. Text measuring uses the same `cosmic-text` buffer the renderer draws from, so measured and drawn sizes can't disagree.
- Every unit is a logical pixel. The renderer multiplies by the output's scale.
- A `PanelWindow` anchored left and right gets its width from the compositor's `configure` event. Layout runs again after every configure.

### Invalidation

- Each property is either a layout property or a paint property. Text content, font, padding, gap, size and children are layout. Colors, background, gradient, border, opacity and radius are paint. A shadow reaches past its widget, so changing one lays the window out again to find the area it paints.
- A binding that changes a paint property marks only its own widget for repaint. A layout change marks the whole window for layout, then every widget whose rectangle moved is repainted at both its old and new position.
- When nothing is dirty there is no layout, no paint and no commit.

### Styles

- `Style` has `background`, `gradient`, `shadow`, `color`, `font` (family, size, weight), `padding`, `radius`, `border` (width, color), `opacity`, `clip`, `width`, `height` and `transition`. Every field is optional, and an unset field falls back to the widget's default. `clip`, `width` and `height` are layout properties.
- `Text` inherits `color` and `font` from the nearest ancestor that sets them. Nothing else inherits, and there are no selectors or classes.
- Colors are sRGB with alpha. `Color::rgb`, `Color::rgba` and `Color::hex` are all `const fn`.

### Input

- Pointer events come from `wl_pointer`. Hit testing walks the tree from the top and picks the deepest `Button` under the pointer.
- The cursor shape is set through `wp_cursor_shape_v1`, with a pointer cursor over buttons.
- `on_scroll` receives whole steps. Mouse wheels use `axis_value120`. Touchpad scrolling is summed and becomes one step per 15 logical pixels.
- Keyboard input comes from `wl_keyboard`, with sctk reading the keymap through libxkbcommon. A window gets keys only with a keyboard focus other than `None`.

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
- The window's child is exactly the window's size. Content that doesn't fit overflows and is not clipped, as in CSS, unless a style sets `clip` or the widget is a scroll view or text field. Damage covers everything a widget paints, including text lines taller than their box, so the renderer clips only where those say.
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

Rules settled while building `crabshell-ui` (Phase 7):

- A `Region` joins rectangles, rounded rectangles and ellipses with union, subtract, intersect and xor, in call order, and is cut into rectangles at each row's center for `wl_region`.
- The focus moves only by Tab, a click on a field, or a field's `focus` turning true; a click on a button doesn't take it. The focus shows only while the window has the keyboard. Removing the focused widget drops the focus.
- A focus style that moves buttons checks the hover again after the layout.
- A clip cuts its children to the inside of its border, following the corners, and cuts their bounds to its rectangle. A clip that comes, goes or changes shape repaints its old and new bounds, and the pointer can't reach children under its border.
- A scroll view lays its child out at full height, never shrunk, and shows it moved up by the offset clamped to the content. The wheel scrolls 48 logical pixels a notch, the touchpad by its pixels, starting from the signal so scrolls before the next layout add up. A button with `on_scroll` under the pointer takes the scroll instead.
- A list builds the rows in view plus one each side, reached through a one-item keyed list for its context. A keyed item whose builder panics is logged and shows empty, keeping its place.
- A text field edits by grapheme and word through unicode-segmentation, places its caret and selection with cosmic-text so right-to-left text works, scrolls sideways to keep the caret in view, and is cut to its content box. Text set from outside puts the caret at the end, and line breaks become spaces. Ctrl+C and Ctrl+X copy the selection, Ctrl+V pastes, and a password field pastes but never copies. Enter runs `on_accept`, or without one goes on to the window.
- An input method sees the focused field only while the window has the keyboard and never a password. Its preedit shows underlined at the caret, replacing the selection, and moving the focus drops it.
- A floating window's settings are read after the first flush, like a panel's. Move and resize requests are honored only while a pointer button is held.

Rules settled while building `crabshell-ui` (Phase 8):

- One clock on the UI thread drives every animation, and the platform advances it only while one runs. An animation's first tick is its time 0. One that replaces a running one counts from the clock's last tick, so it moves at the next tick, and the first animation after a rest counts from its own first tick. A transition of 0 ms sets the value in the same flush.
- A new target while a value moves starts a new animation from where the value is. An animation whose value's owner is gone stops. One that panics is logged as `animation cx.animate at <file:line>` or `<widget path>.transition` and stops, and finished ones are dropped inside `catch_unwind` too.
- A style fades only on a paint change, on a widget already laid out, when the new style's transition is longer than 0. It blends from what showed, so leaving a hover mid-fade fades back from there. A color only one side sets fades from or to its transparent self, a border from or to 0 wide, a radius from 0 and an opacity from 1. A text color only one side sets, and gradients of different shapes or numbers of colors, change at once. A fading widget counts as drawing, so a layout that moves it repaints both places.
- A shadow's painted area is its shape grown by 1.5 blurs, past which the blur fades below a 255th. A fading shadow's area covers its start and its end, and a spread that swallows the box still reaches between the edges it would have. A shadow with a number that isn't finite is never drawn.
- `Gradient::stop` panics past four colors, which is a compile error in a `const`.

## Rendering

v0.1 draws on the CPU with `tiny-skia`, straight into shared-memory buffers, and repaints only what changed. For a bar that uses less memory than a GPU context. A 1920×30 bar buffer is 225 KiB.

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
- Nothing dirty means no commit, so an idle shell never wakes up to draw.

### Scale

- Use `wp_fractional_scale_v1` with `wp_viewporter` when the compositor offers them. Otherwise fall back to integer `wl_surface.set_buffer_scale`.

### Text

- `cosmic-text` shapes and lays out text, and rasterizes glyphs through `swash` with its own glyph cache.
- Don't scan every system font at startup. On a machine with thousands of font files, that alone could miss the 100 ms startup target. Resolve the families the config names through fontconfig and load only those files into `cosmic-text`'s font database. Missing glyphs use fontconfig's fallback list. In Phase 0, fontconfig's startup took about 15 of the 17.7 ms to first frame, and its cache files held about 4 MiB of memory.
- Glyphs are drawn as grayscale alpha masks. There is no subpixel antialiasing in v0.1.

### Icons and images

- Icon names resolve through the freedesktop icon theme spec with an in-house lookup. The config picks a theme with `cx.set_icon_theme("...")`, and the default is `hicolor`.
- `resvg` renders SVG icons. It already draws with tiny-skia, so it renders into the same pixmap. PNGs load with the `png` crate.
- Rasterized icons are cached by name, size and scale.
- `Image` files load by extension: PNG through `png`, JPEG through `zune-jpeg`, the first frame of a GIF through `gif`, WebP through `image-webp`, and SVG through `resvg`. They are halved, then scaled, to the size drawn, and kept in the icon cache.

### Shapes

Solid fills, gradients, rounded rectangles, borders and shadows. Blur behind a window is the compositor's, through `ext-background-effect-v1`. v0.1 had no gradients, shadows or blur; Phase 8 added them.

### Later GPU backend

Keep every drawing call inside `crabshell-render`, behind plain functions. A GPU backend using `vello` on `wgpu` replaces that crate's internals later. Don't add a renderer trait until a second backend exists.

Phase 8 checked whether it was needed yet. While the effects bar's indicator slides and a tab's hover fades in, a frame takes 0.31 ms to draw at scale 1 and 0.54 ms at scale 2, release build, on a 1920 pixel wide bar. That is under 4% of a 60 Hz frame, so the CPU renderer stays.

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

Rules settled while building `crabshell-render` (Phase 7):

- A clip draws the children into a layer blended in through a mask of the inside of the border, filled in the tile's own pixels on the fixed grid, so a repaint cuts every edge as a full draw does. A text field draws its selection, text and caret the same way, through its content box.
- Limits on image files: 32 MiB, 8192 pixels a side and 24 million pixels, told to each decoder before it allocates; SVG images up to 4 MiB. A GIF frame with no pixels is refused. Cached pictures past 32 MiB, like icons past 128, are dropped once no recent draw used them. The cache key holds the file's modification time, so a file rewritten in place, or one that appears after a failure, is read again.
- An image is placed and sized apart, so one past the window's edge is cut there instead of squeezed.

Rules settled while building `crabshell-render` (Phase 8):

- A gradient is laid out in buffer pixels and moved onto each tile, so tiles agree. A linear one takes CSS's angle, with its line long enough for the ends to meet the corners. A radial one is a circle from the middle to the farthest corner. A clear stop fades each side out in its neighbor's color, as CSS blends with the alpha applied. Nothing is dithered, so a subtle gradient across a wide bar shows steps.
- A shadow is drawn under its box, so a translucent background shows it. A sharp one is a filled shape. A blurred one is a Gaussian of half the blur, worked out for each pixel in buffer pixels: the product of the error function on each axis away from the corners, and eight Gaussian-weighted rows near one. A corner smaller than the spread grows by less, as in CSS, so a square box keeps square corners.
- A full draw of an 1880 pixel wide box with a 12 pixel blur at scale 2 takes 5.1 ms, 0.8 ms of it without the shadow.

## Wayland platform

`crabshell-wayland` uses `smithay-client-toolkit` (sctk) on a single-threaded `calloop` event loop. Everything that touches widgets or signals runs on that one thread.

| Protocol                                                 | Used for                                         |
| -------------------------------------------------------- | ------------------------------------------------ |
| `wl_compositor`, `wl_shm`                                | Surfaces and CPU buffers                         |
| `wl_output` version 4                                    | Output names such as `DP-1`, plus mode and scale |
| `zwlr_layer_shell_v1`                                    | `PanelWindow`                                    |
| `xdg_wm_base`, through `zwlr_layer_surface_v1.get_popup` | `PopupWindow`                                    |
| `xdg_toplevel`, `zxdg_decoration_manager_v1`             | `FloatingWindow`, with server-side decorations   |
| `wp_fractional_scale_v1`, `wp_viewporter`                | Fractional scaling                               |
| `wp_cursor_shape_v1`                                     | Cursor shapes                                    |
| `wl_seat`, pointer and keyboard                          | Clicks, hover, scroll and keys                   |
| `zwp_text_input_v3`                                      | Input methods in text fields                     |
| `wl_data_device_manager`                                 | The clipboard                                    |

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

- `PanelWindow` defaults to layer `top`, keyboard interactivity `none`, no margins, namespace `crabshell`, and an exclusive zone equal to its height on its anchored edge. Sway adds the margin on the anchored edge to the zone itself, so the default zone leaves it out.
- When the compositor sends `closed`, drop the window.

### Popups

- A `PopupWindow` positions itself with `xdg_positioner`, anchored to the rectangle of the widget that opened it. It flips to the other side when it would leave the output. Its `edges`, `gravity` and `adjustment` replace those defaults.
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

Rules settled while building `crabshell-wayland` (Phase 7):

- On-demand keyboard focus needs layer-shell version 4; before it, a window asking for it takes no keys instead of all of them. A namespace holding a NUL byte is replaced with the default.
- A mask is cut to the surface's size and sent before the commit that applies it, again whenever the size changes.
- sctk repeats a held key with a calloop timer that exists only while the key is down. Text typed is printable only. Shortcuts in another script go by the US letter at that key, so Ctrl+C copies with a Hebrew layout.
- Sway gives an exclusive layer surface the keyboard only if a keyboard exists when it maps, so the tests' virtual keyboard comes first. A virtual keyboard sends its own modifiers.
- sctk tells `new_seat` only about seats that come later, so the seats at startup get their text input and data device in `Platform::new`.
- A text input is enabled again for each field the focus moves to, told at most 4000 bytes around the caret, and sends only what changed. Sway ignores an input method's commit whose serial isn't its latest `done`.
- The clipboard offers five text types and moves text through pipes the loop watches only while text is moving, 4 KiB at a time, so a slow program never blocks the shell. A paste reads at most 1 MiB, and an empty one pastes nothing.
- `XdgShell` is bound with the decoration manager. A floating window asks for server-side decorations, starts at scale 1 until the compositor says otherwise, and is told the size it asks for when a configure leaves the size to it. Size limits are capped at 16384 and a maximum below the minimum is raised to it, since either would end the connection. A hidden floating window drops its toplevel and its popups.
- A virtual pointer's clicks never reached a toplevel on headless Sway, so the floating window tests click with `swaymsg seat - cursor`.

Rules settled while building `crabshell-wayland` (Phase 8):

- The animation clock ticks from a calloop `Timer` that is in the loop only while something animates, once a frame of the fastest output, or at 60 Hz without one.
- Blur binds `ext_background_effect_manager_v1` version 1. A surface gets its effect object the first time it asks, and the region is cut to the surface's size and sent before the commit that applies it, and again when the size changes, as the mask is. Without the protocol, or with the compositor's blur off, a window that asks is logged once per shell and shows unblurred. Blur that turns on later shows from each window's next frame. No compositor here has the protocol (SwayFX 0.6, wlroots 0.20, labwc 0.20), so only the path without it is tested.
- `Stats::drawing` adds up the time spent drawing frames.

Rules settled while building `crabshell-wayland` (Phase 9):

- Idle binds `ext_idle_notifier_v1` and `zwp_idle_inhibit_manager_v1`, both version 1. A watch is a notification on the first seat, so a watch made before any seat exists never fires, and one whose seat goes stays as it was. A panicking callback is logged and the shell goes on.
- A window's idle inhibitor is made once its surface is configured, while its `inhibit_idle` is true, and goes with the surface. Headless Sway honors one on a layer surface.

## Services

v0.1 had four services in `crabshell-services`, and Phase 9 added six; the phases after it add Quickshell's others (see "Beyond v0.1"). Each one owns its connection, runs on the main thread, and hands out `ReadSignal`s plus a few methods. Every service is created on first use and shared by all bars, so two outputs never mean two connections.

| Service        | Signals and methods                                                                                                                                                                                                                                                                               | Source                                                              | Crate                         |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------- | ----------------------------- |
| Workspaces     | `workspaces()` gives `Vec<Workspace>` with id, name, output, focused, visible, urgent. `focus_workspace(name)`.                                                                                                                                                                                   | Sway IPC at `$SWAYSOCK`                                             | Own framing with `serde_json` |
| Tray           | `items()` gives `Vec<TrayItem>` with id (bus name and path), app_id, title, status, icon_name, icon_pixmaps, attention_icon_name, icon_theme_path, tooltip, item_is_menu and menu. `activate`, `secondary_activate`, `scroll`, `menu()`, `open_menu`, `open_submenu`, `close_menu`, `menu_event`. | StatusNotifierItem and `com.canonical.dbusmenu` on the session bus  | `zbus`                        |
| Audio          | `default_sink()` gives `Option<Sink>` with name, volume_percent, muted. `change_volume(percent)`, `toggle_mute()`.                                                                                                                                                                                | PulseAudio protocol, served by pipewire-pulse on PipeWire systems   | `libpulse-binding`            |
| Clock          | `clock(precision)` gives a `jiff::Zoned`                                                                                                                                                                                                                                                          | System time and time zone                                           | `jiff`                        |
| UPower         | `devices()`, `display_device()` give `PowerDevice`s with kind, state, percent, present, times to empty and full, energy rate. `on_battery()`.                                                                                                                                                     | UPower on the system bus                                            | `zbus`                        |
| Power profiles | `active()`, `available()` give `PowerProfile`s. `set(profile)`.                                                                                                                                                                                                                                   | power-profiles-daemon on the system bus                             | `zbus`                        |
| Bluetooth      | `adapters()`, `devices()` with name, address, powered, discovering, connected, paired, trusted, battery. `set_powered`, `start_discovery`, `stop_discovery`, `connect`, `disconnect`, `pair`, `set_trusted`, `forget`.                                                                            | BlueZ on the system bus                                             | `zbus`                        |
| Network        | `wifi_enabled()`, `devices()` with kind, state and connection, `wifi_networks()` with SSID, strength, secured, connected. `set_wifi_enabled`, `scan`, `connect`, `disconnect`.                                                                                                                    | NetworkManager on the system bus                                    | `zbus`                        |
| Media players  | `players()` gives `Player`s with status, track, artists, art, length, position and what each can do. `play_pause`, `next`, `previous`, `seek`, `set_position`, `set_volume`, `set_shuffle`, `set_loop`, `raise`.                                                                                  | MPRIS on the session bus                                            | `zbus`                        |
| Notifications  | `list()`, the history, and `popups()`, the ones still showing. `dismiss`, `invoke`, `clear`.                                                                                                                                                                                                      | The shell serves `org.freedesktop.Notifications` on the session bus | `zbus`                        |

### Workspaces

- Write the IPC framing in-house. Each message is a 14-byte header (`i3-ipc`, a length, a type) plus JSON, about 150 lines of code, and owning it avoids pulling in another async runtime.
- On start, send `SUBSCRIBE` to `workspace` events, then `GET_WORKSPACES`, so no change between the two goes unseen. On every event, send `GET_WORKSPACES` again and replace the whole list. That is simpler than applying each event's changes and costs one small JSON reply.
- If the socket closes, set the list to empty and reconnect every 2 s. A first connect that fails, as with a stale `SWAYSOCK` under another compositor, logs once and never retries, so it never wakes the bar.
- Other wlroots compositors don't speak Sway IPC. Before Phase 3, check which of your target compositors implement `ext-workspace-v1`. If none do, v0.1 shows workspaces on Sway only, and other compositors get the bar without them. Checked on 2026-11-11: labwc has it since 0.8.3, Sway and Wayfire only on their `master` branches, and river not at all. So v0.1 shows workspaces on Sway only, and an `ext-workspace-v1` backend waits until after v0.1.

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
- `change_volume` clamps to 0-100% in v0.1.
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

Rules settled while building `crabshell-services` (Phase 9):

- System daemons are followed by one driver: read whole when the daemon has an owner, kept up to date from its broadcasts, emptied when it leaves and read again when it returns. Only broadcasts from the daemon's current unique name count, since any program can aim a signal at the shell. A signal that arrives during a read marks it stale, and it reads again when done, so a change is never lost or overwritten. A signal that names an object without its properties, as UPower's `DeviceAdded`, reads everything again.
- A daemon that broadcasts much the shell doesn't read gets narrower match rules, so it never wakes the shell for it: BlueZ and NetworkManager take only ObjectManager signals and `PropertiesChanged` for the interfaces read. A change to a property nothing reads is applied without rebuilding the lists.
- zbus stops reading a connection while one message stream holds 64 unread messages, so nothing awaits inside a message loop, and a service's start awaits nothing per object.
- Caps apply when a list is shown, not when an object arrives, so one left out shows once there is room. Bluetooth shows connected and paired devices first. At most 4096 objects are kept, and a flood past that is logged and left out.
- UPower: the display device and every device from `EnumerateDevices`, at most 64. Times of 0 are unknown; a percent that isn't finite reads as 0.
- Power profiles: read from `net.hadess.PowerProfiles`, which every version of power-profiles-daemon still owns. A refused `set`, as polkit may refuse, is logged.
- Bluetooth: read with `GetManagedObjects` at `/`, keeping `Adapter1`, `Device1` and `Battery1`. A device's name is its `Alias`, else its `Name`, else its address. `Pair` and `Connect` may take 90 s, since BlueZ answers a pairing once the user confirmed it; other calls 25 s. The shell registers no pairing agent.
- Network: read with `GetManagedObjects` at `/org/freedesktop`. Loopback and unmanaged devices are left out. One network per SSID and device, from its strongest access point, strongest first; hidden SSIDs are left out, and SSIDs are cut to 32 bytes and shown as lossy UTF-8 while `connect` sends the access point's own bytes. A password connection sends only the key and lets NetworkManager pick WPA2 or WPA3 from the access point; without one, NetworkManager picks a saved connection, and an open network with none gets a new one. Enhanced Open counts as open. A password is never logged or kept.
- Media players: every `org.mpris.MediaPlayer2.*` name but playerctld, which mirrors another player, at most 32, in the order they appeared. The position comes from a read when a player appears and when its status, rate or track changes, and from `Seeked`; until such a read answers, a new status or rate takes effect from where playback got to. Nothing polls.
- Notifications: the shell queues for `org.freedesktop.Notifications` and lets another daemon replace it, serving while it holds the name and keeping the history through a loss. It offers actions, body, icon-static and persistence. Tags in a body are taken out and entities decoded. A popup shows for its timeout, 5 s by default, and a critical one or one that never expires until dismissed; one loop timer exists only while a popup waits to expire. Summaries are cut to 1 KiB, bodies to 4 KiB and other strings to 256 bytes; at most 16 actions, 256 notifications and 8 MiB of pictures are kept.
- Audio: sinks, sources and streams are read whole on each of their events. A failed read keeps the last list. A sink's monitor isn't listed as a source, but can be the default source. Setting a volume keeps the channels' balance. Peak level meters aren't there yet: they would need a stream reading the sound many times a second.

Rules settled while building the `Tray` widget (Phase 4, step 5):

- `Tray::new()` takes `icon_size` (16 by default) and `menu_style`, the menu popup's style (light text on dark gray by default). Both take a value or a closure.
- It is built from `crabshell-ui`'s widgets. `cx.tray()` needs a context, which a keyed child's view gets, so the tray is one such child holding a row of the items. `Prop::shared` lets one property feed many widgets.
- A click opens the menu popup at once, so it grabs the pointer with the click's serial. The popup is clear and empty until the app's layout arrives. It closes once the tray stops following its opening: the app's menu went, a read failed (the first one too), or the app left. `menu_style` is the `Menu`'s style, so submenus get it too.
- When the popup goes, the tray stops following the app's menu, unless the menu was opened again since. `open_menu` numbers each opening for that.
- A widget doesn't know its output's scale, so pixmaps are picked for twice the icon size. An item with no icon shows `image-missing`.

## Crates

One Cargo workspace with seven crates. Each crate owns one area, so an agent can work inside one without touching the others.

| Crate                | Owns                                                                                        | Depends on           | Main external crates                                                        |
| -------------------- | ------------------------------------------------------------------------------------------- | -------------------- | --------------------------------------------------------------------------- |
| `crabshell-reactive` | Signals, memos, effects, owners, flush                                                      | none                 | `slotmap`                                                                   |
| `crabshell-ui`       | `Cx`, widgets, styles, layout, text layout, hit testing, focus, invalidation                | reactive             | `taffy`, `cosmic-text`, `fontconfig`, `unicode-segmentation`                |
| `crabshell-render`   | Drawing into pixmaps, damage tracking, glyphs, icons, images                                | ui                   | `tiny-skia`, `resvg`, `png`, `swash`, `zune-jpeg`, `gif`, `image-webp`      |
| `crabshell-wayland`  | Event loop, windows, popups, outputs, pointer, keyboard, text input, clipboard, shm buffers | reactive, ui, render | `smithay-client-toolkit` (with `xkbcommon`), `calloop`, `wayland-protocols` |
| `crabshell-services` | Workspaces, tray, audio, clock, power, Bluetooth, network, media players, notifications     | reactive, ui         | `zbus`, `serde_json`, `libpulse-binding`, `jiff`, `calloop`, `serde`        |
| `crabshell`          | Public API, `run()`, `prelude`, `cx.persist`                                                | all of the above     | `serde`, `serde_json`, tracing-subscriber                                   |
| `crabshell-cli`      | The `crabshell` binary with `init`, `run`, `check` and the supervisor                       | none                 | `calloop`, `rustix`, `serde_json`, `inotify`, `zbus`                        |

`Cx` lives in `crabshell-ui` and carries a map of services keyed by type. `crabshell-services` adds `cx.sway()`, `cx.audio()`, `cx.tray()`, `cx.clock()` and Phase 9's services through an extension trait, so `crabshell-ui` never depends on services.

### Dependency policy

- Pin every external version once, in the workspace `Cargo.toml` under `[workspace.dependencies]`.
- Add a crate only when it saves more than about 200 lines or implements a protocol or file format. Add it to the table above first.
- Rust crates must be MIT, Apache-2.0, BSD, ISC, Zlib or Unicode licensed. `cargo deny check licenses` enforces this with `deny.toml`, in CI on every push.
- System C libraries (libpulse, fontconfig, libxkbcommon) are linked dynamically. sctk speaks the Wayland protocol in pure Rust, so libwayland-client isn't linked.
- Use edition 2024 on the latest stable toolchain. No nightly features.

## Performance targets

Every target is a guess until Phase 0 measures it. The baseline is Quickshell running the same bar, with workspaces, tray, volume and clock, on the same machine and output. Use Quickshell's `Quickshell.I3` module for workspaces, and check that it connects to Sway's socket. Don't compare against numbers found online.

| Metric                                          | Target                                                     | v0.1, measured                                          | How to measure                                                                  |
| ----------------------------------------------- | ---------------------------------------------------------- | ------------------------------------------------------- | ------------------------------------------------------------------------------- |
| Idle memory (RSS), v0.1 bar on one 1080p output | 30 MB or less, and at most a quarter of Quickshell's       | 18.7 MiB, 15% of Quickshell's 125.2 MiB                 | `ps -o rss= -p <pid>` after 60 s idle                                           |
| Idle wakeups                                    | 1 per minute with a minute clock and nothing else changing | 2 context switches at the minute boundary, none between | `perf stat -e 'syscalls:sys_exit_epoll_wait' -p <pid> -- sleep 300`             |
| Startup to first frame                          | 100 ms or less                                             | 10.8 ms                                                 | The shell logs the time from `main` to its first committed buffer               |
| Workspace switch to screen                      | Within one frame, 16 ms at 60 Hz                           | median 0.5 ms, slowest 0.6 ms                           | The shell logs the time from the IPC event to the commit                        |
| Rebuild after a one-line edit                   | 10 s or less                                               | 0.7 s                                                   | The supervisor logs each build's duration                                       |
| Reload gap with nothing on screen               | 200 ms or less                                             | 18 ms                                                   | The supervisor logs the time from the old shell's exit to the new one's `ready` |

Measured on 2026-11-13 for v0.1, on headless Sway. The first four rows are `examples/bar.rs` with workspaces, the tray, the volume and the minute clock, release build on one 1920×1080 output at scale 1, from `measure_the_counter_bar`. The counter example, with no services, used 15.8 MiB and woke 0 times. The last two rows are the template bar under `crabshell run` on one 1280×720 output, from the Phase 5 exit check. Each phase's own numbers follow.

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

Measured on 2026-11-11 with the example counter bar, release build. It has no services yet, so Phase 3 measures the full v0.1 bar against Quickshell's 125.2 MiB.

| Metric                       | Target         | Headless Sway, 1920×1080, scale 1 | Laptop eDP-1 under SwayFX, 1920×1080, scale 1 |
| ---------------------------- | -------------- | --------------------------------- | --------------------------------------------- |
| Startup to first frame       | 100 ms or less | 8.6 ms                            | 13.0 ms                                       |
| Idle memory (RSS) after 60 s | 30 MB or less  | 13.6 MiB                          | 13.2 MiB                                      |
| Idle wakeups with no clock   | 0              | 0 context switches in 60 s        | 0 context switches in 62 s                    |

The laptop run used the build from before the step 4 review fixes.

### Phase 3 results

Measured on 2026-11-11 with `examples/bar.rs`: workspaces, the minute clock and the volume button, with no tray. Release build on headless Sway, 1920×1080 at scale 1, with audio connected to a private pipewire-pulse.

| Metric                       | Target                                           | v0.1 bar, headless Sway                                  |
| ---------------------------- | ------------------------------------------------ | -------------------------------------------------------- |
| Startup to first frame       | 100 ms or less                                   | 14.2 ms                                                  |
| Idle memory (RSS) after 60 s | 30 MB or less, at most a quarter of Quickshell's | 17.9 MiB, 14% of Quickshell's 125.2 MiB                  |
| Idle wakeups                 | 1 per minute with a minute clock                 | 2 context switches at each minute boundary, none between |
| Workspace switch to screen   | within one frame, 16 ms at 60 Hz                 | median 0.6 ms, slowest 0.9 ms                            |

The counter bar, which uses no services, went from 13.4 to 15.5 MiB once `crabshell` linked libpulse. Its first frame didn't change.

### Phase 4 results

Measured on 2026-11-13 with `examples/bar.rs`, now with the tray: workspaces, the tray, the volume button and the minute clock. Release build on headless Sway, 1920×1080 at scale 1, with audio on a private pipewire-pulse and the tray running the watcher on a private session bus. Two runs.

| Metric                       | Target                                           | v0.1 bar, headless Sway                                       |
| ---------------------------- | ------------------------------------------------ | ------------------------------------------------------------- |
| Startup to first frame       | 100 ms or less                                   | 14.3 and 15.8 ms                                              |
| Idle memory (RSS) after 60 s | 30 MB or less, at most a quarter of Quickshell's | 18.7 and 18.8 MiB, 15% of Quickshell's 125.2 MiB              |
| Idle wakeups                 | 1 per minute with a minute clock                 | 2 or 3 context switches at each minute boundary, none between |
| Workspace switch to screen   | within one frame, 16 ms at 60 Hz                 | median 0.5 ms                                                 |

The tray adds about 0.8 MiB. The counter bar, with no tray, stays at 15.4 to 15.5 MiB.

### Phase 5 results

Measured on 2026-11-13 by `crates/crabshell-cli/tests/exit_check.rs`. It makes the template's bar with `crabshell init --path` and runs it under `crabshell run` on one 1280×720 headless Sway output, with a private session bus for the tray and the notifications and no audio server. The config builds with the template's dev profile. These are the ranges over four runs.

| Metric                                          | Target         | Template bar, headless Sway           |
| ----------------------------------------------- | -------------- | ------------------------------------- |
| Rebuild after a one-line edit                   | 10 s or less   | 0.7 s                                 |
| Reload gap, old shell's exit to the new `ready` | 200 ms or less | 13 to 19 ms                           |
| New shell ready after its build ends            | 200 ms or less | 65 to 97 ms                           |
| Back after `kill -9`                            | 2 s or less    | 1.02 to 1.03 s, the 1 s wait included |
| Three startup panics to `last-good` ready       | none           | 4.7 s from the edit                   |
| Supervisor RSS with the shell up                | none           | 5.3 to 5.5 MiB                        |

The time from the build's end to the new shell's `ready` is mostly the old shell saving and exiting: the gap itself is 13 to 19 ms.

### Phase 7 results

Measured on 2026-11-14 with `measure_the_counter_bar`, release builds on one 1920×1080 headless Sway output at scale 1, after all of Phase 7.

| Metric                | Before Phase 7 | After Phase 7 |
| --------------------- | -------------- | ------------- |
| Counter example RSS   | 15.7 MiB       | 16.3 MiB      |
| Counter first frame   | 9.7 ms         | 9.6 ms        |
| v0.1 bar RSS          | 19.0 MiB       | 19.7 MiB      |
| Idle wakeups, counter | 0              | 0             |

Linking libxkbcommon cost about 0.6 MiB; the rest of the phase moved RSS by a few hundred KiB.

### Phase 8 results

Measured on 2026-11-14 with `measure_the_counter_bar`, release builds on one 1920×1080 headless Sway output at scale 1. "Before" is ccc3bf5, built in a second checkout.

| Metric                                     | Before Phase 8 | After Phase 8 |
| ------------------------------------------ | -------------- | ------------- |
| Counter example RSS                        | 16.4 MiB       | 16.5 MiB      |
| Counter first frame                        | 17.4 ms        | 9.5 ms        |
| v0.1 bar RSS                               | 19.8 MiB       | 19.5 MiB      |
| Idle wakeups, counter                      | 0              | 0             |
| Effects bar frame while animating, scale 1 | none           | 0.31 ms       |
| Effects bar frame while animating, scale 2 | none           | 0.54 ms       |

The phase added no dependency, and RSS moved within the run-to-run noise. The first-frame times vary between runs by more than the phase changed them. The frame times come from `Stats::drawing` in the exit check, run once as a release build at each scale: the effects bar's slide and fade took 15 frames.

### Phase 9 results

Measured on 2026-11-14 with `measure_the_counter_bar`, release builds on one 1920×1080 headless Sway output at scale 1. "Before" is 13a520b, built in a second checkout. The desktop example runs every Phase 9 service on private system and session buses with no daemons, and a private pipewire-pulse.

| Metric                            | Before Phase 9 | After Phase 9 |
| --------------------------------- | -------------- | ------------- |
| Counter example RSS               | 16.4 MiB       | 16.6 MiB      |
| Counter first frame               | 11.3 ms        | 9.6 ms        |
| v0.1 bar RSS                      | 19.6 MiB       | 19.8 MiB      |
| Desktop example RSS               | none           | 20.1 MiB      |
| Desktop example first frame       | none           | 10.2 ms       |
| Idle wakeups, counter and desktop | 0              | 0             |

The phase added no dependency. Every service starts only when a config asks for it, so the counter and the v0.1 bar moved by a few hundred KiB, within the run-to-run noise.

## Phases

Seven phases to v0.1, totaling 16 to 26 weeks of focused work, all guesses, then the phases in "Beyond v0.1". A phase is done only when its exit check passes. Phase 0 is a go/no-go gate for the whole project.

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
   - Exit: tests with a fake tray item pass on a private `dbus-daemon`. By hand, `nm-applet --indicator`, `blueman-applet`, KeePassXC and one Electron app such as Discord all show icons, their menus open, clicks reach the app, and all of them survive a reload. Passed on 2026-11-13: the fake-item tests pass, and the maintainer checked nm-applet, blueman-applet, KeePassXC and Discord on the laptop's session, beside Quickshell's watcher and as the watcher. Surviving a reload was checked the same day under Phase 5's `crabshell run`: after an edit to the template bar reloaded it, all four came back with working menus. Commits 59477fd to de5710a, after a review per step, with popups also tested on headless labwc.
6. **Phase 5, CLI and reload (2-3 weeks).** `init`, `run`, `check`, the watcher, `cx.persist`, crash restarts and `last-good`.
   - Exit: a scripted test checks four cases. A syntax error leaves the old shell running and sends a notification. Fixing it brings up the new shell within build time plus 200 ms. After `kill -9` the shell is back within 2 s. A config that panics at startup three times falls back to `last-good`.
   - Passed on 2026-11-13: `crates/crabshell-cli/tests/exit_check.rs` checks all four cases on headless Sway, with the numbers in "Phase 5 results". Built in the commits from 0b603c2 on, after a review per step. With it, Phase 4's tray apps were checked surviving a reload.
7. **Phase 6, the v0.1 release (1-2 weeks).** Template, README, API docs on docs.rs, published crates, and measured numbers in the performance table.
   - Exit: on a clean user account, you go from `cargo install crabshell-cli` to a running bar in under 15 minutes by following the README.
   - Marked done on 2026-11-13 without the clean-account run, which the maintainer skipped for now. On 2026-11-14 the maintainer ran the bar on Hyprland, where it worked, and signed the exit check off. All seven crates are on crates.io at 0.1.0, tagged `v0.1.0`, and docs.rs built every library's docs. With an empty cargo cache, `cargo install --locked crabshell-cli`, `crabshell init`, `crabshell check` and the first build of the template bar took 2 min 54 s on 12 cores. Commits 0ebf76e to b862f0f.

### Beyond v0.1

After v0.1, the phases bring every feature Quickshell has, so any Quickshell setup can be rebuilt in Crabshell. The list follows Quickshell 0.3.2's modules, from its public docs; its source stays unread. Each phase gets its own plan, exit check and measurements when it starts, and every one keeps the four rules: lighter than Quickshell, idle means asleep, a config is plain Rust, and one bad binding never takes the shell down. The order is a first guess.

| Phase | Area                             | What it brings, by the Quickshell modules it matches                                                                                                                                                                                                                                                                                |
| ----- | -------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 7     | Windows and input                | Floating windows, layer-shell options (keyboard focus, layers, margins, exclusion), input regions and masks, keyboard input (libxkbcommon), text input, scrolling lists, images, clipping and wrapper widgets (`PanelWindow`, `FloatingWindow`, `PopupWindow`, `Region`, `WlrLayershell`, `WlrKeyboardFocus`, `Quickshell.Widgets`) |
| 8     | Animation and effects            | Animations that run only while something moves, gradients, shadows and the compositor's background blur (`EasingCurve`, `BackgroundEffect`), and the GPU backend if the CPU renderer can't keep up                                                                                                                                  |
| 9     | Desktop services                 | Notifications server, media players, PipeWire nodes and levels, battery and power profiles, Bluetooth, networking, idle monitor and inhibitor (`Services.Notifications`, `Services.Mpris`, `Services.Pipewire`, `Services.UPower`, `Bluetooth`, `Networking`, `IdleMonitor`, `IdleInhibitor`)                                       |
| 10    | Launchers and system integration | Desktop entries and their actions, processes, files and sockets, an IPC handler driven from the command line, JSON files, color quantizing, screencopy views, toplevel management (`DesktopEntries`, `Quickshell.Io`, `IpcHandler`, `ColorQuantizer`, `ScreencopyView`, `ToplevelManager`)                                          |
| 11    | Lock screen, greeter and polkit  | Session lock surfaces, PAM authentication, a greetd greeter and a polkit agent (`WlSessionLock`, `Services.Pam`, `Services.Greetd`, `Services.Polkit`)                                                                                                                                                                              |
| 12    | More compositors                 | Hyprland (workspaces, monitors, toplevels, focus grab, global shortcuts), i3, niri and generic window-manager protocols (`Quickshell.Hyprland`, `Quickshell.I3`, `Quickshell.WindowManager`, `GlobalShortcut`, `ShortcutInhibitor`)                                                                                                 |
| 13    | Parity check                     | A comparison against every Quickshell 0.3.2 type, with each gap closed or written down with a reason                                                                                                                                                                                                                                |

Phase 7 passed on 2026-11-14, built in the commits from aa5d8c5 to a0b82d8 after a review per step. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the launcher example on headless Sway: an overlay panel that takes the keyboard, with a text field filtering 500 rows in a `ListView` and an about `FloatingWindow`. It matches its goldens at scales 1, 1.5 and 2, typing filters the list within one frame a key, the wheel scrolls it, the compositor closes the floating window, the window's nodes come back to their count, and the open launcher wakes nothing while idle. Gaps left for later phases: no drag or kinetic scrolling and no scroll bar, no drag-select in text fields, popups that never shrink to fit, and no wrapper widgets of their own, since a `Stack` or `Button` with a padded style covers `WrapperItem`, `WrapperRectangle` and `WrapperMouseArea`, and `Style::clip` covers `ClippingRectangle`.

Phase 8 passed on 2026-11-14, built in the commits from 5c301c4 to 1cb98f5 after a review per step. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the effects example on headless Sway: a bar with a translucent gradient that asks the compositor to blur behind it, tabs whose hover color fades in, an indicator that slides under the picked tab with `cx.animate`, and a card with a soft shadow. It matches its goldens at scales 1, 1.5 and 2. Picking a tab draws 15 frames while the slide and the fade run, then one dispatch sleeps for 2 s. Sway lacks the blur protocol, which is logged once. The CPU renderer draws each of those frames in well under a millisecond, so the GPU backend waits. Gaps left for later phases: blur is untested against a compositor that has `ext-background-effect-v1`, one shadow per style and no inset shadows, no dithering, no springs, and layout values animate only through `cx.animate` driving a style's size, which lays the window out each frame. The same day the maintainer's own Quickshell bar, the rime islands, was rebuilt as a Crabshell config and checked by eye on the laptop's SwayFX session, blur included. What it couldn't rebuild was left out: its panels, now playing, network, Bluetooth, battery and keyboard layout, which later phases bring, and two things no phase lists yet: a shadow cut out under translucent glass, and rotating or scaling a widget.

Phase 9 passed on 2026-11-14, built in the commits from 1954fa0 to afe1fbd after a review per step; agents built MPRIS, notifications, Bluetooth and networking in worktrees of their own, and each was reviewed before its fixes went in. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the desktop example on headless Sway with every Phase 9 service started on private buses and a private pipewire-pulse. It matches its goldens at scales 1, 1.5 and 2, a notification sent with `gdbus` shows as a toast and goes when it expires, and then the shell wakes nothing for 10 s. Each service is tested against a fake daemon on a private bus, and audio also against a private PipeWire. Gaps left for later phases: audio level meters and PipeWire's links, saved network connections and connectivity checks, a Bluetooth pairing agent of the shell's own, notifications lost while a reload swaps the shell, and a hand check against the real daemons of a desktop session.

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
- Workspaces are tested against a fake Sway socket. The tray is tested with a fake item on a private `dbus-daemon` that starts no services. Audio is tested against a fake server, and against a private pipewire-pulse in `#[ignore]` tests run by hand.
- Integration tests run on headless Sway, as described under Wayland platform.

### Reporting

- End every task with what changed, how you tested it, and what the human must check by hand on a real screen.
- Explain Rust choices in plain words for a beginner. When you pick `Rc` over `Arc`, add a lifetime, or choose `Box<dyn Trait>` over generics, say why in a sentence or two.

## Risks and open questions

The biggest risk is the reload loop. A 2-10 s rebuild per edit is the price of compiled configs, and it is the one goal this design doesn't meet. The second is the size of the goal: Quickshell exposes about 240 QML types, so full parity is many phases past v0.1.

| Risk                                      | Why it matters                                                              | Mitigation                                                                                                                           |
| ----------------------------------------- | --------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| Reload misses the goal                    | 2-10 s per edit, against Quickshell's sub-second reload                     | Fast dev profile, a small config crate, build time measured in Phase 5. Try Dioxus's experimental subsecond hot-patching after v0.1. |
| You review Rust you can't fully judge yet | Bugs in the reactive core or event loop hide well                           | Small changes, required tests, headless Sway screenshots, and plain-language explanations from agents                                |
| Tray apps bend the spec                   | Electron, Qt and Ayatana apps each implement StatusNotifierItem differently | Phase 4 gets 4-6 weeks and tests real apps                                                                                           |
| Text looks worse than Qt's                | v0.1 has grayscale antialiasing only                                        | Compare screenshots side by side in Phase 0                                                                                          |
| Font loading slows startup                | Scanning every system font can blow the 100 ms target                       | Load only the files fontconfig matches                                                                                               |
| Sway-first shrinks the audience           | Most Quickshell users run Hyprland or niri                                  | Keep compositor IPC inside `crabshell-services`, so adding Hyprland later touches one module                                         |
| Configs need a Rust toolchain             | A higher entry bar and a slow first build                                   | Template and README, with the full first run timed in Phase 6                                                                        |
| No accessibility in v0.1                  | Screen readers can't read the shell                                         | Add AccessKit (AT-SPI) right after v0.1                                                                                              |

Open questions:

- [x] Project name: Crabshell.
- [ ] Which other wlroots compositors to test (river, labwc, Wayfire), and which of them implement `ext-workspace-v1`.
- [ ] Should a reload start the new shell before stopping the old one, to remove the blink? That needs a fix for both bars reserving space at once.
- [ ] Stay on libpulse after v0.1, or switch to native PipeWire?
- [ ] When to add Hyprland and niri, and in which order.
- [ ] Should the shell exit with status 1 when `f` panics for every output at startup? Today it keeps running with no bar, so Phase 5's fallback to last-good after three crashes never fires for that config.
- [x] Install cargo-deny and add a deny.toml. Done in Phase 6, and CI runs it.

## Sources

- [Quickshell source](https://github.com/quickshell-mirror/quickshell) at commit `9b0f339` from 2026-10-21, counted locally. About 62k lines of C++ exposing 241 QML types, licensed LGPL-3.0.
- [Quickshell 0.3.2's type list](https://quickshell.org/docs/v0.3.2/types), from its public docs, read on 2026-11-13 for "Beyond v0.1".
- Everything else comes from memory and wasn't checked against current docs: crate names and APIs, protocol versions, Sway commands and toolchain behavior. Phase 0 checked the stack's crates against their source and found four changes: sctk 0.21 uses delegate_dispatch2! and has no fractional-scale helper, cosmic-text 0.19 changed set_text, and zbus 6 replaced async-io with zruntime. Pinned versions are in the repo's Cargo.toml.
