# Crabshell, a full Rust replacement for Quickshell

Nov 10, 2026 · @Idan Gilboa

Build a new shell toolkit in Rust instead of porting Quickshell, and make it a full replacement: everything Quickshell can do, Crabshell does. Configs are compiled Rust crates and the toolkit draws on the CPU. The first milestone, v0.1, is a bar for Sway and other wlroots compositors. It is a checkpoint that proves the approach works, not the scope; the phases after it bring the rest of Quickshell's features (see "Beyond v0.1"). The overview is for you. Every section after it is the spec that AI agents build from.

## Overview

Nothing from Quickshell carries over. Users write their shell as a Rust crate that links the toolkit, and a supervisor rebuilds and restarts the shell when they save. The project is called Crabshell. The `crabshell` crate and every `crabshell-*` name were free on crates.io and the AUR on 2026-11-10.

| Decision           | Choice                                                                    | What it costs                                                                                                                                 |
| ------------------ | ------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| Config language    | Plain Rust, compiled                                                      | First build takes about 3 min, then under a second per one-line edit. Every user needs a Rust toolchain.                                      |
| Config API         | Builder methods and fine-grained signals                                  | Users learn closures and `move`. Signals are `Copy`, so nobody clones handles.                                                                |
| Reload             | Supervisor rebuilds, saves state, starts the new shell beside the old one | Both shells run for about 80 ms per reload; nothing blinks.                                                                                   |
| Styling            | Typed Rust styles only                                                    | Every color change is a rebuild.                                                                                                              |
| Rendering          | Own CPU renderer, GPU later                                               | v0.1 had no gradients, shadows or blur. Phase 8 added gradients and shadows, and asks the compositor to blur.                                 |
| Scope              | Everything Quickshell does, in milestones                                 | v0.1 is a bar with workspaces, clock, tray and volume. Notifications, launchers, lock screens, OSDs and the rest come in the phases after it. |
| Compositors        | Sway and other wlroots compositors first                                  | Hyprland came in Phase 12 and niri's IPC in Phase 18.                                                                                         |
| C libraries        | Fine when the system ships them                                           | Links libpulse, fontconfig, libxkbcommon and libpam at runtime. Wayland itself is pure Rust.                                                  |
| Quickshell configs | No compatibility, no migration                                            | Users rewrite their configs by hand.                                                                                                          |
| License            | MIT or Apache-2.0                                                         | Agents write fresh code and never translate Quickshell's LGPL-3.0 source.                                                                     |
| Team               | You plus AI agents, you new to Rust                                       | The spec spells out every rule, and each phase ends in a check you can run yourself.                                                          |

### Better than Quickshell

- **Memory.** No QML engine, no JavaScript engine and no GPU context in v0.1. The target is 30 MB or less at idle for the v0.1 bar, checked in Phase 0, and every later feature keeps to the same budget.
- **Idle CPU.** With nothing changing on screen, the clock's once-a-minute tick is the only wakeup.
- **Startup.** The shell is a compiled binary. The first frame should land within 100 ms of launch.
- **Crashes.** Rust rules out memory bugs in the toolkit. A panic inside one config closure disables that binding and logs it, and the rest of the shell keeps running. If the process dies anyway, the supervisor restarts it and falls back to the last build that worked.
- **Config errors.** The compiler catches them before the shell runs, and rust-analyzer gives autocomplete and inline errors.

### Worse than Quickshell

- **Reload is slower.** Quickshell reloads QML in well under a second. Here every edit costs a rebuild plus a restart, measured at 1.0 s for a one-line edit (see Performance targets).
- **Configs need Rust.** That is a higher bar than QML for users, and for you while you learn.
- **No ecosystem.** There are no existing configs to copy from.

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

The template's `Cargo.toml` compiles dependencies once with full optimization and without debug assertions or overflow checks, as release does, and the config itself at `opt-level = 1`, so edits rebuild fast and the shell still runs fast.

```toml
[package]
name = "my-shell"
version = "0.1.0"
edition = "2024"

[dependencies]
crabshell = "0.9"

[profile.dev]
opt-level = 1

[profile.dev.package."*"]
opt-level = 3
debug-assertions = false
overflow-checks = false
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
- Event handlers receive `&Cx` and run on the main thread.
- `Style` is `Copy` and every builder method is a `const fn`, so a theme is a set of constants. `Text` inherits `color`, `font`, `text_align`, `line_height`, `underline` and `strikethrough`, each from the nearest ancestor that sets it. Nothing else inherits.
- `cx.persist("key", default)` returns a `Signal<T>` for any `T: Serialize + DeserializeOwned`. Its value survives reloads. If an edit changes the type so the saved value no longer fits, it falls back to the default and logs once. The key `crabshell::services` is the shell's own.
- A service that isn't running gives `None` or an empty list. A missing Sway socket or audio server logs once and never panics.
- Containers store children as `Box<dyn Widget>`. Deeply nested `impl Widget` types slow compiles, and compile time is reload time.

API rules settled while building `crabshell` (Phase 2, step 4):

- The shell's methods on `Cx`, such as `cx.per_output`, come from the `ShellExt` trait, and the services', such as `cx.sway()`, from `ServicesExt`, since only the crate that defines a type can add methods to it. The prelude brings both in.
- `f` returns a `PanelWindow`, or a `Vec<PanelWindow>` for several windows or none.
- Call `per_output` from `root` or from inside another `per_output`'s `f`. A builder ends when the context it was registered with is disposed, and its windows go with it. Called from a plain effect, it adds another builder on every run.
- A panic in `f` is logged with the output and the call site, for example `per_output[DP-1] (called at src/main.rs:4)`. Whatever `f` built is dropped, and that output gets nothing until it is announced again. When the compositor closes a window, that output's bar is dropped the same way and logged at info.
- `cx.set_icon_theme` repaints every window whole.
- `run()` reads GTK's icon theme once, before the root closure, from `$XDG_CONFIG_HOME`, or `$HOME/.config` when that is unset or relative. A file that is missing or names no theme leaves it to the next. One over 64 KiB, not UTF-8, not a regular file, or naming a theme with a `/` is logged and skipped. The config's own `set_icon_theme` lands after it and wins.
- `crabshell::run` exits with 1 (`EXIT_ROOT_PANICKED`) when `root` panics, or when every `per_output` closure panics for the outputs there at startup and no other window was made. It exits with 2 (`EXIT_NO_COMPOSITOR`) when there is no compositor, a required protocol is missing, or the connection is lost. A logout is not a crash.
- When the first output is listed, the shell sends `wl_display.sync` and judges startup when it is answered, since the compositor answers after every output it announced before. If by then every `per_output` closure panicked and there is no window other than a popup, it logs `every per_output closure panicked at startup, so the shell has nothing to show; exiting`, stops the config's programs, removes its IPC socket and exits with 1. A closure that returns no windows didn't panic. A shell started with no outputs waits, and a closure that panics for an output announced later is only logged. `Shell::dispatch` never exits.
- `CRABSHELL_LOG` sets the log level, such as `debug`. Unset or empty means `info`, and a value that doesn't parse is warned about once.
- `cx.provide::<T>()` and `cx.service::<T>()` use one services map shared by every child context. `Shell` drives the loop by hand for tests, and dropping it frees everything it made.
- The prelude holds the widgets, styles, `Cx`, `Output`, the signal types, `ShellExt` and `IntoPanels`. Phase 3 adds `ServicesExt`, `Precision`, `Workspace` and `Sink`, and Phase 4 `PopupWindow`, `Menu`, `MenuEntry`, `MenuToggle`, `Tray`, and `TrayHost` with its item and menu types.

API rules settled while building `crabshell` (Phase 3, step 4):

- `Shell::new` calls `crabshell_services::provide` before `root`. Each service starts on first use, so a config that never calls `cx.sway()` opens no Sway connection.
- `crabshell::jiff` re-exports jiff, since `cx.clock()` hands out a `jiff::Zoned`.
- The shell logs `workspace switch on screen in X ms` at the first commit from any window after Sway's event. A switch no commit shows within 1 s is dropped. The log reads the Sway service only if a config started it.
- Linking libpulse costs every config about 2.1 MiB of RSS, even one with no volume widget.

API rules settled while building `crabshell` (Phase 5, step 1):

- `cx.persist`'s signal lives under the root owner, so it outlasts the context that asked and any output. The same key gives the same signal. The same key with another type is logged and gets a signal under the asking context that isn't kept.
- `run` reads `$XDG_RUNTIME_DIR/crabshell/state.json` before `root` and removes it, so a shell started later, as after a crash, starts fresh. Without `XDG_RUNTIME_DIR`, nothing is saved, and that is logged.
- On `save`, the values are written to a new file beside `state.json` that then replaces it, in a directory only the user can read (a looser one is tightened, a symlink refused), and the shell exits with 0. A value whose `Serialize` fails or panics is logged and left out. A saved value whose `Deserialize` fails or panics is logged, and the default is used.
- The shell sends `ready` once every panel it shows has committed a frame, or, when it shows none, once the startup judgement passed. A panel waiting for its output, or refused by layer-shell, isn't waited for. An unknown command is logged and skipped. If the supervisor hangs up, the shell runs on alone.
- `Shell::new` starts with no saved values. Only `run` reads the file and follows the supervisor.

API rules settled in Phase 7, windows and input:

- A property that the widget itself writes back takes a `Signal`, not a value or a closure: `TextInput::new(text)`, `ScrollView::offset`, `ScrollView::offset_x` and `ListView::offset`. Typing and scrolling write the signal, and setting it changes what shows. Every other property still takes a value or a closure.
- A `cx.floating` window lives as long as the calling context, and its `visible` shows and hides it meanwhile. Without an `on_close`, a close from the compositor hides it until `visible` turns false and true again.
- A `PanelWindow`'s namespace is read once, when the window is shown, and a `PopupWindow`'s `edges`, `gravity` and `adjustment` when it opens.
- Keys go to the focused text field, then the focused button's `on_key`, then the window's `on_key`; a handler returns `true` when it used the key. Then Tab and Shift+Tab move the focus between buttons and fields, and Enter or Space clicks the focused button. A button's `on_press` runs while the pointer button is held, the time for `cx.start_move()` and `cx.start_resize(edges)` in a floating window.
- The prelude adds `KeyboardFocus`, `Margins`, `Region`, `Key`, `KeyEvent`, `Modifiers`, `Image`, `Fit`, `ScrollView`, `ListView`, `TextInput`, `FloatingWindow` and `Adjust`.

API rules settled in Phase 8, animation and effects:

- `cx.animate(target, transition)` returns a `ReadSignal` that follows the closure `target`, moving to each new value over the transition instead of jumping. It starts at the first value. `f32`, `Point` and `Color` implement `Animate`.
- `Transition` is a `const` value, and its default easing is `OutCubic`. A gradient takes up to six stops between its ends.
- A style change fades in over the new style's `transition`: background, gradient, shadow, color, border, radius and opacity. Other fields change at once.
- `Shadow::inset()` draws the shadow inside the box, as CSS's `box-shadow: inset` does: over the background and under the border and content, everywhere inside the border except a hole of the box's shape, moved by `x` and `y`, shrunk by `spread` and blurred. It never reaches past the box, and `follow_content` and `cut_out` do nothing to it. A style has one shadow; a second comes from a box wrapped around the widget.
- `PanelWindow::blur` and `FloatingWindow::blur` name the region the compositor blurs behind the window.
- The prelude adds `Animate`, `Easing`, `Transition`, `Gradient`, `GradientShape` and `Shadow`.

API rules settled in Phase 9, desktop services:

- Each service hands out `ReadSignal`s of plain structs, such as `Vec<BluetoothDevice>`, and methods that take plain values, such as a device's path or a player's bus name.
- `cx.idle_monitor(timeout)` gives a `ReadSignal<bool>`, `true` while the user has been away for `timeout`.
- `Player::position_at(now)` works out where playback is, since MPRIS doesn't say as it moves.
- `cx.bluetooth_agent()` makes the shell BlueZ's pairing agent. One request shows at a time, as a `ReadSignal<Option<PairingRequest>>` whose `PairingKind` says what BlueZ wants.
- The prelude adds the services' types: `UPower`, `PowerDevice`, `DeviceKind`, `BatteryState`, `PowerProfiles`, `PowerProfile`, `Bluetooth`, `BluetoothAdapter`, `BluetoothDevice`, `Network`, `NetworkDevice`, `NetworkKind`, `NetworkState`, `WifiNetwork`, `Mpris`, `Player`, `PlaybackStatus`, `LoopStatus`, `Notifications`, `Notification`, `Urgency`, `Timeout`, `AudioNode` and `AudioStream`.

API rules settled in Phase 10, launchers and system integration:

- `Button::hovered(signal)` writes whether the pointer is over the button or anything inside it, inner buttons included, so an outer button can show controls on hover; `hover_style` still shows only on the deepest button. Buttons may share one signal, which is `true` while any of them has the pointer. `Shadow::cut_out()` leaves a shadow out under its box, as CSS always does, so translucent glass shows what is behind it.
- `cx.process(|| command)` runs a program. `command` returns a `std::process::Command` or a list such as `["uptime", "-p"]`, and runs again when a signal it read changes. `stopped()` starts nothing until the first `restart`, for a program run on demand. `cx.exec_detached(command)` starts a program that runs on its own, as a launcher does.
- `cx.file_view(path)`'s `text()` follows the file and its `write` replaces it. `cx.json_file(path, default)` gives a `Signal<T>` kept in step with a JSON file both ways.
- When the compositor offers `xdg-activation-v1`, `launch`, `launch_action` and `cx.exec_detached` ask it for an activation token first and start the program once it comes, or after 100 ms without one, so the compositor may focus the new window.
- A `cx.ipc(target, function, handler)` handler is `Fn(&[String]) -> Result<String, String>`. `crabshell ipc <target> <function> [args...]` calls it and prints its `Ok` text or reports its `Err`; `crabshell ipc` lists the handlers.
- `cx.quantize_colors(path, depth)` gives up to 2^`depth` main colors of an image, found off the loop's thread.
- `cx.toplevels()` gives `Toplevels`, a `Copy` handle whose requests take a window's `id`.
- `ScreencopyView::new(source)` shows an output by name or a window by its `Toplevel` id, once or `live`.
- The prelude adds `untrack`, `Process`, `IntoCommand`, `Socket`, `FileView`, `DesktopEntries`, `DesktopEntry`, `DesktopAction`, `Toplevels`, `Toplevel`, `ScreencopyView` and `CaptureSource`.

API rules settled in Phase 11, lock screen, greeter and polkit:

- `cx.session_lock(SessionLock::new()...)` gives a `ReadSignal<bool>`, `true` while the compositor says the session is locked. Turning `locked` false is the only unlock, and the signal turns false once the compositor has it, so a config may exit then. If the lock's context goes or the shell exits or crashes, the session stays locked. So keep `locked` in a `cx.json_file` under `$XDG_RUNTIME_DIR`: the shell after a reload or a crash locks again, and a reboot starts unlocked. `cx.persist` covers a reload only.
- `cx.pam(service)` runs a file in `/etc/pam.d`, and `start(None)` authenticates the shell's own user. Take the password first, start on Enter and answer as soon as PAM asks: an attempt ended while PAM waits for an answer counts as a failed login where `pam_faillock` counts them.
- `cx.exit(code)` ends the shell with that status, as a greeter does once its session starts.
- `cx.polkit_agent()` makes the shell the session's polkit agent.
- The prelude adds `SessionLock`, `Pam`, `PamState`, `PamResult`, `PamMessage`, `Greetd`, `GreetState`, `GreetMessage`, `PolkitAgent`, `AuthRequest` and `AuthState`.

API rules settled in Phase 12, more compositors:

- `cx.sway()`'s `dispatch(command)` runs any Sway command as `swaymsg` does, and `on_event(kinds, handler)` calls `handler` with each event of the kinds given, with Sway's JSON, while the asking context lives.
- `cx.window_manager()` gives `WindowManager`, a `Copy` handle over `ext-workspace-v1` with Quickshell's names. `windowsets_on(output)` is what a bar on that output lists. A windowset's `can_` fields say which requests the compositor takes, and a request they leave out isn't sent.
- While `inhibit_shortcuts` is true and the window has the keyboard, the compositor's own shortcuts reach the window as keys. `shortcuts_inhibited(signal)` has the compositor write whether it honors that.
- `cx.global_shortcut(app_id, name, description)` gives a `ReadSignal<bool>`, true while the compositor's binding for it is held, as Hyprland's `bind = SUPER, D, global, crabshell:launcher`. Calls with the same app id and name share one registration.
- While a `focus_grab` signal is true, the windows given it, with their popups, hold the keyboard and pointer, and a click elsewhere has the compositor clear the grab and write `false`. Writing `false` ends it.
- `cx.hyprland()` gives `Hyprland`, a cheap clone, whose lists name each other by name, as `Workspace::output` does. `focus_workspace(target)` switches in the syntax Hyprland reads: Lua, with the name quoted as a Lua string, whenever its config is Lua, as it can be from 0.55 and always is from 0.57; otherwise `workspace X`, where a name with whitespace or a control character is logged and not sent. Whenever Hyprland's config is Lua, `dispatch(command)` runs a Lua expression inside the compositor, so never put window titles, classes or workspace names into it. `on_event(handler)` gets every raw event until the asking context goes.
- `on_event` on a Hyprland, niri or Sway handle whose context went logs that and does nothing.
- The prelude adds `Sway`, `SwayOutput`, `SwayEvent`, `SwayEventKind`, `WindowManager`, `Windowset`, `WindowsetProjection`, `Hyprland`, `HyprlandWorkspace`, `HyprlandMonitor`, `HyprlandToplevel` and `HyprlandEvent`.

API rules settled in Phase 13, parity:

- `cx.outputs()` lists every output before `per_output` builds for it, and follows every change the compositor tells. `scale` is the whole scale and `fractional_scale` the one windows draw at.
- `Style::padding_top` and the other sides set one side each and keep the others. A row's or column's own `.padding(v, h)` still wins on both sides of its axis, and `merge` takes the top style's whole padding.
- `Adjust::ALL` includes resizing, as Quickshell's `All` does. The default flips and slides. A popup the compositor resizes is drawn at the size it gives.
- A popup a click opened holds the pointer, so a click outside its mask reaches the shell's own window below, but over another program it closes the popup.
- `compositor_opacity` is the opacity Hyprland shows the whole window at, blur included, unlike a style's `opacity`, which Crabshell paints. `visible_mask` is a hint naming the part that holds anything to see, and must cover every pixel that isn't fully transparent.
- `cx.notifications().on_closed(...)` hears each notification that leaves the history, while the asking context lives.
- A saved network connection's `settings` hold no secrets. Read `saved_connections()` with `with`, since each `get` clones it.
- `on_split(marker, handler)` on a `Process` or `Socket` splits its output at `marker`; `on_line` is `on_split("\n", ...)`, and `""` hands over each read. A handler that sets another marker has the rest split at it. `FileView::error()` is cleared by a read or a write that works. A `cx.socket_server(path)` hands each client to `on_connection` as a `Socket` with a context of its own.
- A `cx.audio().peaks(target)` meter wakes the shell 25 times a second while its target plays, so build one only while it shows.
- The prelude adds `OutputInfo`, `TrayCategory`, `PerformanceDegradation`, `BluetoothAdapterState`, `BluetoothDeviceState`, `CloseReason`, `PamError`, `Connectivity`, `ConnectionFailReason`, `WifiMode`, `WifiSecurity`, `SavedConnection`, `FileError`, `FileErrorKind`, `SocketServer`, `AudioChannel` and `PeakTarget`.

API rules settled in Phase 14, time and motion:

- `cx.after(delay, f)` runs `f` once after `delay`, and `cx.interval(period, f)` every `period`, the first time one period in. Each gives a `Timer`, a `Copy` handle that goes with the asking context. `restart()` counts again from now and runs a one-shot that fired again. `stopped()` stops a timer before it ever fires, so `cx.after(delay, f).stopped()` waits for its first `restart()`.
- `Easing::Spline` takes several segments, for curves such as Material 3's emphasized one.
- `Transition::delay(ms)` waits before it moves, style fades included, and `Transition::alternate()` makes a cycle come back.
- A button's style fades back to its own over `Style::transition_out` whenever it returns there from a hover, pressed or focus style, and over `transition` without it. Fades toward a state style, or between state styles, take the new style's `transition`. Only the button's own style's `transition_out` is read, and `merge` keeps the bottom style's.
- An `Animation`'s `running()` is true from a new target, through its delay, until it settles. An effect on it is where whatever follows an animation goes, as Qt's `onFinished`. `cx.animate` is `cx.animation`'s `value()`.
- `cx.animate` and `cx.animation` take the transition as a value or a closure, read untracked as each new target starts, so one animation can pick its curve by direction. `cx.cycle` keeps a fixed `Transition`.
- `cx.cycle(running, transition)` gives eased progress from 0 to 1 that repeats while `running` is true. `cx.spring` follows an `f32` on a spring that keeps its speed when the target moves; the default stiffness and damping are 700 and 0.9, Material 3's standard spatial spring. Its `running()` turns false at once when the value jumps. `cx.frames(running)` gives the time since `running` turned true, updated each frame.
- `Style::translate`, `scale`, `rotate` and `origin` move a widget and everything inside it where it is drawn and hit, never the layout, and they fade with the `transition`. Popups and anything else anchored to a widget keep its laid-out rectangle.
- `.exit(duration)` after `children_keyed` takes a value or a closure, read as each key leaves. The removed child stays at most that long while `cx.leaving()` is true, so its own `cx.animate` can move it out, and an exit of 0 removes it at once. A leaving child still takes input, so a handler can check `cx.leaving()`. To move a new child in, set a signal after making its `cx.animate`; the next flush starts the move.
- A removed child is freed at the first tick of the animation clock that finds nothing moving inside it, or at its exit, whichever comes first. Only motion made inside the item counts; motion stepped by `cx.interval`, a GIF, or an animation made outside the item doesn't hold it, so drive it with `cx.animate` or `cx.animation` in the item to keep it. A child with nothing moving goes at the first tick, which comes at once.
- `cx.leaving()` inside an item is true while that item leaves, and while any item of a list with an exit around it leaves.
- The prelude adds `Timer`, `Animation`, `Spring` and `std::time::Duration`.

API rules settled in Phase 15, layout, pointer and views:

- The layout fields of `Style` merge per field, and a number that isn't finite counts as unset. All but `z` change the layout at once; a `z` change repaints.
- Any of `left`, `top`, `right` and `bottom` takes a widget out of its parent's flow, as CSS's `position: absolute`: it is placed from those edges of the parent's padding box and no longer sizes the parent. `align_self` places a child across a row or column, or vertically in a stack's or grid's cell, and `justify_self` horizontally in a cell (an absolute child aligns in the parent's padding box instead); rows and columns ignore `justify_self`. `z` orders siblings for drawing and for the pointer, lowest first, keeping child order for equal ones; Tab keeps child order.
- `Grid` places children row by row into `columns` equal columns, 1 by default, with rows as tall as their content. A `wrap` row starts a new line for children that don't fit, and `reverse` runs a row right to left and a column bottom to top, so `Justify::Start` packs at the right or the bottom.
- Hidden through `visible`, a widget frees its space, isn't drawn, hit or reached by Tab, drops the focus, unhovers and lets go of a held press, and keeps its bindings and state. A field inside it can't take the focus, and when it shows again the first field in Tab order whose `focus` is still true takes it.
- After each layout that moves or resizes a widget, its laid-out rectangle is written to its `geometry` signal, in window logical pixels, with scroll offsets but not transforms. `mapped_geometry` writes the box where it is drawn, through its own and its ancestors' transforms, every frame of an animated transform whether or not anything reads it, so pass it only where it is used. A hidden widget writes neither. A window laid out from another's geometry catches up in the same loop iteration. Neither says where the window is on its output, since layer-shell and xdg-shell don't tell a client.
- `on_release` runs when the left button comes up after going down on the button, wherever the pointer is, before anything else the release runs. `on_double_click` replaces the second `on_click` when two clicks come within 400 ms and 5 logical pixels, timed by the compositor. `on_long_press` runs once the left button has been held 800 ms without moving 10 logical pixels, after which the release doesn't click.
- A drag starts when a held left button moves more than 10 logical pixels, or at the press for a button with no `on_click`, `on_double_click` or `on_long_press`, so a slider moves from the first touch. While it drags, the button gets every motion inside the window or out, it and the buttons around it stay hovered, and it doesn't click. It ends with `End` on release, on leaving the window, or when the button is turned off; a button removed or a window dropped mid-drag runs nothing more.
- `pointer_position(signal)` writes the pointer's place in the button's own coordinates while it is over the button or inside it, following a drag anywhere, and `None` when it leaves, only on a change.
- `on_wheel` gets every scroll event on both axes, positive down and right, unlike `on_scroll`, which keeps whole vertical steps with up positive. With both, both run, and either takes the scroll from a scroll view.
- A text field keeps the I-beam cursor, and a disabled button shows what is around it. While a button drags, the pointing hand and the open hand show as the grabbing hand until the release; any other shape stays.
- A disabled button runs no handler and shows no hover, pressed or focus style, Tab skips it, and the pointer passes through it to the button around it. `enabled` isn't inherited. Turning it off while held lets go, ending a drag with `End`, and drops the focus. `focus(...)` takes the keyboard focus whenever it turns true, unless the button is disabled or hidden.
- A touchscreen's first finger is a left-button pointer, so a tap clicks and a moving finger drags.
- `.displaced(transition)` after `children_keyed` takes a value or a closure, read untracked at each change of the list. When keys are added, leave once their exit ends, or move, each child whose place changes slides from where it was drawn to its new place through a translate, so it is hit where it is drawn and the window isn't laid out each frame. A new child shows in its place, a transition of 0 jumps, and other layout changes, such as a resize, still move children at once.
- While `Button::drag_data` holds a value, the button drags as one with `on_drag` does; while it holds `None`, it clicks, so `editing.get().then_some(id)` drags only in an edit mode. `on_drop` takes drops of its type only, and `drop_hovered` is `true` while a drag carrying such a value is over it. Releasing over it runs `on_drop` with a copy of the value; releasing anywhere else drops nothing. `Drag::dropped` is true at `End` only when the target's `on_drop` ran and returned. Once the pointer leaves the window, a drag carrying a value goes on as a Wayland drag that reaches buttons in the shell's other windows; the value is handed over in-process, so another program never gets it.
- `Slider` clamps its signal to `range` (0 to 1 by default), snaps it to `step` (0, continuous, by default) and writes it only on a change. A value set from outside moves the handle, past either end it shows at that end, and NaN shows at the start. A press moves the value there and drags with no threshold. The wheel moves a step a notch, or a twentieth of the range without a step, a touchpad one step per 15 pixels, and the arrow keys the same while it has the focus. The track is 120 by 4 and the handle 16 by 16 unless their styles set a size. `dragging` turns false when the slider goes while held.
- To bind a slider to an outside value, write the outside value into the slider's signal only while `dragging` is false, and send the signal's value out only when it differs from the outside one, so neither side loops and a held handle doesn't jump back to a late reply.
- A horizontal `ScrollView` takes sideways wheel and touchpad input and, with no sideways input, a turned wheel. One that scrolls `both` ways moves and springs back on each axis on its own, and `both` wins over `horizontal`.
- With `kinetic` and `overshoot`, both on by default, a fling keeps moving at most 2500 px/s, slowing by 1500 px/s², and a touchpad, drag or fling goes past either end, within a quarter of the box, then springs back. The offset signal stays within the content. A wheel notch still moves 48 px at once. `mouse_drag`, on by default, lets the mouse drag the content as a finger does; off, a mouse press that moves and comes up over a button clicks it.
- A `scroll_bar` handle shows while the child is longer than the box, cut to the view's rounded corners, `width` 6 by default. Dragging a handle scrolls. A press beside it pages toward the press, then after 300 ms pages again every 50 ms until the handle reaches the pointer or the button comes up. The bar takes the press over the view's own content, but not over a button drawn above the view from outside it.
- `ListView` rows are at least `row_height` tall and grow to fit their content, and each height, measured after layout, is kept by key; a row not built yet counts as `row_height`. A `current` past the end is scrolled to once the items reach it.
- `GridView` lays out cells of one size in as many columns as fit, at least one, building only the rows in view plus one each side. Horizontal, it fills each column top to bottom and counts its cells across again when resized or turned.
- A `highlight` moves to a new current item over its style's `transition`, place and size together. A change for the same item, as a row is measured, jumps at rest, and while a slide runs it turns the slide toward the new place. While a list or grid moves, a change of `current` scrolls nothing and only the highlight follows it, so `current` can follow the hovered row.
- Every `Option<T>` passes as a plain value, so `.drag_data(Some(id))` works, and so does `Transition`.
- `Window`'s pointer methods take the compositor's event times, which time double clicks and flings.
- A date shown without a clock waits for the next midnight with `cx.after`, re-armed each day in a fresh child context, so it wakes once a day instead of every minute.
- The prelude adds `Grid`, `Rect`, `Point`, `Drag`, `DragPhase`, `Wheel`, `WheelAxis`, `ScrollSource`, `CursorShape`, `Slider` and `GridView`.

API rules settled in Phase 16, drawing and effects:

- A `Canvas`'s paint closure runs in an effect, inside `catch_unwind` with no borrow held, once the canvas is first laid out, again when its laid-out size changes, and again when a signal it read changes. Each run replaces the drawing and repaints only the canvas's box, never the layout, and a drawing equal to the last repaints nothing. Coordinates are logical pixels from the canvas's top-left, and the drawing is cut to its box and corners. A panic is logged as `<path>/Canvas.paint` and disables the closure, so the last drawing stays, even through a resize.
- A fill uses the nonzero rule, and a gradient paint is laid over the canvas's box. Arc angles are in degrees, clockwise from 3 o'clock as an HTML canvas's go, a negative sweep runs counterclockwise, and a sweep is at most a whole turn. An odd dash pattern repeats, as `setLineDash` does, and a stroke width of 0 draws nothing. Paths and strokes are plain data, so a config can build them outside the closure.
- Corners are fitted as CSS fits them: when two corners on a side add up to more than its length, all four shrink by the one factor that makes the tightest side fit. Drawing, a clip's pointer test and `Region` all use `Radii::fit`, so they agree, and everything that draws a box follows each corner.
- `Gradient::sweep(angle, from, to)` runs once around the middle, clockwise from `angle` degrees with 0 at 12 o'clock, as CSS's `conic-gradient(from …)`, and fades into another sweep with as many colors.
- `Style::border_dash(on, off)` dashes the border along its middle line, in logical pixels, stretched or squeezed so a whole number of dashes fits each run from one square corner to the next, centered on both, or goes around a box with no square corner, with one dash centered on the end of the top-left corner. An `on` of 0 draws round dots `off` apart, middle to middle. The dash merges on its own, so a hover style can dash a border the style under it sets; with no border it draws nothing, and a fade takes the end style's dash at once.
- The `Color` helpers are `const fn`s. Hue is in degrees and the rest run from 0 to 1; `mix` blends each sRGB channel and the alpha, and `lighter` and `darker` add to the HSL lightness, as Sass's `lighten` does.
- A `tint` paints every pixel of an icon or image in its color, keeping the pixel's alpha times the tint's.
- `Image::brightness`, `contrast` and `saturation` follow CSS's filters, where 1 leaves the picture as it is. `blur` is a Gaussian's standard deviation in logical pixels, as CSS's `filter: blur()`; a `Shadow`'s blur is CSS's `box-shadow` blur, twice its deviation, so the same number blurs an image twice as much. A number that isn't finite counts as unchanged and one below 0 as 0. They apply once, in the order tint, brightness, contrast, saturation, blur, to the picture scaled to its box, and a change repaints without a layout.
- A GIF with more than one frame plays while it is drawn, at its own delays, and loops as its file says; a delay under 20 ms counts as 100 ms. `Image::playing` pauses it on its frame. A GIF with a `blur` shows its first frame, still.
- `Style::mask(Mask)` keeps the widget and everything inside it only where the mask is opaque, as Qt's `OpacityMask`, and nothing shows outside the widget's box. A gradient mask uses only its colors' alpha. `Mask::image` takes a `&'static str`, so a style stays `const`; a path made at run time is leaked once, outside the style's closure. The file is read through `Image`'s loaders and caps, and one that is missing or refused is logged once for its path and the widget drawn unmasked. A mask doesn't change where the pointer hits.
- `Shadow::follow_content()` casts the shadow from what the widget and its children draw, through their alpha, instead of from its box. With a mask it is cast by the masked shape, and cut out it is multiplied by one minus the content's alpha. A content shadow reaching more than 64 logical pixels past its content, its spread plus one and a half blurs, isn't drawn; it is logged once with the widget's path, and `Shadow::casts_from_content()` says whether a shadow is within that. It costs a blur each time the content repaints, so a box takes a box shadow. A fade takes the end style's mask, and whether its shadow follows the content, at once.
- An `ImageTransition` brings a new source in over the old picture over a `Transition`, so its delay and easing work as elsewhere; the default shows it at once. A wipe's angle is CSS's: 0 sweeps toward the top, 90 toward the right. Both pictures are drawn at the image's size, fit and filters. A new source mid-transition starts again from the picture most of the box shows, and going back to that picture shows it at once. A first source, a hidden image or a transition of 0 ms shows at once. A source that can't be loaded keeps the old picture, logged once, and the transition ends at its next tick. While a large new picture is decoded off the loop's thread, the transition waits at its start with the old picture shown.
- Procfs files change without an inotify event, so a value read from one, such as the CPU's use, is sampled with `cx.interval` in a context that lives only while it shows. A `set` notifies even with the value a signal already has, so an effect that builds on a signal set to the same value reads it through a `cx.memo`.
- `IntoProp` is in the prelude, so a config's own widget can take `impl IntoProp<T>`, a value or a closure, as the toolkit's do.
- The prelude adds `Canvas`, `Pen`, `Path`, `Paint`, `Stroke`, `Cap`, `Join`, `Radii`, `Mask`, `ImageTransition` and `IntoProp`.

API rules settled in Phase 17, text:

- Letter spacing is logical pixels after each glyph, at least minus half the font's size, and a number that isn't finite counts as 0. Font features reach the shaper in order, so a later one can undo an earlier one. A slice of the named `FontFeature` constants works anywhere, but one built with `FontFeature::new` only inside a `const`.
- Any `text_align`, `Start` included, lays the text out across its content box, so a lone line sits in that box; with none, a lone line is as wide as it is. `Start` and `End` follow the text's direction, and `Justify` widens the spaces of every line but a paragraph's last. A line height is a multiple of the font size, 1.2 by default, rounded to whole pixels. Alignment and line height are layout properties, the decorations paint properties, and a fade takes the end style's text fields at once. `TextInput` and `TextArea` lay their text out with the alignment, line height and decorations they inherit or set.
- `ellipsis(true)` is `Elide::End` and `false` is `Elide::None`; whichever is called last wins.
- `cx.measure_text(text, font)` measures in logical pixels, shaped as a `Text` with that font is, with the same fonts and fallbacks. Each line break adds a line, and the width is the widest line's. It isn't reactive itself; a binding that calls it runs again when the signals it read change.
- `cx.add_font(bytes)` adds every face in the bytes without copying them. A bundled face serves its family at its own weight, a variable font's whole weight axis, and its slant, before fontconfig is asked, so a config whose text names only bundled families that cover its characters never starts fontconfig. Other weights and slants, and characters the bundled faces lack, still come through fontconfig. Bytes that hold no font are logged and ignored. A font added after text was laid out shapes that text again and repaints every window whole.
- In `TextInput` and `TextArea`, a press places the caret, and a drag selects to the pointer wherever it goes and holds the pointer, so a scroll view around it doesn't scroll. A second press within 400 ms and 5 logical pixels selects the word, a third the line between line breaks, and a fourth starts again; a drag after them goes by whole words or lines. A password is selected whole on a double or triple click, and Shift with a press extends the selection.
- A selectable `Text` shows the I-beam, selects as a field does, drawn in a third of its color, and a press takes the keyboard focus, so Ctrl+C copies while the window has the keyboard. Tab doesn't stop on it. A press anywhere else, the focus moving or new text drops the selection; a span change that keeps the text keeps it. Inside a `Button`, a selectable text takes the press, so a button whose whole label is selectable never clicks.
- In a `TextArea`, Enter breaks the line and Ctrl+Enter runs `on_accept`, or without one goes on to the window. Line breaks are kept as `\n`, and `\r\n` and `\r` from a paste or an input method become `\n`. PageUp and PageDown move by the height of the scroll view around it or 10 lines. Tab, without Ctrl, Alt or Super, types a tab, drawn four spaces wide, and Shift+Tab and Ctrl+Tab move the focus. A `TextInput` keeps Tab for the focus.
- A text area is as tall as its lines, at least one, and as wide as its style allows, 200 by default; `wrap(false)` makes the area as wide as its longest line. In a `ScrollView`, a change that moves a focused field's caret out of the view, or the focus coming to a field outside it, scrolls the view the least that shows the caret, in the same layout, and writes the offset signal.
- What a `Span` doesn't set comes from the text, and `bold` sets weight 700 on the text's font.
- `parse_markup(body)` reads the Desktop Notifications spec's markup, never general HTML. Unknown tags go and their text stays, a stray `<` or a bad entity stays as text, a close tag nothing opened is dropped, and an unclosed tag lasts to the end of the body. `img` shows its `alt`, and its `src` is never read. A link is kept only for `http:`, `https:` or `mailto:`, and is underlined. A body over 64 KiB (`MARKUP_CAP`) is cut, logged once, and parsing takes time in step with the body however deep its tags nest.
- `Text::on_link` runs on a left click on a link span's glyphs. Over a link the cursor is the pointing hand and nothing else gets the click, not even a button around the text; elsewhere the text lets the pointer through. The release must land on the same span with the same link. In a selectable text, a press and release on a link that selected nothing opens it, and a drag selects. A handler that panicked leaves its links plain text, with no Tab stops.
- A link under the pointer, or holding the keyboard focus, is drawn underlined, and in `link_hover_color` when one is set. Only that link's area repaints.
- Tab stops on each shown link of a `Text` with an `on_link`, in text order, and Enter or Space opens the focused link. A focused link gets the hover look and no ring. A focused link whose span stops being a link loses the focus, so Tab starts again from the first stop.
- Crabshell drops the space cosmic-text 0.19 starts an elided wrapped line with, until an upgrade fixes it upstream.
- The prelude adds `FontFeature`, `TextAlign`, `Elide`, `TextMetrics`, `TextArea`, `Span` and `parse_markup`.

API rules settled in Phase 18, system and outside data:

- `cx.http_get(url)` runs the system's `curl`, so nothing waits on the network. A closure fetches again when the URL it gives changes, and an empty URL fetches nothing. `body()` keeps the last whole body through a failed fetch, and a fetch that works clears `error()`. Only `http:` and `https:` go out, redirects included.
- `cx.cover_art(url)` gives a `file://` URL's file, or an `http:` or `https:` picture downloaded into `$XDG_CACHE_HOME/crabshell/art`, and `None` while it downloads or after a failure.
- `cx.logind()`'s `delay_sleep_until(ready)` holds a sleep delay while the asking context lives and lets it go once `ready` reads true after sleep is announced. A lock screen locks before suspend this way: an effect sets its `locked` when `preparing_for_sleep()` turns true, and `delay_sleep_until` waits on the signal `cx.session_lock` gives. A power action asked for before logind is reached is dropped and logged.
- `cx.brightness()`'s `set` goes through logind, so it needs no root, and a value set before logind is reached is sent once it is.
- `cx.niri()` follows niri's event stream as `cx.hyprland()` follows Hyprland's. `focus_workspace` and `focus_window` take niri's ids, and `action(json)` sends any niri action written as JSON. `crabshell::serde_json` re-exports serde_json for it.
- `cx.clipboard()`'s `text()` follows every copy through `ext-data-control-v1`, whatever has the keyboard, and is `None` for a copy a password manager marks secret. `history()` keeps copies from its first call, newest first, at most 100 and 32 MiB, never a secret. An image entry is a `0600` file under `$XDG_RUNTIME_DIR/crabshell/clipboard`.
- A selection in a `TextInput`, `TextArea` or selectable `Text` becomes the primary selection, except a password's, and a middle click in a field pastes it at the click.
- `cx.folder_view(path)` lists one level, hidden entries included, sorted by name ignoring case, at most 10,000, and reads again 50 ms after a change.
- `cx.system_stats(period)` samples every `period`, at least 250 ms, on a timer that lives with the asking context. Each call samples on its own, so call it once above `per_output`. Reads run on a thread of their own, so a value starts empty and fills a moment later, and each sample wakes the loop twice. CPU use comes from the second sample, and `cpus` lists the online CPUs by number.
- `Icon::fallback(source)` draws when the icon's own source can't be found or loaded. `cx.icon_path(name, size)` finds a file as `Icon` does, so `.is_some()` is Quickshell's `hasThemeIcon`. It isn't reactive, but sees `cx.set_icon_theme` at once.
- `cx.locale()` gives the date names of `LC_ALL`, else `LC_TIME`, else `LANG`, read once from glibc through `locale -k LC_TIME`, and English until then or for a locale that isn't installed. `strftime` fills `%a %A %b %h %B %p` from it, and `^` uppercases a name. `months` is the form used inside a date; a calendar header takes `months_standalone`.
- `cx.audio().play_sound(path)` plays a PCM or float WAV file once as an event sound, and up to 4 wait for the audio connection. The shell's own sounds aren't in `streams()`.
- `drag_content(content)` drags as `drag_data` does and, outside its window, offers text as `text/plain;charset=utf-8` or URIs as `text/uri-list` to any program. `on_drop_content` takes text and URI drops from other programs and from the shell, URIs first, and `drop_hovered` covers them. They share a slot with `drag_data` and `on_drop`. Drags copy only and show no icon. A drag's `on_drag` keeps getting `Move` over its own window and none over another.
- The prelude adds `Http`, `HttpError`, `HttpErrorKind`, `Logind`, `Brightness`, `Backlight`, `Niri`, `NiriWorkspace`, `NiriWindow`, `NiriKeyboardLayouts`, `Clipboard`, `ClipboardEntry`, `ClipboardContent`, `FolderView`, `FolderEntry`, `SystemStats`, `Temperature`, `Locale` and `DragContent`.

API rules settled in Phase 19, the parity remainder:

- `cx.pipewire()` follows PipeWire's graph over its native protocol, connecting only once a config asks. `node_links(node)` takes an id or a closure and is a memo of the context `cx.pipewire()` was called on, so call it inside the row that shows it. `screencasts()` lists each `Video/Source` node with no `device.id` that links to another node.
- `network.add_connection(settings)` and `update_connection(uuid, settings)` take the JSON form `SavedConnection::settings` has, where an SSID or hardware address may be text. An update replaces every setting, and NetworkManager keeps the saved secrets only when it carries none. `network.scanner()` scans each Wi-Fi device every 10 s while the context that called `cx.network()` lives, so call it inside the popup that lists networks.
- Every widget takes `accessible_name(name)`, which a screen reader reads instead of what the widget shows; an icon button needs one to be read at all. While `org.a11y.Status` says a screen reader is on, the shell serves its widgets over AT-SPI; otherwise it holds no accessibility connection. `NO_AT_BRIDGE=1` turns this off. A screen reader reads a field's text and caret, a password field only as dots, and a slider's value, which it can also set.
- `cx.reload()` asks `crabshell run` for a reload, as `Quickshell.reload` does; without the supervisor it is logged and nothing happens.
- The prelude adds `PipeWire`, `PwNode`, `PwLink`, `PwLinkState`, `PwLinkGroup` and `Scanner`.

API rules settled in 0.9, from porting a Quickshell config:

- Every widget takes `mask_region(region)`, a value or a closure of `Option<Region>`, and keeps itself and everything inside it only within the region, given in its own logical pixels from its top-left corner, and within its box, with antialiased edges. With a style's `mask` too, both apply. It doesn't change where the pointer hits; a window's `mask` is what lets clicks through. A new region repaints the widget's painted area, never the layout. A panic in its closure is logged as `<path>.mask_region` and keeps the last region.
- `Gradient::center(x, y)` centers a radial or sweep gradient at fractions of its box, 0.5, 0.5 by default, and `Gradient::radius(r)` gives a radial one a radius in logical pixels instead of reaching the farthest corner from its center. Past the radius the end color holds, and a radius of 0 or less leaves only the end color. Both are `const`, and a linear gradient ignores them.
- `cx.capture(source)` takes one frame of an output or a window as a `Copy` `Capture` that goes with its context. `ScreencopyView::of(capture)` views share that frame and capture nothing themselves. `capture.ready()` reads `true` once it holds a frame taken since the last `capture.refresh()`, and the views keep the old frame meanwhile. A frame, about 8 MB for a 1080p output, stays until the capture's context goes, so make a capture for a passing effect in a child context. A capture of an output holds the shell's own windows on it, so hide a view that covers it before a refresh.
- `.crop(rect)` on any screencopy view shows part of its frame, given in the source's logical pixels as an output's windows are placed. `.shown(signal)` on any screencopy view is written `true` once the compositor presented the frame that first drew its picture, and `false` while it has no picture, takes a new one or is hidden; give each view a signal of its own.
- `TextMetrics::ink` is the box the glyphs' ink covers, from the text's top-left, as Qt's `tightBoundingRect`: size an icon glyph by `ink.width` and move it left by `ink.x`. Blank text gives an empty rect at 0, 0.
- `TextInput` and `TextArea` take `max_length(n)` and `read_only(on)`. A limit cuts typing, pastes and input-method text between graphemes, and cuts text set from outside, writing the cut text back to the signal. While read-only, keys that would edit go on to the window's `on_key`; the caret, selection, Ctrl+C and `on_accept` still work, and input methods don't see the field.
- `selection_color(color)` and `selected_text_color(color)` color a field's selection and its selected text; `None` keeps the defaults, the text's color at a third of its alpha and the text's own color.
- `SessionLock::on_key(handler)` gets the keys a lock surface's focused field or button left, as `PanelWindow::on_key` does; Escape clearing the password is the usual one.
- A panel's `width` or `height` may read a child's `geometry`, as in `.height(move || (body.get().height + 16.0).ceil() as u32)`. The panel is laid out at the size it asks for before it is shown, and again whenever that size changes, so its first frame and every later one are drawn at the content's size. Give that child `Style::new().shrink(0.0)`, so a panel smaller than its content can't squeeze it. A side a panel sends as non-zero keeps that size through any configure; only a side sent as 0 takes the compositor's.
- `cx.notifications().on_notification(handler)` hears each notification as it arrives or is replaced, before `popups()` shows it, while the asking context lives; `hide_popup` inside it keeps it out of the popups, for do not disturb. `set_default_timeout(duration)` sets what `Timeout::Default` means, 5 s until set, for popups that arrive after it. `hold_popup(id, held)` stops a popup's countdown, as for the pointer over it, and `popup_time_left(id)` gives the time left and the whole timeout for a countdown bar; it isn't a signal, so a binding that `cx.frames` ticks reads it.
- `DesktopEntry::command` is what `launch` runs, the program then its arguments, with field codes filled in as for a launch with no files, or empty for an `Exec` that can't be split.
- `Slider::limit(50.0)` caps what a drag, the wheel, the keys or a screen reader can set while the track shows the whole range, and `wheel_step(5.0)` moves 5 a notch or an arrow key while `step(1.0)` keeps drags on whole values. With a step, the wheel step rounds to whole steps, and a limit between steps stops at the step under it.
- `Button::pressed(signal)` is `true` from a left press or touch on the button until the release, wherever the pointer is then, as Qt's `MouseArea.pressed`.
- `Image::status(signal)` writes an `ImageStatus`, as Qt's `Image.status`: `Empty` for an empty path, `Loading` after each new source, and `Ready` or `Failed` once the picture is loaded, whether or not it shows. To fade a picture in, bind its opacity to `Ready`.
- `cx.sequence(initial, trigger, steps)` plays `steps` one after another each time `trigger`'s value changes, as Qt's `SequentialAnimation`; `trigger`'s first value plays nothing. `Step::to(value, transition)` moves from wherever the value is, and `Step::hold(ms)` keeps it there. Steps are `const`, so a shake lives in a `const` array. `cx.sequence_loop(initial, running, steps)` plays them round after round while `running` is true. Both give an `Animation` whose `running()` is true from each trigger, or while `running` holds for a loop, until the last step ends, holds included, so an effect on it runs what comes after, such as an unlock. A hold sleeps instead of drawing frames.
- The prelude adds `Capture`, `ImageStatus` and `Step`.

API rules settled in 0.9.1, from finishing that port:

- `cx.sequence_now(initial, steps)` plays `steps` once, starting when it is made, so a staggered entrance plays each time its widget is built: `Step::hold(index * 40)` and then a fade.
- `Button::on_any_press(|cx, button| ...)` runs at the press of any pointer button, as a click catcher that closes a panel needs. Only the innermost enabled button under the pointer gets it.
- `cx.notifications().restart_popup(id)` counts a popup's whole timeout again from now, as a replacement does; a held popup stays held.
- `SessionLock::refused(signal)` is written `true` when the compositor refuses the lock, as when another locker holds it, or ends it while `locked` is still true, and `false` once `locked` turns false, so a config can clear its own flag: `cx.effect(move || if refused.get() { locked.set(false) })`.
- The prelude adds `PointerButton`.

API rules settled in 0.9.2:

- `Image::from_source(source)` shows an `IconSource`, or a closure returning `Option<IconSource>` such as a notification's `image`: raw pixels, a file, or an icon name looked up at the box's larger side. `None` is `Empty`. Fit, filters, `transition` and `status` work as for a file. Pixels keep an `Icon`'s caps, 512 a side and `width * height * 4` bytes.

API rules settled in 0.9.3:

- An `Image` at opacity 0, or cut out by its mask region, still loads its picture at its drawn size, status or not, so binding opacity keeps pictures warm; `visible(false)` loads nothing without a status.
- A sequence's leading steps of 0 ms and no delay land in the flush that starts its play, so a jump-then-slide never shows the old value for a frame. A play of no time never turns `running()` true.

API rules settled in 0.9.4:

- `Image::decode_size((w, h))` caps the size a picture is made at in logical pixels (multiplied by the output scale) before filters run, then stretches it up to the box bilinearly, like Qt's `sourceSize`. A side of 0 leaves that side uncapped, a size at least the box's changes nothing, and `None`, the default, makes the picture at the box's size. The file is still read whole and the cache keeps the picture at the box's size, so it saves time, not memory.

## Process model and reload

Two processes run. The `crabshell` CLI, installed once per machine, is the supervisor. The shell is the user's own compiled binary, which links the `crabshell` library.

Started without the supervisor on glibc, `crabshell::run` first starts the program again, with `GLIBC_TUNABLES` fixing malloc's `mmap` threshold at 131072 unless it sets one already, as the supervisor does; otherwise a freed wallpaper's memory stays in use. So code in `main` before `run` runs twice: keep `main` to the `run` call. In secure mode, as a setuid program, or when it can't start again, it logs why and runs on as it is. The logged time to the first frame counts from the first start, about 3 ms more.

The CLI has four commands; v0.1 had the first three:

- `crabshell init` copies the template into `~/.config/crabshell/`. It refuses if the directory already exists.
- `crabshell run` builds the config, starts the shell, watches for edits and supervises.
- `crabshell check` runs `cargo check` and prints errors, for editors and scripts.
- `crabshell ipc [<target> <function> [args...]]` calls a running shell's `cx.ipc` handlers, or lists them (Phase 10).

What `crabshell run` does:

1. Sets `CARGO_TARGET_DIR=~/.cache/crabshell/target` and runs `cargo build` in the config directory.
2. Listens on a Unix socket in `$XDG_RUNTIME_DIR/crabshell/`, then starts the built binary with the socket's path in `CRABSHELL_CONTROL`. The shell connects and writes `ready` once every panel it shows has committed its first frame, or, with none shown, once its startup judgement passed. (A path rather than an inherited descriptor: adopting a raw descriptor number is `unsafe` in Rust, and the crates forbid `unsafe`.)
3. Watches `src/` and `Cargo.toml` with the `inotify` crate, debounced by 300 ms. (`notify` is CC0-1.0, which the license policy doesn't allow.)
4. On a change, runs `cargo build` again. If the build fails, the running shell stays up. The supervisor prints the full error and sends the first error line as a desktop notification over `org.freedesktop.Notifications`.
5. If the build succeeds, the supervisor writes `handoff` to the socket. The shell writes every `cx.persist` value to `$XDG_RUNTIME_DIR/crabshell/state.json`, answers `saved` and keeps running. The supervisor starts the new binary beside it, which reads that file, and once the new shell says `ready` writes `exit` to the old one. The new shell gets 10 s to say `ready`; if it ends or misses that, the old one keeps running and the supervisor sends a notification, as for a failed build. Once the old shell has gone, the supervisor writes `alone` to the new one.
6. If the shell exits with any other code or dies from a signal, the supervisor restarts it after 1 s, doubling the wait each time up to 30 s. Three crashes within 60 s of a new build make it start `~/.cache/crabshell/last-good` instead and send a notification.
7. A binary that has run for 60 s after it said `ready` is copied to `last-good`. A lock-only config or a hidden launcher says `ready` once its startup judgement passes. A shell that never says it, such as one waiting for its first output, is never copied.

Rules settled while building `crabshell-cli` (Phase 5, step 2):

- The config lives in `$XDG_CONFIG_HOME/crabshell` and builds in `$XDG_CACHE_HOME/crabshell/target`, falling back to `~/.config` and `~/.cache`. A relative path in either variable is ignored, as the XDG spec says.
- `crabshell init` refuses if the directory exists at all, even empty, so it never overwrites a config. It creates the directory with a call that fails if it appeared meanwhile, and removes a half-written config if a write fails, so a retry isn't blocked. Without `--path` the config depends on `crabshell` from crates.io. `crabshell init --path DIR` builds against the Crabshell checkout at `DIR` instead. A `DIR` without `crates/crabshell/Cargo.toml` is refused.
- The template in `crates/crabshell-cli/template/` is "Writing a config"'s two blocks word for word, and a test compares them, so the doc and the template can't drift apart.
- The template line `--path` replaces is built from the CLI's own major and minor version, which the library shares, so a release bump that misses the template fails a test.
- `crabshell check` runs `cargo check` on the config from the config's own directory, so another project's `.cargo/config.toml` or `rust-toolchain.toml` doesn't apply, with the cache's target directory. Its output passes straight through, and it exits with cargo's status. `CARGO`, when set, names the cargo to run.
- A bad command line, including an argument that isn't UTF-8, prints the usage and exits with 2. A command that fails explains why and exits with 1. A `--path` that isn't UTF-8 is refused, and control characters in it are escaped in the TOML.

Rules settled while building `crabshell run` (Phase 5, step 3):

- The supervisor runs on one thread, on a calloop loop like the shell's. It learns the shell exited from a pidfd (`rustix`'s `pidfd_open`, so Linux 5.3 or later), which sleeps until then instead of polling. A shell that is up and quiet wakes the supervisor once, 60 s after it says `ready`. The 60 s count from its first `ready` after each start, and a later `ready` changes nothing.
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
- To reload, the supervisor writes `handoff`, and starts the newest build once the shell answers `saved`. A shell that never said `can-hand-off`, as one from 0.7, gets `save` and is replaced when it exits. A shell that never connected gets SIGTERM, and one that hasn't answered or ended 5 s after the ask, or exited 5 s after `exit`, gets SIGKILL.
- A new build starts over: no crashes counted, and it replaces `last-good` if that was running.
- Each notification opens its own session-bus connection, which closes after the call, so an idle supervisor stays asleep. A server gets 25 s to answer, timed by a loop timer. A failure, such as no notification server, is logged. The body escapes `&`, `<` and `>`, since servers read it as markup. The supervisor notifies when an edit doesn't build, when the first build fails and `last-good` starts, and when it falls back to `last-good`.
- The supervisor logs each build's duration, and the time from the old shell's `saved` to the new one's `ready`.
- A build blocks the loop. A signal that arrives meanwhile is handled when it ends.
- Every crate's tests run their buses on a private `dbus-daemon` whose config lists no service directories, since `--session` would start real services, such as the desktop's notification daemon. `crabshell-cli`'s other tests get a session bus address that leads nowhere, and none of its tests passes on the desktop's `DISPLAY`, `XAUTHORITY`, `WAYLAND_DISPLAY` or `SWAYSOCK`: libpulse alone reads X's root window, which would start the desktop's Xwayland.

Rules settled for IPC and the supervisor (Phase 10):

- The shell serves `cx.ipc` handlers at `$XDG_RUNTIME_DIR/crabshell/ipc-<display>.sock`, named for the last part of `WAYLAND_DISPLAY` (`wayland-0` without it), in the directory `persist` makes private. It binds only at the first registration, replaces a file already there, and removes it at exit only if its device and inode are still its own, so an old shell never removes a newer one's. A newer registration under the same names replaces the older and outlives the older one's owner.
- One request per connection: a line of JSON, `{"target","function","args"}` or `{"list":true}`, answered with `{"ok"}` or `{"error"}`. Requests are capped at 64 KiB and replies at 1 MiB; at most 16 connections are open, each closed 5 s after it opened. Connections are edge-triggered, so one waiting for its request sleeps. An accept that fails while a connection waits, as with no file descriptors left, rests the listener for 1 s instead of spinning. A panicking handler, or the drop of one replaced, is logged with where it was registered; the handler is disabled and the client told.
- `crabshell ipc` finds the socket the same way, gives the exchange 5 s, reads the reply to its newline, and exits 0, 1 for an error or no shell, or 2 for bad arguments.
- The supervisor starts the shell with glibc's `mmap` threshold fixed at 128 KiB (`GLIBC_TUNABLES=glibc.malloc.mmap_threshold=131072`, added to the user's own unless that sets it). Without it, glibc raises the threshold to the size of each large block freed, so a second decoded wallpaper's buffers stay in the heap: 41 MiB for a 4K image, measured. The shell's own spawns take it back out, and the supervisor's socket variable too.
- Before it exits for a reload or a lost compositor, the shell stops every program `cx.process` started, since `exit` skips the drops that would. On SIGTERM or SIGINT, as from `crabshell run` on Ctrl-C or a session ending, it does the same and removes its IPC socket, then dies by that signal as before. The handlers come from `signal-hook` and only write to a pipe the loop reads: blocking the signals for a signalfd would block them in every program the shell starts too, which inherit the mask.
- `cx.exit(code)` does the same, then exits with `code`. glibc's `exit` drops the services as well, which stops the programs, but Rust doesn't promise that, so the stop stays explicit.

Rules settled for the reload handoff (Phase 19):

- An edit or `cx.reload()` while the new shell isn't ready ends it, asks the old shell to save again, and starts the newest build beside it once it has. A new shell that fails leaves the old one running, and the next crash starts the newest build afresh, counting toward `last-good`. The 10 s deadline holds only while the old shell runs.
- The supervisor learns which shell a control connection belongs to from `SO_PEERCRED`, and writes `alone` when the old shell's connection closes after `exit`, about 1 ms after it exits. A shell started beside an old one gets `CRABSHELL_BESIDE=1`, which `cx.process` programs don't inherit, and only such a shell waits for `alone`.
- The new shell says `ready` only once it has taken the notifications over, so none is lost or doubled. SIGTERM during a swap ends both shells, then `run`.
- Told to stop, the supervisor sends each shell SIGTERM and SIGKILL 5 s later. If the old shell exits while the new one is being replaced, the newest build starts at once rather than as a crash's restart.
- A program `cx.process` starts in the new shell waits for `alone` too, so the old shell's run is never doubled.

Panic handling inside the shell:

- Builds use `panic = "unwind"`. The toolkit wraps every binding, effect, event handler and service callback in `catch_unwind`.
- A panicking binding is disabled and logged with its widget path, for example `PanelWindow[DP-1]/Row/Button[2]/Text.text`. The rest of the shell keeps running.
- The reactive core releases every `RefCell` borrow before it calls user code, so a caught panic never leaves the runtime borrowed. A reactive-core test checks this.
- A panic in the toolkit's own Wayland or rendering code is not caught. The process exits and the supervisor restarts it.

Logging goes through `tracing` to stderr. The supervisor passes the shell's stderr through to its own, so `journalctl` sees both when `crabshell run` runs as a systemd user service.

During a reload's overlap a new shell's panels reserve no space and it holds nothing only one process may hold, such as the polkit agent, the IPC socket, socket servers, global shortcuts, a session lock and `cx.process` programs, until `alone`. So windows stay still and nothing is refused, except for one arrangement as the old bar goes, since two programs can't hand a layer-shell zone over at once.

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

Required tests, all passing in `crates/crabshell-reactive/src/tests.rs`:

- [x] Diamond. Signal `a` feeds memos `b` and `c`, and one effect reads both. One `set` on `a` runs the effect exactly once.
- [x] A memo that recomputes to an equal value doesn't rerun its dependents.
- [x] After a branch switch, setting the signal the old branch read doesn't rerun the effect.
- [x] Disposing an owner frees every arena slot it created. Count slots before and after.
- [x] A self-feeding effect stops after 100 rounds, and the other effects in the same flush still run.
- [x] A panicking effect is disabled, the next `set` and `flush` work, and no `RefCell` is left borrowed.
- [x] `try_get()` on a disposed handle returns `None`.

Added after the Phase 1 review on 2026-11-10:

- The `fn(T) -> T` marker stops Rust from quietly converting a `Signal<A>` into a `Signal<B>`. Every handle implements `PartialEq`, `Eq`, `Hash` and `Debug` by key, with no bounds on `T`.
- `owner.effect_scoped(|scope| ...)` gives each run a fresh child owner. The runtime disposes it before the next run and when the effect is disposed, so children rebuilt on every run don't pile up. A plain `effect` frees nothing between runs.
- `untrack(|| ...)` runs code without recording dependencies, so a widget built inside a binding doesn't make that binding depend on what the widget reads.
- `try_set()` and `try_update()` return `false` and do nothing on a disposed handle, for callbacks that may outlive their window.
- A memo must not write signals. A write or a flush from inside a memo panics, and the memo is disabled and logged.
- A panic in the `Drop` of a user value or closure is caught and logged like any other panic in user code.
- `flush()` repeats rounds until nothing is queued. An effect created during a flush first runs later in that same flush; otherwise a new effect first runs at the next flush.
- At thread exit, once the runtime is torn down, every handle acts as disposed: `set()`, `update()` and `dispose()` do nothing, and `try_get()` returns `None`.
- `owner.is_disposed()` says whether an owner was disposed, itself or with its parent, so a handle can refuse quietly without making a node to find out (Phase 12).

More required tests:

- [x] An effect whose captured value panics when dropped is disabled, and later flushes still run every other effect.
- [x] Exiting with an owner still alive, by returning from `main` or calling `process::exit(0)`, gives exit status 0.
- [x] Rebuilding children 10 times with `effect_scoped` leaves the arena's slot count where it was after the first run.
- [x] Disposing 50 child owners that read one long-lived signal returns that signal's subscriber count to where it started.

## Widgets, layout and styles

v0.1 shipped ten widgets, Phase 7 added five, Phase 10 one, Phase 15 three, Phase 16 one, Phase 17 one, and the phases after it add the rest a full shell needs (see "Beyond v0.1"). All but one are in `crabshell-ui`. `Tray` is in `crabshell-services`, because it needs the tray service and `crabshell-ui` can't depend on services; the prelude re-exports it. Users build everything else by combining them.

| Widget           | What it does                                                                                                                                                                            |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `PanelWindow`    | A layer-shell surface on one output with anchors, size, exclusive zone, layer, margins, keyboard focus and an input mask.                                                               |
| `FloatingWindow` | An ordinary `xdg_toplevel` window with a title, size limits, maximized, fullscreen, `visible` and an input mask, shown with `cx.floating`.                                              |
| `PopupWindow`    | An `xdg_popup` anchored to a widget's rectangle, with an input mask. Tray menus use it.                                                                                                 |
| `Row`, `Column`  | Flex layout with `gap`, `padding`, `align`, `justify`, `wrap` and `reverse`.                                                                                                            |
| `Stack`          | Children drawn on top of each other in one cell, or placed absolutely by their style's insets.                                                                                          |
| `Grid`           | Children row by row in equal columns, spanning cells by their style.                                                                                                                    |
| `Spacer`         | Takes the leftover space in a `Row` or `Column`.                                                                                                                                        |
| `Text`           | One line by default, with `elide` at the start, middle or end, `max_lines` and `selectable`, or styled spans with `rich` and links through `on_link`.                                   |
| `Icon`           | A freedesktop icon name, an SVG or PNG path, or raw ARGB pixels from a tray item, tinted or as it is.                                                                                   |
| `Button`         | A clickable box with hover, pressed and focus styles, a cursor and `enabled`, plus click, drag, long-press, wheel, key and drop handlers.                                               |
| `Image`          | A PNG, JPEG, GIF, WebP or SVG file, or an `IconSource`, in its box, filled, contained or covered, with filters, `decode_size`, an animated GIF playing and transitions between sources. |
| `Canvas`         | A box a paint closure draws paths, arcs, curves and dashed strokes in, cut to its corners.                                                                                              |
| `ScrollView`     | Shows part of a taller or wider child, or both, cut to its box, scrolled by the wheel, the touchpad, a drag, its scroll bars or its offset signals, with flings and overshoot.          |
| `ListView`       | A scrolling list, down or across, that builds only the rows in view, with rows of any height, a `current` row kept in view and a highlight.                                             |
| `GridView`       | A scrolling grid of cells of one size, down or across, that builds only the rows in view, with a `current` cell and a highlight, for pickers.                                           |
| `TextInput`      | A one-line text field with a caret, drag-select, placeholder, password dots, the clipboard and input methods.                                                                           |
| `TextArea`       | A multi-line editor that wraps or runs sideways, with the field's caret, selection, keys, clipboard and input methods.                                                                  |
| `Slider`         | A handle on a track that writes back a value in a range, dragged, scrolled or moved by keys.                                                                                            |
| `Menu`           | A vertical list of buttons built from a DBusMenu layout, with separators, checkmarks and submenus as nested popups.                                                                     |
| `Tray`           | One icon button per tray item, each opening its `Menu`. Built only from the widgets above.                                                                                              |
| `ScreencopyView` | An output or a window, captured once or live, sized by its style.                                                                                                                       |
| `SessionLock`    | A lock screen with a surface on every output while `locked`, shown with `cx.session_lock`.                                                                                              |

### Layout

- `taffy` computes flexbox and grid layout. `Row` and `Column` map to a flex direction, `Stack` maps to a one-cell grid, and `Grid` to a grid of equal columns. A style's insets place a widget absolutely in its parent.
- `Text` and `Icon` report their size through taffy's measure function. Text measuring uses the same `cosmic-text` buffer the renderer draws from, so measured and drawn sizes can't disagree.
- Every unit is a logical pixel. The renderer multiplies by the output's scale.
- A `PanelWindow` anchored left and right gets its width from the compositor's `configure` event. Layout runs again after every configure.

### Invalidation

- Each property is either a layout property or a paint property. Text content, font, padding, gap, size and children are layout. Colors, background, gradient, border and its dash, opacity, radius and a mask are paint. A shadow reaches past its widget, so changing one lays the window out again to find the area it paints.
- A binding that changes a paint property marks only its own widget for repaint. A layout change marks the whole window for layout, then every widget whose rectangle moved is repainted at both its old and new position.
- When nothing is dirty there is no layout, no paint and no commit.
- Translate, scale, rotate and origin are paint properties that move the area painted. A change repaints the old area and places the window again without taffy, which repaints the new one.
- A scroll offset is a placement property: a change places the window again without taffy and finds the hovered button again, as a layout does. A list lays out only when the rows it builds change.

### Styles

- `Style` has `background`, `gradient`, `shadow` (outside the box, or inside it with `inset`), `color`, `font` (family, size, weight, italic, letter spacing and OpenType features), `text_align`, `line_height`, `underline`, `strikethrough`, `padding`, `radius` (each corner's, through `radii`), `border` (width, color), `border_dash`, `opacity`, `mask`, `clip`, `width`, `height`, `min_width`, `min_height`, `max_width`, `max_height`, `grow`, `shrink`, `basis`, `align_self`, `justify_self`, `left`, `top`, `right`, `bottom`, `span`, `z`, `translate`, `scale`, `rotate`, `origin` and `transition`. Every field is optional, and an unset field falls back to the widget's default. `clip`, the sizes, the flex and grid fields, the insets, `text_align` and `line_height` are layout properties; `z`, `underline` and `strikethrough` are paint properties.
- `Text` inherits `color`, `font`, `text_align`, `line_height`, `underline` and `strikethrough` from the nearest ancestor that sets each. Nothing else inherits, and there are no selectors or classes.
- Colors are sRGB with alpha. `Color::rgb`, `Color::rgba` and `Color::hex` are all `const fn`.

### Input

- Pointer events come from `wl_pointer`, and a touchscreen's first finger from `wl_touch` as a left button. Hit testing walks the tree from the top, in z order, and picks the deepest enabled `Button` under the pointer.
- The cursor shape is set through `wp_cursor_shape_v1`: the button's `cursor`, the pointing hand by default, and an I-beam over text fields and selectable text.
- `on_scroll` receives whole steps. Mouse wheels use `axis_value120`. Touchpad scrolling is summed and becomes one step per 15 logical pixels. `on_wheel` receives every event on both axes, with its pixels, notches, source and the gesture's end.
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
- The focus moves only by Tab, a click on a field, or a field's or, since Phase 15, a button's `focus` turning true; a click on a button doesn't take it. The focus shows only while the window has the keyboard. Removing the focused widget drops the focus.
- A focus style that moves buttons checks the hover again after the layout.
- A clip cuts its children to the inside of its border, following the corners, and cuts their bounds to its rectangle. A clip that comes, goes or changes shape repaints its old and new bounds, and the pointer can't reach children under its border or, since Phase 16, past its corners.
- A scroll view lays its child out at full height, never shrunk, and shows it moved up by the offset clamped to the content. The wheel scrolls 48 logical pixels a notch, the touchpad by its pixels, starting from the signal so scrolls before the next layout add up. A button with `on_scroll` under the pointer takes the scroll instead.
- A list builds the rows in view plus one each side, reached through a one-item keyed list for its context. A keyed item whose builder panics is logged and shows empty, keeping its place.
- A text field edits by grapheme and word through unicode-segmentation, places its caret and selection with cosmic-text so right-to-left text works, scrolls sideways to keep the caret in view, and is cut to its content box. Text set from outside puts the caret at the end, and line breaks become spaces. Ctrl+C and Ctrl+X copy the selection, Ctrl+V pastes, and a password field pastes but never copies. Enter runs `on_accept`, or without one goes on to the window.
- An input method sees the focused field only while the window has the keyboard and never a password. Its preedit shows underlined at the caret, replacing the selection, and moving the focus drops it.
- A floating window's settings are read after the first flush, like a panel's. Move and resize requests are honored only while a pointer button is held.

Rules settled while building `crabshell-ui` (Phase 10):

- A screencopy view draws nothing until its first frame. A new source drops the frame it showed, and a frame the same as the one shown repaints nothing, so a live view of its own output settles. `Window::captures` lists the views shown with a box, for the platform. A live view whose painted area lies wholly outside the window or a clipping ancestor, such as a scroll view, at the last layout is left out until it shows again; an ancestor's painted area stands for its clip. A one-shot view stays listed. A small view sharing a session with a large one gets the frame cut for the largest; the renderer keeps one scaled copy per view, scaled again only when its frame or box changes.

- A button's `hovered` signal is `true` while the pointer is over a node on the path from the window down to the hovered button, so leaving an inner button for its outer one's text keeps the outer one hovered. It is written only on a change, once per signal for buttons sharing it, and goes `false` when the pointer leaves the window, the button is removed, its window is dropped, or a floating window is hidden. The signal belongs to the config: the button only writes it.

Rules settled while building `crabshell-ui` (Phase 8):

- One clock on the UI thread drives every animation, and the platform advances it only while one runs. An animation's first tick is its time 0. One that replaces a running one counts from the clock's last tick, so it moves at the next tick, and the first animation after a rest counts from its own first tick. A transition of 0 ms sets the value in the same flush.
- A new target while a value moves starts a new animation from where the value is. An animation whose value's owner is gone stops. One that panics is logged as `animation cx.animate at <file:line>` or `<widget path>.transition` and stops, and finished ones are dropped inside `catch_unwind` too.
- A style fades only on a paint change, on a widget already laid out, when the new style's transition is longer than 0. It blends from what showed, so leaving a hover mid-fade fades back from there. A color only one side sets fades from or to its transparent self, a border from or to 0 wide, a radius from 0 and an opacity from 1. A text color only one side sets, and gradients of different shapes or numbers of colors, or where only one sets a radius, change at once. A fading widget counts as drawing, so a layout that moves it repaints both places.
- A shadow's painted area is its shape grown by 1.5 blurs, past which the blur fades below a 255th. A fading shadow's area covers its start and its end, and a spread that swallows the box still reaches between the edges it would have. A shadow with a number that isn't finite is never drawn.
- `Gradient::stop` panics past eight colors, which is a compile error in a `const`.

Rules settled while building `crabshell-ui` (Phase 13):

- Padding is set per side. A row's or column's own padding wins on both sides of its axis, and a list fits its rows between its top and bottom padding.
- A popup is the size of its content, in whole logical pixels and at least 1×1, and no larger than a smaller size the compositor configured it at; it still asks for its content's size.
- A `compositor_opacity` outside 0 to 1 is clamped, and NaN reads as 1, logged once per window with its path. Popups carry the same window hints as panels but use only the mask, compositor opacity and visible mask. `f32` takes a plain value.

Rules settled while building `crabshell-ui` (Phase 14):

- `fire_timers(now)` runs the timers due at the call, earliest first, each with the table released, so a callback may stop, restart or make timers, itself included; one made during the call fires at a later one. An interval's late fire keeps the schedule, and after a stall longer than a period the next fire is one period from then. A period under 1 ms becomes 1 ms. A callback that panics is logged as `timer cx.interval at <file:line> in <path>` and stops, and `restart` runs it again. A timer whose context went is freed at the next `next_timer` or `fire_timers`, its callback dropped inside `catch_unwind`, and never arms the loop. `Owner::name` gives the nearest debug name for that line.
- A spline that is empty, goes back in x, doesn't end at (1, 1) or holds a number that isn't finite moves linearly. A delay counts from the animation's time 0, and while it waits nothing is written or repainted; a 0 ms transition with a delay jumps when it ends. `running()` changes only on an edge. A panic in a target, a `running` closure or a config's own `lerp` is logged as `animation cx.<fn> at <file:line>`, stops that animation and leaves `running()` false.
- A cycle's legs each take `ms`, and its delay comes once. Stopped, it finishes its round and rests at 0; started again before then, it never stops. A 0 ms cycle never moves. A spring steps by implicit Euler, at most 1 ms a step and 0.25 s a tick, rests within 0.001 of its target below 0.01 a second and snaps onto it, and jumps to a target or spring that isn't finite. Stopping `frames`, or a retarget, leaves one tick before the clock stops.
- A widget is scaled and turned clockwise around its origin, the middle by default, then translated, and a child's transform composes with its parent's. A number that isn't finite counts as unset, a scale of 0 draws nothing and is never hit, and a negative scale also turns it upside down. The painted area is the bounding box of the transformed one, grown by a logical pixel at each transformed level. `NodeView::bounds` is in the parent's coordinates and `inner_bounds` in the node's own.
- The pointer is mapped back through each transform. Children are searched within the subtree's area, so a child moved or overflowing outside its parent's box is hit where it is drawn, and a widget itself only inside its own box; a clip still cuts both. In 0.3 an overflowing child couldn't be clicked. Hover is checked again when the pointer moves and after a layout, never after a transform alone places the window, so a hover transform that moves its button off a still pointer stays hovered until the pointer moves. A text field tells input methods where its caret is drawn.
- A leaving child stays right after the child before it in the last order, and a first one stays first. A key that comes back while leaving keeps its child and state, takes its new place and frees its exit's timer. Each leave gets a one-shot `cx.after` under a child owner of the item, so disposing the list frees every item and timer at once. An exit binding that panics is logged with the list's path, and the child goes at once. Every root `Cx` holds one `false` leaving signal that its contexts share.
- Each item of a list with an exit counts what runs on the animation clock inside it, and inside the items within it, from when a step starts until it is dropped. A leaving item's watch keeps the clock ticking until that count is 0, judged at the start of each tick before anything moves, or until its exit's owner goes. `animating()` is true while a watch waits. A key that comes back drops its watch with its exit's owner.
- A style fade counts as fading back when the style in effect differed from the button's own before the change and equals it after.
- Inside another exiting item, an item's leaving signal is kept by an effect, so it turns true in the same flush as the outer one.

Rules settled while building `crabshell-ui` (Phase 15):

- `Justify::Start` and `End` map to taffy's flex start and end, so `reverse` swaps them. Wrapped lines pack at the start, as Qt's `Flow` does, rather than stretch as CSS's do, and a grid's rows pack at the top. A grid's columns are `minmax(0, 1fr)`, so a child too wide for its cell overflows it. Column counts and spans are clamped to 1 through 65535. A scroll view's child keeps a shrink of 0 over its own `shrink`.
- `NodeView::children` gives the shown children in z order, so render draws them in that order with no change of its own, and hit testing tries them in reverse.
- A hidden widget is taffy's `Display::None`. Its subtree's placed rectangles are forgotten, so nothing in it is drawn, hit, captured or anchored to; a popup it opened loses its anchor, as before a first layout. Its old bounds repaint once, and shown again each node in it is placed and painted as a new one is. `Tree::focus` refuses a node that is hidden or inside a hidden widget, whoever asks. A field remembers its `focus` binding's last value, and the layout after a widget is shown gives the focus to the first such field in it, after the flush, so a `focus` that turned false in the same flush keeps the focus away.
- `Window::layout` writes each `geometry` signal only when the rectangle differs from the one it last wrote, and only after the tree is released. Then it flushes and lays out again until nothing moves, at most 8 times. Past that it logs `<window path> still moved after 8 layouts that wrote geometry`, and the feedback goes on at the next layout. A layout that writes nothing runs no effects, and a box sized from its own geometry settles within one call.
- The window's pointer state is shared by the mouse and touch. A press records its button, where it went down in the window and in the button, and whether it dragged or long-pressed. Double clicks compare release times, wrapping, and press places. With no time given, a process clock stands in.
- Each button with `on_long_press` keeps one `cx.after` timer under its own context, restarted on each left press and stopped by the release, a leave, a move past the threshold or the button being turned off, and freed with the button.
- Hit testing skips disabled buttons, and the `hovered` and `pointer_position` signals leave them out. A drag holds the hover and the inside signals on its button and the buttons around it; after the release, hover follows the pointer again.
- A displaced slide starts at the layout after the list changed. Its offset is where the child was drawn against its parent's old corner, less where it now sits, so a parent that moved too carries it at once, and a child that didn't move keeps the slide it had. Each start counts up the node's slide number, so the slide it replaced stops at its next tick, as does one whose node or window went. The slide comes after the style's transform, in the parent's coordinates, so it adds to `translate`. Every child of the container slides, static ones included. A `displaced` binding that panics is logged with the list's path, and the children jump.
- A drag's value lives in a signal under the source's owner, never in the tree. The target is the deepest enabled button under the pointer whose `on_drop` takes the value's type and hasn't panicked, skipping the dragged button and everything inside it. It is found again on every motion, after every layout and every pass that only places the window while a drag is held, and when the drag starts, ends, or its source or target is removed, hidden or turned off. At the release, the source's `on_release` runs, then the target's `on_drop`, then the source's `End`. A source turned off, removed or taken out of the window drops nothing, and a target whose `on_drop` panicked takes no more drops.
- The slider is made of a `Button` with `on_drag`, `on_wheel` and `on_key` around boxes whose `grow` splits the free space by the value, so it needs no node kind or drawing of its own; a `vertical` closure switches between a row and a column body with `visible`. It keeps a value under its own context whose drop writes `dragging` false if it was true.
- A scroll goes to a button with `on_scroll` or `on_wheel` under the pointer first, then to the innermost scroll view under the pointer that can still move that way, then, for a touchpad, to the innermost view it can push past an end or the one it already pushed. When the fingers lift, the view the gesture moved goes back from past an end or on at its speed, even when a button that scrolled under the pointer takes that last event.
- A left press stops every scroll view under it whose content the pressed button, if any, lies in, and a tap that stops a fling still clicks. A move past 10 px along a view's axis drags that view's child: the innermost along that axis that can move, else one that can overshoot; the pressed button then lets go without `on_release` or a click. A button that drags, with `on_drag` or a `drag_data` holding a value, keeps its drag. A press on a button drawn over a view from outside goes to that button alone, never to the view's bar or a drag, and a press on the bar stops the view as any press does.
- A fling's speed is the moves of the last 100 ms before the release or the touchpad's stop; under 50 px/s it doesn't fling. Each axis's fling or spring stops when it slows to 0, on a write to that axis's offset from outside, on content that shrinks under it along that axis, on a press or wheel, or when its view or window goes. The other axis goes on. Overshoot is shown as `limit × (1 − e^(−0.5 × raw / limit))`, with a limit of a quarter of the box, springs back at a stiffness of 150 and a damping of 1, and rests within 0.5 px and below 10 px/s. It moves the child in the place pass only, as the offset does, never through a layer. The pointer leaving mid-gesture or mid-drag springs back without a fling.
- The scroll bar's handle is as long as the shown part, at least 16 px, placed by the offset shown, and drawn inside the view's clip after its children. It repaints where it was and is when it moves, comes or goes.
- A list row is measured after each layout that resizes it. A newly shown row costs one layout, or a second in the same call when its length differs from what it was counted as. A row that starts above the view grows upward and the offset moves with it, so the rows shown stay put, and a running fling carries on from the moved offset. A row measured in the flush that changes the items keeps its length, moved to its new index. The list calls `key` on every item when the items change and keeps each key's index.
- `current` on a row not built yet scrolls by the estimate, then shows the row whole once it is measured. Items that keep it in range don't pull the view back, and neither do rows measured later. While the view moves in a fling, a drag of its content, a touchpad gesture or a spring-back, as Qt's ListView with its tracked item, a change of `current` scrolls nothing, and the motion's end scrolls nothing either; a drag of the scroll bar's handle counts as moving until the button comes up.
- A highlight slides its place and size only when its target changes, a new current index or new items. A move from a measurement or a new column count jumps when nothing slides, and while a slide runs it restarts the slide from where the box is.
- A grid's first show costs two layouts, since its width is known only after one. A new column count finds the top-left cell where the old columns showed it and keeps it in view, then the current one; a grid keeps `current` in view when it, the height, the cells' height or the column count changes, never for a width that keeps the columns.
- A grid counts its cells across from its scroll view's own laid-out box, less padding and border, and that box has no automatic minimum across, so a grid narrows or shortens past its cells and recounts when turned.
- A view that scrolls both ways lays its child out as a row that doesn't stretch it, at its own width and height.
- A drag picks its view by the way it mostly goes, then moves every axis that view scrolls along. A mouse press doesn't drag a view whose `mouse_drag` is off; a finger does.
- Each axis keeps its own offset, overshoot, fling and spring. The repeat timers of a held track are made at the view's first track press, under its owner, and run only while the track is held.
- `mapped_geometry` is the laid-out rectangle mapped through each transform on the way to the window, without the pixel of bleed or the shadows that damage adds.

Rules settled while building `crabshell-ui` (Phase 16):

- A canvas's closure waits for the first layout, then gets each new size through the layout's measure hook, after the tree is released. A run keeps at most 16,384 path commands and dash lengths, and its strokes may make 100,000 dashes in all. A drawing holding a number that isn't finite or is beyond 1e6 logical pixels, a negative radius or width, or a dash pattern of negative or non-finite lengths is skipped, and so is one past either budget; the first one skipped is logged once per canvas. A path's length for the dash count starts at its first point when it doesn't start with a move, as it is drawn.
- `hsl` follows CSS Color 4's conversion; the hue wraps, the rest clamp, and a number that isn't finite gives opaque black. A NaN passed to `with_alpha`, `mix`, `lighter` or `darker` leaves the color unchanged, and their amounts clamp to 0 through 1. Every 8-bit color survives the trip through HSL and back.
- A clip hides its children from the pointer past its corners as well as under its border; the inside curve is the corner less the border. In 0.4 the pointer still reached children in a clip's cut-off corners. A fade moves each corner on its own, and an unset side counts as square corners.
- Each image has an id from a process-wide counter. `Window::repaint_image(id)` repaints that image's box and returns false for an image that is removed, hidden or not laid out.
- A content shadow is a step on the way from a node out to the window, inside its transform, so it turns with the widget: every descendant's painted area and damage grow by its reach, and the node's bounds include it, a fading one between its start and its end. A node that casts or cast a content shadow repaints its old and new bounds when it changes, as a clip does, so the shadow of children that overflow or are scaled follows an instant change. A mask leaves bounds alone; one that comes, goes or changes repaints the subtree's bounds without a layout.
- An image's transition runs on the animation clock while it moves and repaints the image's box each tick, nothing during its delay. Its end lets go of the old picture with one last repaint, and a replaced transition or a window that goes stops it. It is logged as `<image path>.transition`. An image node's drop records its id, and `dropped_images()` hands each one out once, so a renderer can let go of what it kept for it; past 4,096 unread ids the list stops growing.

Rules settled while building `crabshell-ui` (Phase 17):

- Italic is asked of fontconfig as slant italic, and the family and fallback caches are keyed by slant as well as weight; cosmic-text slants an upright face that fontconfig gives for an italic request. Letter spacing is handed to cosmic-text in ems of the size used.
- A text's painted area covers the width it was laid out in. With a line height under the default, it grows above and below by half the difference.
- There is one `TextSystem` per process, a cloned handle to the same fonts, given to the root `Cx` as a service. Only layout and drawing borrow it, and neither runs user code, so a binding may measure text during any flush.
- Adding a font removes the faces loaded from files that it replaces, the same family, slant and weight. Their files stay read, so those faces never come back.
- Selecting is a drag of the pressed field or text: it holds the hover, and scroll views don't drag. Double clicks on text are timed press to press, since a drag's release doesn't click. A selectable text keeps its selection as two byte offsets, and only the focused text holds one.
- A caret at a byte where a wrapped line ends and the next starts shows at the start of the next, unless End or a click past the end put it there; any other move forgets that, and the place across that Up and Down keep. cosmic-text hangs the space a line wraps at past the line's end, so End stops before that space.
- A change to the focused field, or the focus coming to a field, marks the window to show its caret. After placing, each scroll view around it, innermost first, moves the least that shows the caret, and the window is placed again; the new offsets are written to the views' signals after the tree is released, as `geometry` is. A field removed before that layout is skipped. `Window::set_modifiers` stores the seat's modifiers for a press to read.
- Each glyph's cosmic-text metadata is its span's index plus one, and a glyph belongs to its span only when its bytes are inside the span's, so the ellipsis, which cosmic-text shapes with the first span's attributes, takes the text's own style and is never a link. Fallback fonts are looked up once per span font, not per span.
- A link is hit by its glyph boxes and line box, through transforms, even where the text overflows its box. A Button around a link loses its hover style while the pointer is over the link, as it does over an inner button, but its `hovered` signal stays true.
- Clicks on text are counted press to press, each against the one before, by the double click's time and distance. The count, the node and the byte pressed are kept with the pointer and forgotten with the node. A triple click selects the line between line breaks, where `\r`, `\n`, `\r\n` and `\n\r` each end one, as cosmic-text breaks them. A drag after a double or triple click selects from the word or line pressed to the whole one under the pointer, on whichever side it is.
- An empty line whose line break is selected shows half an em of highlight, where its alignment and direction put the start of a line. cosmic-text puts every empty line at 0, so the caret on an empty line, and over a placeholder, is placed the same way.
- A field with an alignment lays its text out across its content box less one pixel, the caret's. A one-line text longer than that is laid out at its own width, so it starts at the start and scrolls as unaligned text does, and right-to-left text never starts left of the box. A wrapping text area with an alignment is measured at that width too.
- A field's caret, and an input method's underline, cover the default line height, `round(1.2 × size)`, centered in a taller line as cosmic-text centers the glyphs. The selection covers the whole line.
- Every text buffer has a tab width of four spaces.
- Everything that reads text lines goes through `TextNode::runs`: drawing, hit testing, selection, links and the caret. On a left-to-right line that holds the ellipsis, it drops leading blanks that don't start their buffer line, and moves the glyphs left by what the blanks took: all of it for a line placed from the left, half for a centered line, none for one placed from the right. A hit there asks cosmic-text where its own layout has that glyph, and never lands before the first glyph shown. Right-to-left lines are left as cosmic-text lays them out.
- The pointer's link and the focused link are kept as span indices on the text, and a span draws lit only while it is a link. When either changes, the old and new link's boxes are repainted, each grown by an em to each side and half an em above and below. They are carried out to the window through the same transforms and content shadows the place pass uses, so a shadow cast by an underline is repainted with it.

Rules settled while building `crabshell-ui` (gaps run):

- A screencopy view asks for its content box times `Affine::layer_zoom` of the transforms around it, so a view in a scale-up gets a frame as sharp as its layer.
- Reports from `image_source_failed` about an image are dropped with it.

Rules settled while building `crabshell-ui` (Phase 18):

- A left release or a key that leaves a field or selectable text with a new selection asks the platform to offer it as the primary selection, and a finger's selection is offered at the touch's up.
- A middle press on a field takes the focus and places the caret, with nothing selected, before the paste.
- A held drag carrying a value goes out once the pointer passes the window's edge; a leave then doesn't end it, and the platform ends it with `on_release`, the drop, then `End`. A drop that arrives after a read lands only on a button still shown, enabled and taking its type.

Rules settled while building `crabshell-ui` (Phase 19):

- A panel, floating window or lock surface is a frame and a popup a window; `Button` is a push button, `Text` a label, `TextInput` and `TextArea` entries or password text, `Slider` a slider, `Icon`, `Image`, `Canvas` and `ScreencopyView` images, and the rest fillers. Without `accessible_name`, a button is named by its text, a field by its placeholder, a panel by its namespace, a floating window by its title and a tray button by its item's title.
- A hidden widget stays among its parent's children with showing off, and a hidden floating window is unknown. While a lock surface is up only lock surfaces are listed, and other windows' widgets can't be clicked.
- Extents are window logical pixels, all Wayland tells a client. A screen reader hears focus, showing, enabled, children and name changes, only for widgets that were laid out and only while one listens. `DoAction` clicks a shown, enabled button as Enter does.
- A field's text offsets count characters, and a password reads as one dot a grapheme in every answer and event. A line is one between line breaks, not a wrapped one, and a word runs to the start of the next. A screen reader's write to a slider is clamped and stepped as a drag's, and refused while the slider is off or hidden.

Rules settled while building `crabshell-ui` (0.9 gap fixes):

- A mask region is a paint property of every widget: a change marks the node's bounds for repaint, an equal one nothing. `NodeView::mask_region` hands it to the renderer, and `Region::shape` and `Region::joined`, with `RegionShape` and `RegionOp`, let a renderer walk it.
- A gradient's center and radius fade as its colors do.
- Captures sit in a per-thread list, like timers, which the platform reads each iteration. Each frame is asked for under a number that is never reused, so a frame asked for before a refresh or a new source is turned away. Views of a capture bind its frame and capture nothing themselves, and a view with a crop asks for the source's whole frame.
- A view's `shown` follows its picture. Each commit marks every view placed in sight with a picture as drawn, and the frame callback marks it shown and writes `true`. A new picture, a view hidden or out of sight at a commit, one with nothing to draw, a removed view, a dropped window or a lost surface writes `false`.
- Ink is read from unhinted outlines, so it is the same at every scale. A curve's box comes from its ends and where it turns, never its control points. A COLR glyph's ink comes from its color layers, and a glyph with no outline, such as a color bitmap, gives the box of its pixels. Each glyph's ink box is cached, at most 4096 glyphs, after which the cache starts again.
- `max_length` counts chars, a line break as one, and never cuts inside a grapheme. Only a cut is written back to the signal; line breaks turned into spaces still aren't.
- A read-only field refuses edits in `Edit::key`, `Window::paste`, `commit_text`, `delete_around` and `set_preedit`, and turning it on drops the preedit. Ctrl+X in a field that can't copy, such as a password, does nothing, as Ctrl+C does. A screen reader sees it as not editable on its next read; no state-change event is sent.
- A field's selected text color is chosen per glyph, by where the glyph starts, over span and link colors. Underlines, strikethroughs and the caret keep the text's color. Only a focused field shows its selection, so only it recolors glyphs.
- Each lock surface wraps `on_key` in its own handler, so a panic disables it on that surface only, logged as `SessionLock[DP-1].on_key`.
- A slider's `limit` caps every write from a drag, the wheel, the keys or a screen reader. `None`, NaN or a limit past the end is the end, and one under the start is the start. A value set from outside past the limit shows where it is, and the next move from there lands at or under it.
- The wheel and the arrow keys move a slider by `wheel_step`, or by its step without one, or a twentieth of the range without either. With a step, the wheel step is rounded to a whole number of steps, at least one, half away from zero. A drag still snaps to the step, and a screen reader's increment is the wheel step.
- A button's `pressed` signal turns `true` at a left press or touch on it and `false` at the release. It also turns `false` when the pointer leaves the window, a scroll view takes the drag, the button is turned off, hidden or removed, or its window is dropped. The button's own drag keeps it `true`, a press on a button inside it holds that one instead, and it is written only on a change.
- An image's `status` goes to `Empty` for an empty path and to `Loading` for each new source, from the source binding. The renderer says `Ready` or `Failed` once it loads the picture, whether the image is drawn or not: `image_sources_changed` tells the platform a status waits, and `WindowView::loading_images` lists the images to load, hidden ones too, at the box they were last laid out in. A report about another source is dropped, and a picture decoded again at another size keeps `Ready`.
- A sequence's steps are read untracked as each play starts. A new trigger mid-play starts over from where the value is, and the replaced play stops; an empty list plays nothing, and a play rests at its last step's target. A loop's round 0 moves from where the value is, each later round from the last target. Stopped, a loop finishes its round and rests there; started again before then, it never stops. A round of 0 ms plays once.
- A hold, and a delay inside a sequence, takes the step off the animation clock. It sleeps until the next value change, merged through back-to-back holds and the next step's delay, and `next_timer` and `fire_timers` wake it. Its timeline counts from its first tick, so a late wake lands where the timeline says. A sleeping step still counts in its item. One whose owner went, or whose play was replaced, is freed at the next `next_timer` or `fire_timers` without waking.
- A panic in a trigger stops that sequence for good, one in `running` ends the loop after its round, and one in the steps binding or a config's `lerp` stops that play. Each is logged as `animation cx.sequence at <file:line>` or `animation cx.sequence_loop at <file:line>`.
- Every node is `position: relative` to taffy, so a parent is always its absolute children's containing block; taffy's default, static, placed them from the window. An absolute child of a `Stack`, a `Grid` or the window gets no cell placement, so its insets, and its alignment along an axis with no inset, measure from the parent's padding box, not the cell's content box.
- taffy's own rounding is off: in 0.15.1 it reads the list of absolute children a container recorded at its last layout with children, so a container left empty or hidden made it read removed nodes and panic. `place` rounds each node from its parent's unrounded corner as taffy did, which holds only because a parent is always its children's containing block.
- `cx.sequence_now` reads its steps once, untracked, as it is made, and plays at once. It is logged as `animation cx.sequence_now at <file:line>`.
- `on_any_press` runs at the press that starts a hold, after `on_press` for the left button, and never for a second button while one is held or for a left press or touch on a scroll bar.
- A play's leading steps of 0 ms and no delay land in the flush that starts it, so that turn's layout and draw already show them. A play of no time ends in that flush and never starts the clock. A 0 ms transition with no delay on `cx.animate` lands the same way.
- Every node has a `revision` (`NodeView::revision`) that changes whenever anything it or its subtree draws changes, including text reshaped for an inherited style or added font, while its own opacity and transform leave it unchanged.

## Rendering

v0.1 draws on the CPU with `tiny-skia`, straight into shared-memory buffers, and repaints only what changed. For a bar that uses less memory than a GPU context. A 1920×30 bar buffer is 225 KiB.

### Buffers

- Each window gets up to three ARGB8888 `wl_shm` buffers, each in a shared-memory pool of its own, since sctk's `SlotPool` never shrinks and hides its file. A buffer is reused once the compositor releases it.
- While a window has nothing to draw and nothing animates, once the compositor has released the buffer drawn last, the memory of every other released buffer is punched out of its pool. The buffer is kept, so the compositor hears nothing and the loop isn't woken, and one drawn again is repainted whole. A compositor that reads the buffer shown until the next commit keeps the spare, so its frames still repaint only what changed.
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
- Don't scan every system font at startup. On a machine with thousands of font files, that alone could miss the 100 ms startup target. Resolve the families the config names through fontconfig and load only those files into `cosmic-text`'s font database. Missing glyphs use fontconfig's fallback list. Fonts a config bundles with `cx.add_font` are read from its bytes, and a family they provide at the weight and slant asked for skips fontconfig. In Phase 0, fontconfig's startup took about 15 of the 17.7 ms to first frame, and its cache files held about 4 MiB of memory.
- Glyphs are drawn as grayscale alpha masks. There is no subpixel antialiasing in v0.1.

### Icons and images

- Icon names resolve through the freedesktop icon theme spec with an in-house lookup. The config picks a theme with `cx.set_icon_theme("...")`. A config that never calls it gets the theme GTK apps use, `gtk-icon-theme-name` from `$XDG_CONFIG_HOME/gtk-3.0/settings.ini`, else from `gtk-4.0/settings.ini`, else `hicolor`.
- `resvg` renders SVG icons. It already draws with tiny-skia, so it renders into the same pixmap. PNGs load with the `png` crate.
- Rasterized icons are cached by name, size and scale.
- `Image` files load by extension: PNG through `png`, JPEG through `zune-jpeg`, a GIF's frames through `gif`, WebP through `image-webp`, and SVG through `resvg`. They are halved, then scaled, to the size drawn, and kept in the icon cache.

### Shapes

Solid fills, linear, radial and sweep gradients, boxes with a radius per corner, solid, dashed and dotted borders, box shadows outside or inset and shadows cast by the content, masks, and a canvas's paths. Blur behind a window is the compositor's, through `ext-background-effect-v1`. v0.1 had no gradients, shadows or blur; Phase 8 added them.

### Later GPU backend

Keep every drawing call inside `crabshell-render`, behind plain functions. A GPU backend using `vello` on `wgpu` replaces that crate's internals later. Don't add a renderer trait until a second backend exists.

Phase 8 checked whether it was needed yet. While the effects bar's indicator slides and a tab's hover fades in, a frame takes 0.31 ms to draw at scale 1 and 0.54 ms at scale 2, release build, on a 1920 pixel wide bar. That is under 4% of a 60 Hz frame, so the CPU renderer stays.

Rules settled while building `crabshell-render` (Phase 2, step 2):

- Buffers use `ARGB8888`. Damaged areas are drawn in 64-pixel tiles of a fixed grid, each into a reused RGBA scratch pixmap, then copied into the buffer with red and blue swapped. This replaces drawing straight into the buffer: it keeps glyphs, resvg and png in tiny-skia's order, at the cost of one copy of the changed pixels.
- A pixel is always drawn in the same tile, whatever the damage. tiny-skia cuts a curve where it leaves the pixmap, which changes the antialiasing beside the cut, so drawing each damaged area as one pixmap made a repaint differ from a full draw.
- Damage: logical rectangles round outward to pixels, plus a 1 px margin. Overlapping rectangles merge. Each window keeps 4 frames of history. A buffer older than that, a new buffer, a buffer last drawn by another window, or a new size or scale is repainted whole. The reported damage is this frame's change. More than half dirty reports the whole buffer.
- `draw` returns `Result<_, CanvasError>`. The caller commits after every successful draw, in order, or the reported damage is wrong.
- Shapes snap to whole pixels. Corners are fitted as in CSS: when two on a side add up to more than its length, all four shrink by one factor. A border is drawn inside the edge, its width rounded down with a 1 px minimum.
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

Rules settled while building `crabshell-render` (Phase 10):

- A captured frame is turned upright from the transform the compositor reports, and kept scaled down, its shape kept, to the least size that covers its view's box at the largest output scale; never up. Buffers in ABGR, ARGB, XBGR or XRGB 8888 are read, the X formats as opaque. The renderer keeps one scaled copy per view, dropped as soon as the platform ends the view, or 16 draws after its last use.

- A cut-out shadow is masked by the box's own shape, filled as its background is, so its edge meets the background's antialiasing; a sharp one is clipped by the inverted mask, a blurred one multiplied by it, in each tile.
- `quantize_colors(path, depth, size)` decodes with `Image`'s loaders and caps, halves the picture until its longer side is at most `size` (0 keeps it), skips pixels under alpha 128, and splits at the median of the widest channel into at most 2^`depth` boxes, `depth` clamped to 8. Equal channel values stay on one side, so two boxes never give one color. The colors are each box's mean, sorted by pixel count, then by their bytes.

Rules settled while building `crabshell-render` (Phase 8):

- A gradient is laid out in buffer pixels and moved onto each tile, so tiles agree. A linear one takes CSS's angle, with its line long enough for the ends to meet the corners. A radial one is a circle from its center, as fractions of the box, to its radius times the scale, or to the corner farthest from the center when it has none; past it the end color holds, and a radius of 0 or less leaves only the end color, in a mask too. A sweep turns around its center. A clear stop fades each side out in its neighbor's color, as CSS blends with the alpha applied. A style's or a canvas's gradient is worked out per pixel in buffer pixels, blended before its alpha as tiny-skia blends it. It is given a 4×4 ordered dither of under one 8-bit step, keyed on the pixel's place in the buffer, in its alpha as in its colors, so tiles agree, a fade to clear doesn't band and a flat gradient stays flat.
- A style's gradient is worked out over its box and blended through a mask of its shape over the whole pixmap. A canvas's is worked out only over each path's bounds, grown by twice a stroke's width, and drawn through tiny-skia's pattern shader. Masks stay undithered.
- A shadow is drawn under its box, so a translucent background shows it. A sharp one is a filled shape. A blurred one is a Gaussian of half the blur, worked out for each pixel in buffer pixels: the product of the error function on each axis away from the corners, and eight Gaussian-weighted rows near one. A corner smaller than the spread grows by less, as in CSS, so a square box keeps square corners.
- An inset shadow is the inside of the border minus a hole of the padding box's shape, moved and shrunk by the spread with its corners shrunk too, sharp or blurred by the same per-pixel Gaussian as an outer one. The eight sampled rows near a corner are divided by their sum over the full reach, so a point the shape covers whole is covered whole. That changed outer shadows near rounded corners by up to one level.

Rules settled while building `crabshell-render` (Phase 14):

- A transformed node draws its subtree into a layer holding what lands in the tile, plus 2 pixels, in its own buffer pixels, then blends it in through the transform with bilinear filtering. Opacity shares that layer, and a clip is cut inside it, so the clip follows the turned shape. As with tiles, a curve cut at a layer's edge antialiases a few levels differently near tile edges, and a layer past 4 Mi pixels isn't drawn.

Rules settled while building `crabshell-render` (Phase 15):

- Render draws a node's children in the order `NodeView::children` gives, which is z order, so `z` needed no change here.
- A scroll bar's handle is drawn with `draw_box` from its style, inside the view's clip mask after its children, so it is cut to the view's rounded corners. A 6 px handle at scales 1, 1.5 and 2 is pinned by `scroll_bar@*.png`.
- Each of a scroll view's handles, one an axis, is drawn the same way; `scroll_bars_both@*.png` pins two. `NodeView::scroll_bars` gives every handle a view shows, one an axis, and `scroll_bar` the first.

Rules settled while building `crabshell-render` (Phase 16):

- A canvas's drawing is scaled from its box's corner, snapped to the pixel grid, and drawn through a mask of its box with the style's corners, in each tile, so a repaint matches a full draw. Strokes go through tiny-skia's own stroker with a miter limit of 4, an arc becomes cubic curves of at most a quarter turn, and a gradient's shader undoes the path transform so it lies over the box in buffer pixels. `canvas@*.png` pins a ring, a gradient-filled graph, dashes and a canvas inside a turned, translucent clip.
- Each corner is fitted in pixels as CSS fits it, and a uniform radius draws as it did. A sweep is laid out in buffer pixels and turned about the middle by its transform, since tiny-skia 0.12 ignores a whole-turn sweep's start angle. In a blurred shadow, a row near a corner takes its reach from that side's corners, and the fast product of error functions holds past the largest corner, so a radius per corner costs about what one does.
- A dashed border is stroked along its middle line in buffer pixels with miter joins, butt caps, and round caps for dots. A box with no square corner is dashed around its whole middle line, a dash centered on the end of the top-left corner.
- Otherwise, as browsers do, each run from one square corner to the next of a dashed border gets its own whole number of dashes centered on both corners. Each square corner is filled by a short piece joined with a miter, all stroked as one path, so a translucent corner is drawn once. A corner whose middle-line radius is 0, under half the border's width, is dashed square. Each tile keeps the dashes whose box comes within twice the width plus a pixel of it. A number that isn't finite, a negative one, an `off` of 0 or a period under a pixel draws the border solid. `corners@*.png`, `sweep@*.png` and `dashes@*.png` pin the new drawing.
- Image filters run once on the picture scaled to its box in buffer pixels, and the cache keeps the result, keyed by the file, size, fit, tint and each number, with the blur in buffer pixels; each filter clamps before the next, on premultiplied pixels, and a tinted icon is cached apart from the plain one. The blur is three box blurs each way, which the Filter Effects spec allows, with pixels past the edge repeating the edge so a blurred picture keeps opaque edges, and its deviation clamped to the picture's longer side, so any larger number, infinity included, ends quickly. A blurred picture is made once per size, on the decode worker. `tinted_icons@*.png`, `image_filters@*.png` and `blurred_images@*.png` pin them.
- A GIF's frames are composed on its canvas as each frame's disposal says, then scaled and filtered as a still picture, all at its first draw. A frame not wholly on its canvas is refused, by the decoder's consistency check and again before it is composed; a bad first frame refuses the file, and a later one stops the animation on the last good one, logged. Decoding may come to 64 Mi canvas pixels, and the frames kept to 8 Mi pixels at the size drawn, within what the picture cache's 32 MiB has left after the pictures a recent draw used and any waiting animation's frames; past any of these, the first frame shows, logged once.
- The renderer keeps each GIF's place by its image's id. A drawn frame is due its delay after the last was due, or after the draw if that has passed; `next_image_frame` gives the earliest due time and `advance_images` moves the due GIFs on and returns their ids. A GIF waits again only once drawn, so one hidden, off its window, clipped out or in a window that can't draw wakes the loop at most once more, and a waiting GIF's frames stay cached.
- A mask, a content shadow, opacity and a transform share one layer: the mask is applied after the contents are drawn, any clip already cut inside it, the content shadow is worked out from what is left and put under it, and opacity and the transform then blend the result in. An ellipse mask is filled antialiased on the box's pixel edges, a gradient mask is laid out as a background is, and an image mask is read at the box's size with `Fit::Fill`, paused on a GIF's first frame. `masks@*.png` and `content_shadows@*.png` pin them.
- A content shadow is cast in cells of a grid laid from the corner of the area it can show in, each at least 256 pixels and four reaches a side, at most once a draw, when a tile first needs it, from the subtree drawn into a layer over the cell moved back by the offset and grown by the reach and 2 pixels. That layer starts at the same pixel in every draw, since content drawn into a layer that starts elsewhere antialiases a few levels differently. The offset and spread are whole pixels, the spread a square dilation or erosion, and the blur follows SVG's `feGaussianBlur` with integer box sums, so repaints match full draws.
- Images are drawn by copying opaque rows and blending the rest, and a file's modification time is looked at once a draw.
- A transition blends both pictures per pixel in the picture's own buffer pixels, premultiplied, then lays them over what is under them. A wipe's edge and a circle's rim are antialiased over a pixel, the circle growing from nothing to past the corners; pixelate grows blocks to a twentieth of the longer side halfway and shrinks them back, each block showing its middle pixel, with the pictures swapping through the middle half. A GIF being left shows its first frame. `image_transitions@*.png` pins each halfway.
- The file a transition leaves is resolved once per transition, so every tile draws the same. Its picture is held only while a draw uses it: it is dropped when the transition ends, is replaced, is switched to none or its image is dropped, and when its own window, named by its damage history, draws without drawing it. A picture another image showed within the last 16 draws, or another transition still leaves, is kept, so two outputs crossfading the same wallpaper hold it once. Both pictures are cached during a crossfade, and one after.

Rules settled while building `crabshell-render` (Phase 17):

- Underlines and strikethroughs are filled rectangles at the underline and strikeout offset and thickness of the font, as cosmic-text and Pango place them, with cosmic-text's defaults when the font gives none. The edges snap to whole buffer pixels, each line is at least a pixel thick, the offset is clamped to a quarter em below the baseline or half an em above it and the thickness to a quarter em, so a line stays inside the text's painted area. Underlines and strikethroughs are split where the font changes. Each run of glyphs from one font takes that font's offset and thickness. They don't skip descenders, as Qt's don't. `fallback_decorations@*.png` and `lit_links@*.png` pin this.
- A span's color and lines are drawn per run of its glyphs on a line, in the span's color or the text's. cosmic-text 0.19's decoration spans join two decorated spans across the plain text between them, so they aren't used.
- A selectable text's selection is drawn behind it in a third of its color, as a field's is.
- `text_styles@*.png`, `rich_text@*.png`, `selectable_text@*.png` and `text_area@*.png` pin the new drawing. The goldens also need Noto Sans Italic and Bold Italic for fontconfig's italic `sans-serif`.

Rules settled while building `crabshell-render` (gaps run):

- Once the platform calls `decode_off_thread`, image files past 64 KiB, drawn past 256 by 256 pixels, or blurred are decoded on one worker thread, with the same caps. The thread starts with the first job and ends when none is left. Until a picture comes in, its box draws the picture its transition leaves, or else the one it drew before, stretched to the box if its size changed, or else nothing. Once it comes in, the platform repaints that image in every window. An image that starts waiting on another size or filter of the same file leaves its earlier jobs, and a job no image waits for is cancelled if the worker hasn't started it. A GIF decoded off the thread keeps its frames only if the cache still has room for them when they come in; otherwise it shows its first frame, which is logged. A mask decodes its picture at once, taking over a job the worker hasn't started.
- `waits_for_pictures` says whether the last draw drew an image whose picture is still being decoded. While a transition's new picture is decoded, render reports it through `image_source_decoding`, and the transition's clock waits at its start, the old picture shown, until `repaint_image` says the picture came in.
- A GIF stops waiting for its next frame when a draw of its own window repaints every tile it was last drawn in without drawing it, so a GIF hidden while it waits wakes nothing. One drawn in another window, or not repainted, goes on.
- A content shadow blurred by 8 buffer pixels or more is worked out on blocks of 2 pixels a side, and from 16 pixels on blocks of 4. The blocks are averaged on a grid laid from the painter's origin, blurred by the deviation left after the blocks' own, and read back bilinearly where the shadow is drawn.
- A transformed layer is drawn at the output's scale times `Affine::layer_zoom` of every transform around it. That is the transform's largest stretch, rounded up to a quarter and kept from 1 to 4, so text scaled up stays as sharp as text of that size. A scale-down draws as before.
- A picture a mask used within the last 16 draws is kept when a transition stops leaving it.
- When the new source of a running transition can't load, render reports it through `crabshell_ui::image_source_failed`, and the transition ends at its next tick.
- An opaque background with square corners and edges on whole pixels is filled straight into the tile's pixels, which matches tiny-skia's path fill exactly.
- An image looks its pictures up once a draw: later tiles of the same view, at the same size and scale, reuse what the first found while that picture is still cached. A GIF and a source that can't be loaded look up every tile.
- A row's opacity check and the red and blue swap into an ARGB buffer work on whole 32-bit words, which vectorize: a 1080p frame's check went from about 1.5 ms to 0.2 ms, and the swap from 1.1 ms to 0.4 ms.
- `inset_shadows@*.png`, `empty_line_selection@*.png`, `styled_field@*.png`, `fallback_decorations@*.png`, `lit_links@*.png` and `scroll_bars_both@*.png` pin the new drawing.

Rules settled while building `crabshell-render` (Phase 18):

- An icon with a fallback asks the cache again when its source fails, and each failure is logged once. `IconLookup` shares the renderer's theme cache with `cx.icon_path`.

Rules settled while building `crabshell-render` (0.9 gap fixes):

- A mask region shares the layer of a style's mask and is applied after it. Each shape is filled antialiased in the layer's pixels on the fixed grid, its corners fitted as a box's are, and joined by coverage fractions: union a + b − ab, subtract a(1 − b), intersect ab, xor a + b − 2ab. The result is cut to the box. A tile the region and box cover wholly is drawn with no layer, and one they miss is skipped, unless a content shadow may fall there. A full-screen 1080p picture with a few rounded holes takes about 6 to 8 ms a frame at scale 1 and 22 to 25 ms at scale 2, most of it in the tiles a hole's edge crosses. `mask_regions@*.png` pins it at all five scales.
- `placed_gradients@*.png` pins an off-center five-color sweep under a radial glow with a radius, a canvas fill and a mask.
- A crop maps to frame pixels through the frame's logical size, rounded to whole pixels and cut to the frame. A crop wholly outside the frame, or not finite, draws nothing. The kept copy is the cropped part scaled to the box, and it is let go once the view is dropped.
- A field's selection is drawn in `InputView::selection_color`, and the glyphs `TextView::selected_color` names in the selected text color; a selectable `Text` keeps a third of its color.
- The renderer tells an image's status `Ready` when its source's picture loads and `Failed` when the file can't be read or decoded or is refused by a cap, drawn or not, and nothing while the picture is decoded off the thread, until it comes in. `Renderer::load_images` loads a window's images whose status waits, at their laid-out size or one pixel; a job for an earlier source doesn't hold it back, and that job's report is dropped.
- An `Image` of raw pixels is scaled on the loop's thread, once per size, fit and filters, and cached by the pixels' shared allocation; the raw bytes count toward the cache's 32 MiB, and an entry whose pixels nothing else holds is dropped at the end of the frame. A source with the same bytes keeps the picture shown. An icon name is looked up in the theme at the box's larger side and the scale rounded up, then decoded as that file, so it can use the worker. An empty path or name is `Empty` and is never logged.
- A subtree a draw skips at opacity 0, or cut out by its mask region, is looked through once a draw, within its clips and the window, and only when the window holds an image; each Image a reveal would draw is loaded at the size drawing it would give, scale and transform zoom included, through the same cache and worker path as a drawn one. It never plays a GIF or holds a first frame.
- A layer, a moved layer and a picture are blended in by an integer premultiplied source-over loop at an opacity and through an optional mask. It matches tiny-skia's `draw_pixmap` within a level, and it vectorizes. A transform that tiny-skia sees as an identity or pure translation is blitted at `(tx - 0.5).ceil()`, as its nearest sampling places it. A transform that only scales and moves uses a separable bilinear blend, mixed across in floats, then mixed down, faded and rounded to bytes and blended with the word blend. It rounds twice, so it is within 1/255 of the float blend it replaced and within 2 levels of tiny-skia's bilinear pipeline; any other transform goes through `draw_pixmap` bilinearly.
- A tile that a rounded box covers whole, with no corner curve reaching into it, has its children drawn straight in with no clip layer or mask. Its background is filled without a path. The border ring is skipped where its hole covers the tile.
- A cut-out shadow is skipped in a tile its box covers whole. In an edge tile, only the runs of columns the box doesn't cover in every row are worked out. `large_boxes@*.png` and `large_cut_out_shadows@*.png` pin both at all five scales.
- A picture already the box's size is copied, not resampled.
- With a decode size smaller than the box in logical pixels (multiplied by the output scale), the picture is made at that size, keeping the box's shape. The filters run there with the blur scaled to match, and the result is stretched up to the box bilinearly, once. A side of 0 doesn't limit that side. The cache keys the picture by the decode size and keeps it at the box's size.
- A layer drawn for opacity or a transform is kept between draws while the node's `revision` stays and its own opacity (or, with a transform, its transform) changes, and the renderer's icon theme has not changed. It is dropped when a draw of its window doesn't draw it through a layer, and never holds a playing GIF or a transitioning image. While kept, its images and scaled captures stay marked used so they aren't evicted. Kept layers and shadow patches share 32 MiB per renderer.
- A kept transformed layer is drawn whole, so it shows none of the seams a layer cut to each tile shows (up to 14/255 at a chopped rounded corner). For that reason it is kept only while its transform moves. A node drawn without a layer whose style sets a translate, scale or rotate that leaves it where it is counts as seen with no transform, so a move from rest is kept from its first moving frame.
- A blurred outer shadow whose box is at least twice its largest corner plus its blur's reach, plus two, on each side, is drawn from a patch of that size, within 1/255.

Rules settled while building `crabshell-render` (0.9.5, the workspace bloom):

- Where a shape's edges lie on whole pixels and no corner's curve reaches the pixmap, tiny-skia fills it as 255 inside and 0 outside, so a clip blits its children's layer within that rectangle, a cut-out shadow skips the rectangle without a mask, and a mask region's rectangle is written into its mask without a path. In a pixmap a corner reaches, the rows between the corners' rows are handled the same way and only the corners' rows go through the mask. A cut-out shadow checks against its box's own corners, not the shadow's spread ones.
- A cut-out shadow skips, row by row, the columns its box covers whole, so a layer holding a whole box works out only the shadow around it.
- `composite`, a glyph or shadow mask and a scaled layer blend over what is there on whole 32-bit words, two channels a multiply, to the bit of the per-channel blend. A layer's clear rows are skipped.
- Large layers and masks, 1 MiB or more, that a draw lets go of are kept for its next large one, up to 16 MiB a draw, each charged to the 32 MiB that kept layers and shadow patches share and let go before a kept layer or patch would be refused it. A reused one is cut to the size asked for, and every spare is dropped as the draw ends, so an idle window holds none. Fresh memory page-faults every 4 KiB as it is first written, since the shell fixes glibc's mmap threshold at 128 KiB: about 3,750 faults for one 1080p piece of the rime bloom.
- A crop already at its box's size is kept as cut, not copied a second time.

## Wayland platform

`crabshell-wayland` uses `smithay-client-toolkit` (sctk) on a single-threaded `calloop` event loop. Everything that touches widgets or signals runs on that one thread.

| Protocol                                                                                                                                                                  | Used for                                                                                             |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `wl_compositor`, `wl_shm`                                                                                                                                                 | Surfaces and CPU buffers                                                                             |
| `wl_output` version 4                                                                                                                                                     | Output names such as `DP-1`, plus description, make, model, mode, scale, physical size and transform |
| `zwlr_layer_shell_v1`                                                                                                                                                     | `PanelWindow`                                                                                        |
| `xdg_wm_base`, through `zwlr_layer_surface_v1.get_popup`                                                                                                                  | `PopupWindow`                                                                                        |
| `xdg_toplevel`, `zxdg_decoration_manager_v1`                                                                                                                              | `FloatingWindow`, with server-side decorations                                                       |
| `wp_fractional_scale_v1`, `wp_viewporter`                                                                                                                                 | Fractional scaling                                                                                   |
| `wp_cursor_shape_v1`                                                                                                                                                      | Cursor shapes                                                                                        |
| `wl_seat`, pointer and keyboard                                                                                                                                           | Clicks, hover, scroll and keys                                                                       |
| `zwp_text_input_v3`                                                                                                                                                       | Input methods in text fields                                                                         |
| `wl_data_device_manager`                                                                                                                                                  | The clipboard                                                                                        |
| `ext_idle_notifier_v1`, `zwp_idle_inhibit_manager_v1`                                                                                                                     | Idle watches and inhibitors (Phase 9)                                                                |
| `zwlr_foreign_toplevel_manager_v1`                                                                                                                                        | Every program's windows (Phase 10)                                                                   |
| `ext_image_copy_capture_manager_v1`, `ext_output_image_capture_source_manager_v1`, `ext_foreign_toplevel_list_v1`, `ext_foreign_toplevel_image_capture_source_manager_v1` | `ScreencopyView` (Phase 10)                                                                          |
| `ext_session_lock_manager_v1`                                                                                                                                             | `cx.session_lock` (Phase 11)                                                                         |
| `ext_workspace_manager_v1`                                                                                                                                                | `cx.window_manager` (Phase 12)                                                                       |
| `zwp_keyboard_shortcuts_inhibit_manager_v1`                                                                                                                               | `inhibit_shortcuts` (Phase 12)                                                                       |
| `hyprland_global_shortcuts_manager_v1`                                                                                                                                    | `cx.global_shortcut` on Hyprland (Phase 12)                                                          |
| `hyprland_focus_grab_manager_v1`                                                                                                                                          | `focus_grab` on Hyprland (Phase 12)                                                                  |
| `hyprland_surface_manager_v1`                                                                                                                                             | `compositor_opacity` and `visible_mask` on Hyprland (Phase 13)                                       |
| `zxdg_output_manager_v1`, bound by sctk                                                                                                                                   | Each output's logical place and size (Phase 13)                                                      |
| `xdg_activation_v1`                                                                                                                                                       | Activation tokens for launched programs (gaps run)                                                   |

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

Integration tests run Sway headless with `WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1`, add outputs with `swaymsg create_output` and take screenshots with `grim`. They are `#[ignore]` and run one at a time. [AGENTS.md](../AGENTS.md) has the commands and the rules that keep them off the desktop session.

- Each test runs its own Sway from a copy of the `sway` on PATH, in a short private `XDG_RUNTIME_DIR` with `swaybg_command -`, since socket paths are capped at 108 bytes. SwayFX carries the `cap_sys_nice` file capability and switches itself to realtime, which a parent with a realtime CPU limit of 0, as T3 Code has, answers with SIGKILL. The copy has no file capabilities. A failing test prints Sway's exit status and log.
- The private Sway, `grim` and the shells under test start with a cleared environment, and every `swaymsg` names the private `SWAYSOCK`. Every shell a test starts gets that `SWAYSOCK` and a `PULSE_SERVER` that is private or doesn't exist, and no in-process test shell calls `cx.sway()` or `cx.audio()`. Headless shells get an `XDG_CONFIG_HOME` holding only a link to the user's fontconfig, so the user's GTK theme stays out of the goldens.
- SwayFX renders on the GPU whatever `WLR_RENDERER` says.
- A headless seat has no pointer device. A virtual pointer from a second client gives it one, and clicks then work both through `swaymsg seat - cursor` and through the virtual pointer.
- Sway deletes an empty workspace once it isn't shown, so a test that needs two workspaces uses two outputs.
- Goldens are PNGs compared per pixel with a tolerance of 2 out of 255, and `CRABSHELL_BLESS` rewrites them. They need Noto Sans as fontconfig's `sans-serif`, Noto Sans Italic and Bold Italic for its italic `sans-serif`, and DejaVu Sans and DejaVu Sans Bold; the `crabshell` suite checks the first.
- With several windows the logged first frame may be another window's, so a golden waits for a pixel of the window it checks, then for two equal screenshots.
- The control example's tests pin its date with `CONTROL_TODAY` and run on a 3360×2100 output, whole in logical pixels at every quarter scale. The dashboard's golden takes its icons from `tests/fixtures` through `XDG_DATA_DIRS` and a theme name no installed theme takes. A check that memory was freed runs with the `GLIBC_TUNABLES` the supervisor gives every shell, since without it glibc keeps the RSS high-water mark.
- One wayland test runs on a headless labwc instead of Sway, to check popups and the pointer grab on another wlroots compositor, so the suite needs `labwc` installed too. The helper starts it like Sway: in a private runtime directory, with an empty config and a cleared environment.
- Services are tested against fakes: workspaces against a fake Sway socket, the tray with a fake item on a private `dbus-daemon` that starts no services, the other D-Bus services against fake daemons on private buses, and audio against a fake server and, in `#[ignore]` tests, a private pipewire-pulse.

`crates/crabshell-wayland/tests/fake` is a fake compositor built on `wayland-server`. It runs in a thread of the test over a socket pair, offers `wl_compositor` 4, `wl_shm`, `xdg_wm_base`, layer-shell and one output `FAKE-1`, plus a touch seat, the background effect manager, workspaces or a second output announced late when a test asks, and logs the requests a test checks. The tests in `tests/protocols.rs` run by default and need no Sway, GPU or session; `crabshell`'s startup tests use it too.

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
- Buttons other than left, right and middle were ignored until Phase 15, and so was horizontal scroll. A whole scroll notch counts as 120. Since Phase 15 one scroll frame becomes one `Wheel`, and its `stop` drops a partial scroll step.
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
- Blur binds `ext_background_effect_manager_v1` version 1. A surface gets its effect object the first time it asks, and the region is cut to the surface's size and sent before the commit that applies it, and again when the size changes, as the mask is. Without the protocol, or with the compositor's blur off, a window that asks is logged once per shell and shows unblurred. Blur that turns on later shows from each window's next frame. The region goes out in surface pixels, only once while unchanged even when other settings are sent, and as none when the style drops it. No compositor here has the protocol, so the fake compositor tests it.
- `Stats::drawing` adds up the time spent drawing frames.

Rules settled while building `crabshell-wayland` (Phase 9):

- Idle binds `ext_idle_notifier_v1` and `zwp_idle_inhibit_manager_v1`, both version 1. A watch is a notification on the first seat, so a watch made before any seat exists never fires, and one whose seat goes stays as it was. A panicking callback is logged and the shell goes on.
- A window's idle inhibitor is made once its surface is configured, while its `inhibit_idle` is true, and goes with the surface. Headless Sway honors one on a layer surface.

Rules settled while building `crabshell-wayland` (Phase 10):

- Toplevels bind `zwlr_foreign_toplevel_manager_v1`, versions 1 to 3, at the first `watch_toplevels`, so a shell that never asks hears nothing. Without it, that is logged once and the list stays empty.
- A handle counts at its first `done`, and changes at each later one. The watch hears the whole list once per dispatch that changed something. `closed` drops and destroys a handle and clears it as any other's parent, as the protocol doesn't promise to say; `finished` empties the list for good. Outputs are named from the outputs the platform knows, one not announced yet left out until it is.
- Titles and app ids are cut to 4 KiB at a character; past 1024 toplevels the rest are destroyed and logged once. Activate uses the first seat; fullscreen names no output and needs version 2.
- Headless Sway honors activate, fullscreen and close, ignores maximize and minimize, and never says a window's parent; labwc does all of them, so its tests cover those.
- Screencopy binds its protocols when a shown window first holds a view or a capture first waits for a frame, and the ext toplevel list when a view of a window first does. The views of one source and cursor mode in the shown windows share one session, waiting while the source isn't there, with a buffer of the size and format announced; a format `wl_shm` doesn't take too is never used.
- A screencopy frame is cut once, to the least size covering the largest of its views' boxes at the largest output scale, and the same pixels go to every view. A view that joins a session with a frame shows that frame at once if it covers its box, and otherwise the session starts again. A session ends when no view takes it. Sources past 8192 pixels a side or 24 million pixels are logged and show nothing.
- A session with no live view ends after its frame. While one is live, the next frame is asked for once a window that showed the last one drew it, so frames come no faster than they show and nothing comes while those windows can't draw. A frame the same as before asks again at once, which the compositor answers only when the source changes. A live view that joins a session waiting for its windows to draw asks for the next frame at once, since those windows may be hidden and never draw; the compositor answers once the source changes. A session stops waiting for a window with no view of it. A view whose box outgrows its frame, or a higher scale, starts the session again.
- `buffer_constraints` starts a new session; any other failure is tried again once. A stopped source ends the session and keeps the last frame; an output announced again gets a new session.
- Window views pair an `ext_foreign_toplevel_handle_v1` with the wlr toplevel of the same app id and title, oldest unpaired first, freeing a pair whose wlr toplevel went. Two windows with the same app id and title may swap; the two protocols share no identifier, so that is the protocols' limit.

Rules settled while building `crabshell-wayland` (Phase 11):

- The lock binds `ext_session_lock_manager_v1` version 1 at startup. Without it, the first lock logs that once and nothing locks.
- A lock surface is an ordinary window with the path `SessionLock[DP-1]`, made for every output as the session locks and for each output added while locked. Its configure is acked at once, and nothing is committed before it. A widget on a lock surface can't open popups; that is logged once per popup.
- The platform checks `locked` after each flush, and flushes again if it locked, unlocked or built surfaces. If `locked` turns false before the compositor answers, the answer decides: `unlock_and_destroy` after `locked`, `destroy` after `finished`. `finished` drops the surfaces and is logged, and the lock asks again only once `locked` turns false and true.
- After `unlock_and_destroy` the signal turns false only when a `wl_display.sync` comes back, so a config that exits on it never leaves the unlock unsent.
- A lock whose context goes while it holds the session keeps its protocol object, so the session stays locked, and after the shell exits too. A surface builder that panics leaves that output showing only the lock's style, and a `locked` binding that panics is disabled, never read as an unlock.
- On headless Sway, `locked` arrives 3 to 135 ms after the request, a second client is refused with `finished`, and a locker that dies leaves red on every output until another client locks.

Rules settled while building `crabshell-wayland` (Phase 12):

- Workspaces bind `ext_workspace_manager_v1` version 1 at the first `watch_windowsets`, so a shell that never asks hears nothing; without it, that is logged once and the lists stay empty. Changes count at the manager's `done`, and the watch hears both lists once per dispatch that changed one. `removed` destroys a handle at once, and `finished` empties the lists for good. Names and stable ids are cut to 4 KiB at a character; past 1024 windowsets, 256 projections or 16 coordinates the rest is destroyed or left out, each kind logged once. Requests in one loop turn go out with one `commit`; a `create_windowset` name over 4000 bytes or holding a NUL is refused and logged, since Sway ends a connection past 4096 bytes.
- Headless Sway gives each output its own projection, takes activate, assign and create but not deactivate or remove, and sends no ids or coordinates. Headless labwc has one group and activate only.
- A request goes out only when the windowset's or projection's capability says the compositor takes it, since the protocol has the compositor ignore the rest; one left out is logged at debug with its id. A test against the fake compositor checks that deactivate and remove go out with one commit, and only where offered.
- Shortcut inhibitors bind `zwp_keyboard_shortcuts_inhibit_manager_v1` version 1. A window gets one for the first seat once configured while `inhibit_shortcuts` is true, and loses it with its surface. `shortcuts_inhibited` follows `active` and `inactive`, and turns false when the inhibitor goes after an `active`. Without the protocol, the first window that asks logs it once.
- The Hyprland protocols' XML sits in `crates/crabshell-wayland/protocols/hyprland` (hyprland-protocols v0.8.0, BSD-3-Clause), and `wayland-scanner` generates their bindings in `hyprland.rs`. Like the code `wayland-protocols` generates, they hold `unsafe` that `forbid(unsafe_code)` doesn't see, which the maintainer accepted; the only published bindings crate is LGPL-3.0.
- Global shortcuts bind `hyprland_global_shortcuts_manager_v1` version 1 at startup. Watches of one app id and name share a registration, destroyed with the last watch, since Hyprland ends the connection on a duplicate. A release in the same loop turn as its press is told after that turn's flush, so the press shows. Strings with a NUL, or over 4000 bytes together, are refused and logged.
- Focus grabs bind `hyprland_focus_grab_manager_v1` version 1. After the windows draw, each `true` signal's grab holds the mapped surfaces of its windows and their popups, sent as changes and committed. A grab ends when its signal turns false or goes, or `cleared` comes, which writes `false`; one whose surfaces all went ends and writes `false` too. Without the protocol, the first window that asks logs it once and the signal is left alone.
- No compositor here offers the Hyprland protocols, so their tests cover the missing-protocol path and their logic against fakes. Global shortcuts and the focus grab passed a hand check on Hyprland 0.57.2 on 2026-11-15.

Rules settled while building `crabshell-wayland` (Phase 13):

- Each output's description, make, model, physical size and transform come from `wl_output`, and its logical place and size from xdg-output, which sctk binds; without xdg-output the logical size is the turned mode over the whole scale. Headless wlroots outputs report make and model as `Unknown` and 0 mm, and Sway's `transform 90` reaches clients as 270.
- `fractional_scale` is the 120th that gives back both logical sides from the turned mode, cut or rounded, nearest the mode's width over the logical width; that estimate when none fits; and the whole scale outside 1/120 to 8, or without `wp_fractional_scale_v1` and `wp_viewporter`. Where two 120ths give the same logical size, as some small modes past scale 2 do, either may be read.
- `Event::OutputChanged` is sent only after a `done` that changed something, as Sway sends `done`s that change nothing.
- A popup configure of another size than asked limits its layout. Each `reposition` carries a new token, and a configure answering an older one leaves the limit alone. Without `xdg_wm_base` version 3 there is no reposition, so a resized popup keeps its first limit.
- A mask is cut to the size after the layout. A floating window's new mask goes before its settings' commit, a popup's asks for a frame. Headless Sway passes a click outside a toplevel's or popup's input region to the surface below it.
- `hyprland_surface_manager_v1` (hyprland-protocols v0.8.0, beside the other two) is bound at versions 1 to 2 the first time a window asks for anything but the defaults, and a surface gets its object then; without it, that is logged once, and version 1 has no visible regions, also logged once. The visible region is in buffer pixels, as the protocol says: grown by a logical pixel each way so curves' soft edges stay inside, cut to whole buffer pixels and to the buffer, and sent again on a new size or scale. An empty region is sent as none, which Hyprland reads as no hint. Hand-checked on Hyprland 0.57.2 at version 2.
- A panel reconfigures layer-shell only when a layer-shell setting changes. A change to its mask, blur, compositor opacity or visible mask alone is sent before the next frame, which commits it, and a floating window commits its toplevel only when its own settings change; a hint that sends nothing commits nothing.

Rules settled while building `crabshell-wayland` (Phase 14):

- One calloop `Timer` serves every config timer. After each flush it is armed at the earliest deadline, moved when that changes and removed when none runs. Deadlines are monotonic, so they pause while the machine sleeps. A hand-driven `EventLoop::dispatch` arms a timer made outside the loop only after its first wait.

Rules settled while building `crabshell-wayland` (Phase 15):

- After the flush, taking popups, closing, creating surfaces, updating toplevels, and the windows' layout and draw run in passes, at most 4 an iteration. A layout that writes `geometry` flushes, and another pass runs while that left a lock change, new popups, a window that wants to close, changed surface settings, or a configured window that needs a layout or can draw its damage now. A window waiting for its frame callback doesn't count, so an animation stays at one pass, and a feedback that never settles stops after the fourth pass and goes on at the next event, never by itself.
- `BTN_SIDE` and `BTN_BACK` are Back, and `BTN_EXTRA` and `BTN_FORWARD` are Forward. One scroll frame becomes one `Wheel` with both axes. Notches come from `axis_value120`, or `discrete` times 120 on older compositors. A frame with no source is `Wheel` when it has notches and `Continuous` otherwise. A frame with nothing in it, or a number that isn't finite, sends nothing.
- Pointer buttons, motion and scrolling and touch carry `wl_pointer`'s and `wl_touch`'s event times to the window; an enter carries none.
- `wp_cursor_shape_v1` sends every shape. On a version 1 device, `DndAsk` falls back to `ContextMenu` and `AllResize` to `Move`. The cursor is checked after every loop turn, and only a change is sent.
- `wl_touch` is bound through sctk when a seat has it. The first finger down is a motion and a left press on its surface, its motions move, its lift releases and leaves, and a cancel leaves without a click. Other fingers are ignored until it lifts. A down or lift sets the serial popups grab with and the held state `start_move` checks. Headless Sway has no virtual touch device, so the fake compositor tests touch.
- A touchscreen's finger goes to `Window::touch_at`, so a view whose `mouse_drag` is off still drags under it.
- Sway keeps sending a held button's motion to its surface outside it, with negative or too-large coordinates. A virtual pointer's finger swipe arrives with source `Finger` and `axis_stop` on both axes.

Rules settled while building `crabshell-wayland` (Phase 16):

- One calloop timer serves the GIFs. After each loop turn it is armed at `Renderer::next_image_frame` and removed when that is `None`; when it fires, it advances the GIFs and repaints each one's box through `Window::repaint_image`.

Rules settled while building `crabshell-wayland` (Phase 17):

- Before a pointer button, the window is told the seat keyboard's modifiers, and they reset when the keyboard leaves the shell. A touch doesn't tell them.
- A text area's text input is enabled with the `Multiline` content hint.

Rules settled while building `crabshell-wayland` (gaps run):

- An activation token is asked for with the seat and serial of the last button press, touch or key press on one of the shell's windows, that window's surface while it is still there, and the app id when there is one. An input more than 10 s old goes with no serial, since it more likely didn't cause the launch and its serial may have left the ring of recent serials the compositor checks. The token object is destroyed once `done` comes. Without the global, nothing is asked and programs start at once.
- `Platform::sync` sends `wl_display.sync` and returns a ticket. `Platform::synced(ticket)` turns true when its `done` arrives, after every event the compositor sent before. It never blocks, and the answer wakes the loop.
- A window that has never committed holds its first frame while its draw waits for pictures being decoded. It commits once they come in, or after 250 ms on a one-shot timer that is in the loop only while the window is held. A lock surface's first commit then locks with its picture, and a wallpaper never shows its background color alone. Later frames and other windows aren't held.

Rules settled while building `crabshell-wayland` (Phase 18):

- `ext_data_control_manager_v1` is bound at the first clipboard watch, with a device on one seat that moves to another when its seat goes. Without it, the clipboard is read only while a shell window has the keyboard, and a set takes the latest input's serial.
- A selection is read once per change: text first, else a PNG or JPEG while images are asked for. Text over 4 MiB and images over 8 MiB are dropped, a new selection drops a read still going, and an offer keeps 64 types but is a secret if any of its types is `x-kde-passwordManagerHint`. A secret is never read.
- `zwp_primary_selection_device_manager_v1` is bound at startup, with a device per seat, and a selection is offered with the serial of the release or key that made it. A new paste from either selection drops one still waiting for its program.
- A drag leaving its window calls `start_drag` with the held button's serial, on a version 3 manager only, and offers `application/x-crabshell-drag` after the text or URI types. Its source goes if its button goes first, which cancels it and lets go of the press.
- A drop reads `text/uri-list` first, at most 1 MiB, skipping blank and `#` lines, else text up to 4 MiB, one drop at a time. The accept follows the targets each loop turn, and an offer is finished only once accepted.

Rules settled while building `crabshell-wayland` (Phase 19):

- Until the shell runs alone, a panel with a positive exclusive zone asks for -1, so it sits over the old shell's bar without moving windows, and no session lock is asked for.

Rules settled while building `crabshell-wayland` (0.9 gap fixes):

- Captures share a session only with the captures that waited when it started, so a capture never gets a frame asked for before its own request, nor waits on a live session's next frame. A capture refreshed while its frame is on the way gets a session of its own. Captures take the source's whole frame and end their session with it. A frame's logical size is its output's logical size; for a window, its pixels divided by the largest output scale, which is wrong for a window on a smaller-scale output when scales are mixed.
- Each commit tells the window's screencopy views it drew them, its frame callback that they were presented, and a lost surface that they are off screen.
- Before a panel's layer surface is made, the panel is laid out at the size its settings ask for, with a side left to the compositor taken from the output's logical size, so a size read from the content's geometry is right in the first configure. A height of 0 that the content's geometry fills in is shown once it does.
- After a layout that changes a panel's own width or height, the panel takes that size at once and is laid out again, taking at most 4 new sizes, so 5 layouts. The frame drawn at the new size commits the new settings with it. A configure is a hint: each side the panel last sent as non-zero keeps that size, so a configure answering an older resize can't pull it back, and only a side sent as 0 takes the compositor's size. A size still changing after 5 layouts is logged once with the window's path, "a window's size still changed after 5 layouts", until it settles, and goes on at the next pass.
- A new size, from a configure, a popup's layout or a panel's resize, cuts the masks and the blur region again, and the frame drawn at that size commits them.
- Whenever `image_sources_changed` says an image's status waits, the platform has the renderer load each window's waiting images, drawn or not, at the window's scale. After the draws and loads, a status that changed runs the reactive flush before the loop sleeps, so effects that read it run in the iteration that loaded the picture.
- A lock's `refused` signal turns `true` when the compositor sends `finished` (refused, or ended while `locked` is true) or lacks ext-session-lock-v1, only while the config still wants it locked. It turns `false` once `locked` turns false. A lock that takes never writes it.

## Services

v0.1 had four services in `crabshell-services`, and Phase 9 added six; the phases after it add Quickshell's others (see "Beyond v0.1"). Each one owns its connection, runs on the main thread, and hands out `ReadSignal`s plus a few methods. Every service is created on first use and shared by all bars, so two outputs never mean two connections.

| Service           | Signals and methods                                                                                                                                                                                                                                                                                                                                                                                                                                                         | Source                                                                      | Crate                         |
| ----------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- | ----------------------------- |
| Workspaces        | `workspaces()` gives `Vec<Workspace>` with id, name, output, focused, visible, urgent. `focus_workspace(name)`, and since Phase 12 `outputs()`, `dispatch(command)` and `on_event(kinds, handler)`.                                                                                                                                                                                                                                                                         | Sway IPC at `$SWAYSOCK`                                                     | Own framing with `serde_json` |
| Hyprland          | `workspaces()`, `monitors()`, `toplevels()` with focus, each monitor's workspace and urgency. `focus_workspace(target)`, `dispatch(command)`, `on_event(handler)`.                                                                                                                                                                                                                                                                                                          | Hyprland's sockets in `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/` | Own framing with `serde_json` |
| niri              | `workspaces()`, `windows()`, `keyboard_layouts()`, `overview_open()`. `focus_workspace(id)`, `focus_window(id)`, `action(json)`, `on_event(handler)`.                                                                                                                                                                                                                                                                                                                       | niri's socket at `$NIRI_SOCKET`                                             | Own framing with `serde_json` |
| Tray              | `items()` gives `Vec<TrayItem>` with id (bus name and path), app_id, title, status, category, icon_name, icon_pixmaps, attention_icon_name, icon_theme_path, tooltip, item_is_menu and menu. `activate`, `secondary_activate`, `scroll`, `menu()`, `open_menu`, `open_submenu`, `close_menu`, `menu_event`.                                                                                                                                                                 | StatusNotifierItem and `com.canonical.dbusmenu` on the session bus          | `zbus`                        |
| Audio             | `default_sink()` gives `Option<Sink>` with name, volume_percent, muted. `change_volume(percent)`, `toggle_mute()`. Since Phase 13 each node's and stream's `volumes` and `channels`, `set_sink_volumes`, `set_source_volumes`, `set_stream_volumes` and `peaks(target)`. Since Phase 18 `play_sound(path)`.                                                                                                                                                                 | PulseAudio protocol, served by pipewire-pulse on PipeWire systems           | `libpulse-binding`            |
| Clock             | `clock(precision)` gives a `jiff::Zoned`                                                                                                                                                                                                                                                                                                                                                                                                                                    | System time and time zone                                                   | `jiff`                        |
| UPower            | `devices()`, `display_device()` give `PowerDevice`s with kind, state, percent, present, times to empty and full, energy rate. `on_battery()`.                                                                                                                                                                                                                                                                                                                               | UPower on the system bus                                                    | `zbus`                        |
| Power profiles    | `active()`, `available()` give `PowerProfile`s, and `degradation()` why performance is held back. `set(profile)`.                                                                                                                                                                                                                                                                                                                                                           | power-profiles-daemon on the system bus                                     | `zbus`                        |
| Bluetooth         | `adapters()`, `devices()` with name, address, powered, discovering, state, connected, paired, trusted, battery. `set_powered`, `start_discovery`, `stop_discovery`, `connect`, `disconnect`, `pair`, `set_trusted`, `forget`. `cx.bluetooth_agent()` gives a `BluetoothAgent` whose `request()` is an `Option<PairingRequest>` with the device's path and what BlueZ wants; `answer_pin_code(code)`, `answer_passkey(n)`, `accept()`, `reject()`, `make_default()`.         | BlueZ on the system bus                                                     | `zbus`                        |
| Network           | `wifi_enabled()`, `connectivity()`, `devices()` with kind, state, connection, last failure, Wi-Fi mode and cable link, `wifi_networks()` with SSID, strength, secured, security, connected, `saved_connections()` with uuid, id, kind and settings. `set_wifi_enabled`, `scan`, `connect`, `disconnect`, `forget`, `clear_secrets`, and since Phase 19 `add_connection`, `update_connection` and `scanner()`, and each device's connection uuid, hardware and IPv4 address. | NetworkManager on the system bus                                            | `zbus`                        |
| PipeWire          | `nodes()`, `links()`, `link_groups()`, `screencasts()`, and `node_links(node)`.                                                                                                                                                                                                                                                                                                                                                                                             | PipeWire's native protocol on `pipewire-0`                                  | none                          |
| Media players     | `players()` gives `Player`s with status, track, artists, art, length, position and what each can do. `play_pause`, `next`, `previous`, `seek`, `set_position`, `set_volume`, `set_shuffle`, `set_loop`, `raise`.                                                                                                                                                                                                                                                            | MPRIS on the session bus                                                    | `zbus`                        |
| Notifications     | `list()`, the history, and `popups()`, the ones still showing. `dismiss`, `invoke`, `clear`, `on_closed(handler)`, and since 0.9 `on_notification(handler)`, `set_default_timeout`, `hold_popup`, `restart_popup`, `hide_popup` and `popup_time_left`.                                                                                                                                                                                                                      | The shell serves `org.freedesktop.Notifications` on the session bus         | `zbus`                        |
| Desktop entries   | `applications()` gives `DesktopEntry`s with name, comment, icon, exec, categories, keywords, actions, `no_display` and `dbus_activatable`. `lookup(app_id)`, `launch(id)`, `launch_action(id, action)`.                                                                                                                                                                                                                                                                     | `.desktop` files in the XDG data directories                                | `inotify`, `rustix`           |
| Programs          | `cx.process(command)` gives a `Process`: `running`, `exit_status`, `stdout`, `stderr`, `on_line`, `on_split`, `stopped`, `write`, `close_stdin`, `kill`, `restart`. `cx.exec_detached(command)`.                                                                                                                                                                                                                                                                            | Programs the shell starts                                                   | `rustix`                      |
| PAM               | `cx.pam(service)` gives a `Pam`: `state()` and `messages()`. `start(user)`, `respond(text)`, `abort()`.                                                                                                                                                                                                                                                                                                                                                                     | libpam, a thread per attempt                                                | `nonstick`                    |
| greetd            | `state()` and `messages()`. `create_session(user)`, `respond(answer)`, `start_session(command, env)`, `cancel()`.                                                                                                                                                                                                                                                                                                                                                           | greetd's socket at `$GREETD_SOCK`                                           | Own framing with `serde_json` |
| polkit agent      | `request()` gives an `Option<AuthRequest>` with the action, message, icon, identities, state, info and errors. `respond(text)`, `cancel()`, `pick_identity(name)`.                                                                                                                                                                                                                                                                                                          | polkit on the system bus, and its `polkit-agent-helper-1`                   | `zbus`                        |
| Files and sockets | `cx.file_view(path)` with `error()`, `cx.json_file(path, default)`, `cx.socket(path)` with `on_split`, `cx.socket_server(path)`, and since Phase 18 `cx.folder_view(path)` with `entries()` and `error()`.                                                                                                                                                                                                                                                                  | Files and Unix sockets                                                      | `inotify`, `serde_json`       |
| HTTP              | `cx.http_get(url)` gives an `Http`: `body`, `error`, `loading`, `refresh`, `stopped`, `max_size`, `timeout`. `cx.cover_art(url)` gives a picture's file.                                                                                                                                                                                                                                                                                                                    | The system's `curl`                                                         | none                          |
| logind            | `preparing_for_sleep()`. `delay_sleep_until(ready)`, `power_off`, `reboot`, `suspend`, `hibernate`.                                                                                                                                                                                                                                                                                                                                                                         | logind on the system bus                                                    | `zbus`                        |
| Brightness        | `devices()` gives `Backlight`s with name, brightness and max_brightness. `set(name, value)`.                                                                                                                                                                                                                                                                                                                                                                                | `/sys/class/backlight`, set through logind                                  | `inotify`, `zbus`             |
| System stats      | `cx.system_stats(period)` gives CPU use, memory and temperatures.                                                                                                                                                                                                                                                                                                                                                                                                           | `/proc/stat`, `/proc/meminfo`, `/sys/class/hwmon`                           | none                          |
| Locale            | `cx.locale()` gives month and day names and `strftime`.                                                                                                                                                                                                                                                                                                                                                                                                                     | glibc, through `locale -k LC_TIME`                                          | none                          |

### Workspaces

- Write the IPC framing in-house. Each message is a 14-byte header (`i3-ipc`, a length, a type) plus JSON, about 150 lines of code, and owning it avoids pulling in another async runtime.
- On start, send `SUBSCRIBE` to `workspace` events, then `GET_WORKSPACES`, so no change between the two goes unseen. On every event, send `GET_WORKSPACES` again and replace the whole list. That is simpler than applying each event's changes and costs one small JSON reply.
- If the socket closes, set the list to empty and reconnect every 2 s. A first connect that fails, as with a stale `SWAYSOCK` under another compositor, logs once and never retries, so it never wakes the bar.
- Other wlroots compositors don't speak Sway IPC. Phase 12 added `ext-workspace-v1` through `cx.window_manager()`, and Hyprland's own IPC.

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
- `change_volume` clamps to 0-100%.
- If the server goes away, the signal becomes `None` and the service retries every 2 s for up to a minute after startup or after a working connection drops. Then it logs once and stops until the shell restarts.

### Clock

- `Precision::Second`, `Precision::Minute` or `Precision::Day`. Each tick computes the next boundary from wall-clock time, so drift never builds up; a day's is local midnight. Bars on every output share one clock per precision.
- Timers pause during suspend, so after resume the clock could show the wrong time for up to a minute. Listen for logind's `PrepareForSleep(false)` on the system bus and re-sync right away.
- Re-read the time zone when `org.freedesktop.timedate1` reports a `Timezone` change. Also re-read it on every tick: without timedated, as under OpenRC, nothing announces a changed `/etc/localtime`.

Rules settled while building `crabshell-services` (Phase 3, steps 1 to 3):

- `provide(cx, &handle)` runs once in `Shell::new`. It keeps the root `Owner` and a weak loop handle, never a `Cx` or a strong handle, so the loop and the compositor connection still go when the shell drops.
- Each service is its own services-map entry, starts on first use, and creates its signals under the root owner. Without `provide`, it warns once and hands out a still value under the caller's owner.
- One calloop futures executor runs every service's D-Bus futures. Nothing a service does at startup delays the first frame.
- Clock: the Unix timestamp is truncated to the precision, and a timer up to 50 ms early counts as on the boundary. A re-sync sets the signal and replaces the timer. logind and timedated are reached through match rules only, so nothing is bus-activated. With no system bus the clock logs once and keeps ticking. The system bus is reached 1 s after the first clock starts, so its connection stays off the first frame; a suspend before then leaves nothing to resync. A watcher that has just reached the system bus resyncs once, for a suspend whose resume came before its match rules.
- Clock: a day clock's boundaries are midnights in the zone read at each tick, through jiff's `start_of_day`, so on a day the clocks change it ticks after 23 or 25 hours. It resyncs with the others after `PrepareForSleep(false)` and a timedated zone change.
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
- Bluetooth: read with `GetManagedObjects` at `/`, keeping `Adapter1`, `Device1` and `Battery1`. A device's name is its `Alias`, else its `Name`, else its address. `Pair` and `Connect` may take 90 s, since BlueZ answers a pairing once the user confirmed it; other calls 25 s.
- Bluetooth: the first `cx.bluetooth_agent()` registers an `org.bluez.Agent1` at `/org/crabshell/BluetoothAgent` as `KeyboardDisplay`, on a connection of its own, and registers again whenever BlueZ gets a new owner. It asks to be the default agent only after `make_default`. Only the unique name that took the registration may call; others get `AccessDenied`.
- Bluetooth agent requests: one shows, and a new one answers the unanswered one `Canceled`. Codes to show are answered at once and show until BlueZ cancels them, and a passkey's typed count updates the same request. A PIN code is 1 to 16 letters and digits and a passkey at most 999999; any other answer is dropped and logged. BlueZ's `Cancel` answers the waiting call `Canceled`, and the root going answers it `Rejected` and unregisters. `Bluetooth::pair` goes out on the agent's connection, since BlueZ asks the agent of whoever called `Pair`; pairings others start reach the shell only after `make_default`.
- Network: read with `GetManagedObjects` at `/org/freedesktop`, leaving out loopback and unmanaged devices. One network per SSID and device, from its strongest access point, strongest first; hidden SSIDs are left out, and SSIDs are cut to 32 bytes and shown as lossy UTF-8 while `connect` sends the access point's own bytes. A password connection sends only the key and lets NetworkManager pick WPA2 or WPA3; without one, NetworkManager picks a saved connection, and an open network with none gets a new one. Enhanced Open counts as open. A password is never logged or kept.
- Media players: every `org.mpris.MediaPlayer2.*` name but playerctld, which mirrors another player, at most 32, in the order they appeared. The position is read when a player appears and when its status, rate or track changes, and taken from `Seeked`; until such a read answers, a new status or rate takes effect from where playback got to. Nothing polls.
- A track without a title, or with only spaces, is named by the last part of its `xesam:url` when that is a `file://`, `http://` or `https://` URL, cut at `?` or `#` and percent-decoded as file managers show a file's name. The title stays `""` for another URL, or one with no file name.
- Notifications: the shell queues for `org.freedesktop.Notifications` and lets another daemon replace it, serving while it holds the name and keeping the history through a loss. It offers actions, body, icon-static and persistence. Tags in a body are taken out and entities decoded. A popup shows for its timeout, 5 s by default, and a critical one or one that never expires until dismissed; one loop timer exists only while a popup waits to expire. Summaries are cut to 1 KiB, bodies to 4 KiB and other strings to 256 bytes; at most 16 actions, 256 notifications and 8 MiB of pictures are kept.
- Notifications: on a reload the history, the popups with their time left, and the next id are saved under the persist key `crabshell::services`, and the next shell shows them again once a config asks for notifications, so apps' ids go on. A transient one whose time ran out goes, and pictures sent as pixels show the app's icon instead. Restored notifications keep to the same caps, and their expiries wait until the bus answers the name request, so a popup that expired during the swap is still told to its app. During a reload the new shell takes the name and asks the old one for what it got after its save, so no call is lost.
- Audio: sinks, sources and streams are read whole on each of their events. A failed read keeps the last list. A sink's monitor isn't listed as a source, but can be the default source. Setting a volume keeps the channels' balance. Peak meters came in Phase 13.

Rules settled while building `crabshell-services` (Phase 10):

- Programs run in a process group of their own, with their outputs through non-blocking pipes, so a script can open `/dev/stdout`. A program starts on the loop's next turn, so `on_line` and `with_stdin` set at once apply. Each output keeps up to 1 MiB in its signal, and nothing after a character that didn't fit; lines reach `on_line` past that, and a line over 1 MiB is dropped and logged. Input written before the start waits for it; `close_stdin` closes once everything written is sent; 4 MiB unread closes it at once.
- A program's end comes from a pidfd. What is left in its pipes is read then, up to 1 MiB each, as a program it started may hold them open and keep printing. A run reaped behind the shell's back ends with no status rather than spinning. A restart, `kill` or the context going sends `SIGTERM` to the group; a stopped run, and every `exec_detached` program, is reaped through a pidfd that leaves the loop once the child is gone. A panicking command stops the run before and is logged with where `cx.process` was called; the next change tries again.
- Programs the shell starts don't inherit the supervisor's socket variable or the shell's own glibc tunable.
- Sockets connect at once and never retry on their own. A refused write stops writing, but reading goes on to the end, which, like a reset, still passes on the last line. 4 MiB unwritten closes the socket. Lines read before the config closes or reconnects aren't passed on after. A socket closes with its context and never connects after.
- Files are read on the main thread, up to 16 MiB, as UTF-8. They are followed through inotify on their directory, or while it is missing the nearest directory above it, watched again when a directory on the way comes, goes or moves. A file is read again on a close after writing, a rename or a removal, and 50 ms after the first write to a file still open, not on a create, when nothing is written yet.
- That first write is heard through an `IN_MODIFY` watch on the file's own inode, added while a regular file is at the path and again whenever its name is made, renamed onto, written or removed. Directory watches leave writes to open files out, so a log beside a followed file wakes nothing. A file made and already written when its watch comes is read 50 ms on. The timer exists only while it waits, and a close or any other change reads the file first and ends it.
- A write goes through a new file renamed over the old, making the directory if it is missing, following a link and keeping the permissions.
- A JSON file writes only a value the config set, not the one it started from, and never writes back what it just read. The config's `Serialize`, `Deserialize` and `PartialEq` are caught: a panic is logged with the path, and the value and the file go on.
- One inotify instance per shell, in the loop only while a watch is alive.
- The kernel keeps one inotify mask per inode. Every directory takes one shared mask without `IN_MODIFY`, a followed file takes `IN_MODIFY` alone, and a link takes `IN_DONT_FOLLOW` with `IN_ATTRIB`, `IN_DELETE_SELF` and `IN_MOVE_SELF`. Only a regular file gets the file mask, so a directory's is never replaced. Removing a watch makes the kernel say so once, a wakeup when the last view of a file goes.
- Desktop entries come from `applications/` under `$XDG_DATA_HOME` and each of `$XDG_DATA_DIRS`, recursively, links followed and each directory read once. The first file with an ID decides it, even one left out for `Hidden`, its type or a missing `TryExec`. `NoDisplay` and `OnlyShowIn`/`NotShowIn` against `XDG_CURRENT_DESKTOP` set `no_display`. Localized keys follow `LC_ALL`, `LC_MESSAGES` and `LANG` in the spec's order. Exec is parsed by the spec's quoting rules with no shell: `%f %F %u %U` and the deprecated codes are dropped, `%i`, `%c` and `%k` expand, and an unknown code runs nothing. A line the rules refuse is split as GLib's `g_shell_parse_argv` splits it: by sh's quotes and backslashes, with nothing expanded and `#` starting a comment at the start of a word. This is logged once per entry. A line both refuse, such as one with an open quote or a trailing backslash, runs nothing. A `Terminal` entry runs in `xdg-terminal-exec`.
- An entry with `DBusActivatable=true` is started with `org.freedesktop.Application.Activate`, or `ActivateAction` with no parameter for an action, with the activation token in the platform data when there is one. The call goes to the path the spec derives from the ID, over a session-bus connection made for the call, and has 25 s to answer. A refusal runs `Exec` instead: `ServiceUnknown` or `NameHasNoOwner` from the bus, `UnknownObject` or `UnknownMethod` from the app, an ID that isn't a well-known bus name, or no session bus. Any other error, a timeout included, is logged and runs nothing, since the app may have started and a second would open two windows. With no `Exec`, nothing starts, and that is logged. `Open` waits for a launch that passes files.
- A launch waits for its activation token at most 100 ms, on a timer that is in the loop only while it waits, and a token that comes later is dropped. `Exec` gets the token in `XDG_ACTIVATION_TOKEN` and `DESKTOP_STARTUP_ID`, and `Activate` and `ActivateAction` get it as `activation-token` and `desktop-startup-id` in the platform data. A program the shell starts never inherits the token the shell itself was started with, unless the command sets one. A launch still waiting when the shell exits, through `cx.exit`, a signal or a reload, starts without a token, except a D-Bus activation, whose call doesn't finish before the exit. If the wait's timer can't be added, the program starts at once without a token.
- The list is read on the loop's next turn after the first call, and again once the files have been still for 100 ms after a change to an entry, a directory or a link; the timer exists only after a change. Files over 64 KiB, lines over 4 KiB, more than 10,000 entries and anything but regular files are left out. `lookup` tries the ID, then the ID ignoring case, then `StartupWMClass`, then the ID's last dotted part.
- Each link on the way to an `applications` directory, and on the way to what each link names, at most 40, is watched itself with `IN_DONT_FOLLOW`, `IN_ATTRIB`, `IN_DELETE_SELF` and `IN_MOVE_SELF`, never the directory that holds it, which may be `$HOME`. A link renamed over it, as NixOS swaps profiles, takes its last name. That reads the list again, and the new read watches the new link.

Rules settled while building `crabshell-services` (Phase 11):

- greetd: one connection, made at the first request and again after one is lost, with one request in flight, as greetd-ipc(7) has it. A message is its length as a native-endian `u32`, then JSON, at most 64 KiB; texts are cut to 4 KiB and the last 64 informative messages kept. Informative and error messages are acknowledged at once, and an error on a create or an answer cancels the attempt. A new session cancels the one greetd holds first, and replies to an attempt a queued cancel ends show nothing.
- `respond` goes only while greetd asks and `start_session` only once authenticated; otherwise they are dropped and logged. greetd joins a session's command with spaces and runs it with sh(1), so `start_session` documents that an entry with spaces needs its own quotes rather than quoting it. An answer is written into one buffer, zeroed once sent, and never logged.
- PAM: each attempt runs `pam_authenticate` on a thread of its own through `nonstick`, and its channel is in the loop only while it runs. A success also runs the account step, where `pam_faillock` forgets past failures; its refusal is only logged. An abort, a second start or the asking context going refuses the waiting question, which ends the thread; under `pam_faillock` that counts as a failed login.
- PAM has no cancel, so an aborted attempt turns idle at once and its result is dropped, but its thread stays inside PAM until the module returns. A start meanwhile shows `Busy` and begins once that thread has returned, so two transactions never run side by side. `start(None)` finds the user by uid through NSS.
- PAM's auth error is `Failure`, and the rest is an `Error` with a kind: `StartFailed` when the transaction can't start, a name holds a NUL, or NSS doesn't know the shell's user, `InternalError` when the shell's own conversation refuses an answer or an attempt ends without a result, and `TryAuthFailed` otherwise. Texts are cut to 4 KiB and the last 64 messages kept, and an answer with a NUL is refused before libpam. Answers are zeroed once handed on, except the copy nonstick makes for libpam.
- polkit: the first `cx.polkit_agent()` registers as `unix-session` from `XDG_SESSION_ID`, else `unix-process`, at `/org/crabshell/PolkitAgent`, and again whenever polkit's name gets a new owner; a refusal is logged. Only the unique name that took the registration may call; others get `AccessDenied`. One request shows and 8 wait; past that, or with a bad cookie or no known user, polkit gets `Failed`.
- polkit: each try runs `polkit-agent-helper-1` as the first identity, the session's user first, with the cookie on its input and a line per prompt or message. Identities are the `unix-user`s polkit names, found by uid through NSS on a thread of their own, so users from LDAP or systemd-homed count and a slow lookup never holds up the shell. A request waits at most 3 s for their names and goes on with those found, or answers `Failed` with none. Requests being looked up count toward the 8 that may wait, and with 4 lookups NSS never answered still alive, a new request answers `Failed` at once.
- The agent connects to `/run/polkit/agent-helper.socket` first, where polkit 127 and later start the helper as root, and sends the user's name before the cookie. Only when nothing listens there does it spawn the first helper that exists of `/usr/lib/polkit-1/polkit-agent-helper-1`, `/usr/libexec/polkit-agent-helper-1`, `/usr/libexec/polkit-1/polkit-agent-helper-1` and NixOS's `/run/wrappers/bin/polkit-agent-helper-1`, logging why if the socket is there but refuses.
- `SUCCESS` answers polkit with an empty return; after 3 failures it gets `Failed`, and a helper that can't start answers `Failed` at once. Answers with a line break or NUL are refused, and a cancel kills the helper. Texts are cut to 4 KiB, with at most 64 identities.
- No test reaches a real account: PAM's tests run a scripted backend, and the one libpam test uses `other` and skips unless it is `pam_deny` only. polkit's run on a private bus with a fake helper, and greetd's against a fake socket. Two ignored tests reach a real polkit and a real greetd, and do nothing unless their test passwords are set; they are for a throwaway container with a test user.

Rules settled while building `crabshell-services` (Phase 12):

- Sway: the connection subscribes to `workspace`, to `output` from the first `outputs()`, and to each kind a handler asks for, never to the rest. Sway has no unsubscribe, so an event of a kind only a gone handler wanted makes the service connect again with what is still wanted, once Sway answered every request sent, since it drops a client that hangs up with requests unread. The lists stay until the new ones arrive; an event never subscribed to changes nothing.
- Sway: `GET_OUTPUTS` is sent again once per read that held an output or workspace event. An output without `active` or `rect`, as a VR headset, shows as inactive, and one without a scale reads 1. A bad list is logged and the last one kept.
- Sway: handlers run after the read, with nothing borrowed, in the order given, and an event must be UTF-8 JSON within the 4 MiB cap. A handler that panics is logged with where `on_event` was called and taken away. `dispatch` sends the text as written; over 4 MiB or while disconnected it is logged and dropped. The services' Sway tests also run against a private headless Sway, ignored by default.
- Hyprland: the event socket opens first, then `j/workspaces`, `j/monitors`, `j/clients` and `j/activewindow` are read. Each request has a connection of its own, goes in one write with its writing then shut down, and ends when Hyprland closes it. At most 8 requests are out at once, under Hyprland's listen backlog of 10, and the rest queue in order, at most 64 dispatches of them. Connects never wait: while the backlog is full, a request waits at the head of the queue for one of the shell's requests to end, for a new one to be made, or, with none of the shell's out, for a one-shot timer that exists only while it waits: 100 ms, twice as long after each try that fails, up to 2 s, and 100 ms again once a send gets out. A Unix socket whose connect found the backlog full is unconnected, and Linux reports it writable and hung up at once, so its readiness can't be waited on. On the event socket a full backlog counts as unreachable.
- Hyprland: an event reads again the lists it can change, a burst read at once reads each list once, and an event during a read reads once more after it. `activewindow` reads the windows too, since a class change comes only that way. Both versions of an event count, for a Hyprland without the v2. The lists show once every read a burst of events started is answered, so a frame doesn't mix one event's new lists with old ones; a list read again for a later event holds nothing back.
- Hyprland: without the signature or `XDG_RUNTIME_DIR`, or with the event socket missing, the lists stay empty, that is logged once, and nothing retries. When the event socket closes while its file is still there, the shell connects once again and reads every list, keeping the old ones meanwhile, and urgency ends for the window the first focused-window read names and for each window the first window list lacks; one that closes before any event came, or whose file is gone, empties the lists for good.
- Hyprland: `j/status` is read once, at the first `focus_workspace`. A `configProvider` of `lua` means a switch is `hl.dsp.focus({ workspace = "…" })`, with backslash and quote escaped and control characters written as `\ddd`; `hyprlang`, or `unknown request` from Hyprland before 0.55, means `workspace …`. Any other reply is logged and read as the old syntax. Switches wait for the answer, at most 64, and go if it can't be asked.
- Hyprland: workspace references match by id when both have one, otherwise by the `address` 0.57 added, and named workspaces sort first. A shown special workspace is active, and focused on the focused monitor. `fullscreen` is a flag in older Hyprland and a mode later, where 2 is fullscreen and 1 maximized. Hyprland 0.42 and later are supported; on 0.41 and older, from 2024, a maximized window counts as fullscreen. Urgency comes only from `urgent` events and ends on that window's `activewindowv2` or `closewindow`.
- Hyprland: a reply over 8 MiB, or one that isn't JSON, is logged and the last list kept, and an entry that doesn't parse is left out. Texts are cut to 4 KiB at a character, at most 4096 entries per list and 4096 urgent windows are kept, and event lines past 1 MiB are dropped. A dispatch over 8 KiB isn't sent, and any reply but `ok` is logged. Handlers run after the event's reads are sent, with nothing borrowed, inside `catch_unwind`, and one that panics is logged and not called again.
- Hyprland is tested against a fake Hyprland built from its docs and its BSD-3 source; on 2026-11-15 its IPC also passed a hand check on Hyprland 0.57.2 with a Lua config.

Rules settled while building `crabshell-services` (Phase 13):

- A service reads a signal's old value untracked before it sets it, and runs `on_closed` handlers untracked, so an effect that calls a service never runs again for what the service shows. A D-Bus daemon's watch ends as soon as the root owner goes, not at its next message, and clears what it held.
- Tray: `Category` is read with the rest. A missing one is `ApplicationStatus`; an unknown one is logged at debug and read as `ApplicationStatus`.
- Power profiles: `PerformanceDegraded` that is `""` or missing is `None`; a value the daemon's docs don't name is `Other`, logged at debug.
- Bluetooth: `PowerState` is read beside `Powered`; without it, or with a value BlueZ's docs don't list, the state comes from `Powered`. A `connect` or `disconnect` shows Connecting or Disconnecting until BlueZ answers that call or `Connected` changes to where it was headed. A newer call replaces the state, an older call's answer then changes nothing, and BlueZ leaving ends them all.
- Notifications: `on_closed` handlers run after the lists change, in the order given, with nothing borrowed, inside `catch_unwind`; one that panics is logged with where `on_closed` was called and removed. A dismiss, a clear or an action of a non-resident notification is `Dismissed`, `CloseNotification` is `CloseRequested`, and a transient popup timing out or the oldest leaving a full history is `Expired`. A popup that isn't transient and times out closes nothing.
- Network: NM's states map as 100 `Connected`, 40 to 90 `Connecting`, 110 `Disconnecting`, 30 and 120 `Disconnected`, anything else `Unavailable`. A failure's reason comes from `StateReason` in state 120 (7 `NoSecrets`, 8 `WifiClientDisconnected`, 9 and 10 `WifiClientFailed`, 11 `WifiAuthTimeout`, 53 `WifiNetworkLost`, else `Unknown`) and is kept until the device enters states 40 to 100; a whole re-read forgets it. Security is the strongest key management an access point offers: Suite-B 192, SAE, PSK, 802.1X on RSN, then PSK and 802.1X on WPA, then OWE; the privacy bit alone is `Wep`, and nothing is `Open`. A cable's link is the carrier bit of `InterfaceFlags`, else `Carrier`.
- Network: saved connections are read only from the first `saved_connections()`, over a bus connection of their own with two match rules, and read again whole on each Settings signal or `Updated`. The first 256 by number are read, a reply over 64 KiB is left out, and at most 1 MiB of memory is kept, counted as what the JSON holds, names included; past that a connection is listed with `settings: null`. A byte array is an array of numbers, a dictionary an object with non-string keys written as their JSON, a structure an array, and a file descriptor or NaN `null`. Secrets are never asked for. Settings written back are typed as nm-settings-dbus(5) documents them, and a value of another shape is refused by its property's name, with nothing sent.
- I/O: a marker may be several bytes and is found across reads; `""` hands over each read, keeping a character a read cut. Lines go to the handler one at a time, with nothing borrowed; one that sets another marker has everything after its own line split at it, and a marker set outside a handler splits what waits on the loop's next turn. A program's lines split at a newline until a handler says otherwise. A line over 1 MiB is dropped, counting only bytes known to belong to it.
- I/O: a file is checked with stat, opened without waiting and checked again, so a pipe, a directory or a device reads as `NotAFile` and never blocks the shell.
- I/O: a socket server listens at once, without making directories. It replaces a socket file nothing listens on and leaves alone, logged, a live socket, a file that isn't a socket or a link. At most 64 connections are open; one past that is closed at once and logged. Each connection's context goes on the loop's turn after it ends.
- A client that only stops sending has its last line passed on, and its connection stays open for the reply until the config closes it, a write fails, or 5 s pass. One that hangs up ends it at once. An accept error that leaves a client waiting rests the listener for 1 s.
- Stopping closes the listener and every connection, and removes the socket file only if its device and inode are the ones the server made. The file gets the umask's permissions, so keep it under `$XDG_RUNTIME_DIR`.
- Audio: per-channel volumes are rounded as `volume_percent` is, at most `PA_CHANNELS_MAX` channels, and a short channel map is padded with `Unknown`. A per-channel setter needs one percent per channel, each at most 100, or sends nothing.
- Audio: a peak meter is a record stream with `PEAK_DETECT`, 32-bit floats at 25 a second, one frame per fragment, in the target's channel map: a sink through its monitor, a source itself, a stream through its sink's monitor kept to that stream. The target is a memo, so a sink's volume or mute changing doesn't reopen it.
- A meter opens once its target is listed and closes when its context goes, its target changes or leaves, or the connection drops. A sink's or stream's meter is corked, showing zeros, while its sink isn't running or its stream is paused; a source's meter keeps recording. Peaks are each channel's loudest sample since the last read, at most 1, NaN as 0, and set only when they change.

Rules settled while building `crabshell-services` (Phase 17):

- `cx.notifications().set_body_markup(true)` makes the server list `body-markup` and `body-hyperlinks` in `GetCapabilities`, so programs send the spec's markup, and keeps each body as sent, for `Text::rich(parse_markup(&body))`. It is off by default, and then tags are taken out and entities decoded. `body-images` is never listed, since an `img` shows its alt text. A change applies to notifications that arrive after it.
- With markup on, a body is cut at 4 KiB as before, and a tag or entity the cut splits is dropped, so a half tag doesn't show as text.

Rules settled while building `crabshell-services` (Phase 18):

- HTTP: curl runs with `-q` first, `--globoff`, `--proto =http,https` for the URL and each redirect, at most 5 redirects, `--fail`, `--max-time` and `--max-filesize`, with the URL as one argument. The shell counts the bytes too, stops curl past the cap, and stops every running curl before it exits.
- Cover art: a file is named by the URL's FNV-1a hash and the format its first bytes show, downloaded to a part file and renamed once whole. The cache is trimmed after each download, oldest first, to 64 MiB and 200 files. It must be the user's own directory, not a link; a wider one is tightened to `0700`, and anything else shows no remote art.
- logind: one match rule, for `PrepareForSleep` only. A delay's inhibitor belongs to its effect, so the context going or a panic in `ready` lets it go, and one that arrives from a logind that restarted is closed and the new one asked. A refused inhibitor waits for the next resume.
- Brightness: at most 32 backlights, listed once, each value read at most 64 bytes. Each one's `actual_brightness` is watched for `IN_MODIFY`, which the kernel sends for program writes and the firmware's changes alike.
- niri: one connection sends `"EventStream"` and keeps the lists as niri-ipc's `EventStreamState` does. Unknown events are skipped. A stream that closes is opened once more, and lost for good if it closes again before an event past niri's opening state, or on a line over 1 MiB. Texts are cut to 4 KiB and lists to 4096. Each action is one JSON line on a connection of its own, at most 8 KiB and 64 waiting. niri is GPL, so its IPC was read as a spec only and the tests use a fake niri.
- Folders are watched with the shared directory mask, so a write shows once its file is closed. A folder reached through a link isn't followed when its target is removed and made again, as with a file.
- Stats are read on one thread, started with the first sampler and ended with the last, since an NVMe sensor can take 100 ms to answer. A tick while a read runs is skipped, and a panic there is logged and gives an empty sample. They read regular files only, 64 KiB each, and count guest time in user time. Sensors are listed at the first sample and again only when one goes, and a chip whose `runtime_status` is `suspended` isn't read, so a sleeping GPU stays asleep.
- Locale: one `locale -k LC_TIME` run with `LC_ALL` set, on a thread, its output capped at 64 KiB.
- Sound: WAV is parsed in-house, PCM U8, S16, S24, S32 and F32, 1 to 8 channels, up to 16 MiB, unknown chunks skipped. A stream plays with `media.role` `event` and goes once drained.

Rules settled while building `crabshell-services` (Phase 19):

- PipeWire: one connection says hello as core version 3, asks for the registry and binds each link for its state; nodes come from the registry's properties. The graph shows once a sync after the last bind is answered. A proxy id is one PipeWire gave back with `RemoveId`, else the next. A message over 1 MiB, or 1 MiB unsent, drops the connection for good; at most 4096 nodes and links and 4 KiB texts; a closed connection is opened once more, as niri's is. In-process it costs about 1.5 MiB, against libpulse's 2.3.
- Network: settings are typed as nm-settings-dbus(5) says, numbers widened where NetworkManager converts and exact inside arrays of objects, and never logged. Scanners share one timer that exists only while one lives; a scan refused as too soon is logged at debug. A device's IPv4 address is the first in its `Ip4Config`'s `AddressData`.
- Screen reader: `org.a11y.Status` is followed once the first frame shows, and read only while `org.a11y.Bus` has an owner, so the launcher is never started. `AT_SPI_BUS_ADDRESS` names the bus when set. `Accessible`, `Application`, `Component`, `Action`, `Text`, `Value`, an empty `Cache.GetItems` and `Peer.Ping` are answered by hand, reading the live tree; the registry pings each application when a client leaves and drops one that errs; only `Value.CurrentValue` is writable. A failed join is logged and tried again at the next status change.
- Reload: the polkit agent, socket servers and global shortcuts start once the shell runs alone, and a polkit refusal right then is tried once more after 500 ms. The new shell takes `org.freedesktop.Notifications` with replacement, holds up to 256 calls, and asks the old one by `dev.crabshell.Handoff.Take`, answered once, only to the name's new owner, within 16 MiB and 2 s. After answering, the old shell still saves itself as the owner until the name moves again.

Rules settled while building `crabshell-services` (0.9 gap fixes):

- Notifications: `Timeout::Default` is 5 s until `set_default_timeout`, which applies to popups that arrive or are replaced after it; those showing keep their countdown. A timeout too long for the clock to hold shows until dismissed.
- Notifications: `hold_popup` stops a popup's countdown, and no timer runs for a held popup. A popup replaced while held stays held with its whole timeout. A reload resumes held popups with the time they had left, and saves each popup's whole timeout beside it; a save from 0.8 has none, and then the time left counts as whole.
- Notifications: `hide_popup` ends a popup as expiry does: it stays in the history unless transient, and only a transient one's app and `on_closed` hear `Expired`.
- Notifications: `on_notification` handlers run on the main thread, untracked and with nothing borrowed, before the popups change; one that panics is logged with where it was given and removed.
- Notifications: `popup_time_left` isn't a signal; a countdown bar reads it in a binding that `cx.frames` ticks while the popup counts and isn't held. It is `None` for a popup that never expires, has a zero timeout or isn't showing.
- Desktop entries: `DesktopEntry::command` is what `launch` runs, split and expanded once when the list is read, by the same function `launch` uses. An `Exec` the spec's quoting refuses is split as a POSIX shell would, with no `$` or glob expansion, as GLib does; one that still makes no sense gives `[]`. The launch logs a refused `Exec`, not the read.
- `restart_popup` gives a popup its whole timeout from now, as a replacement does. A held one stays held with its whole timeout left. A reload keeps the restarted time left.

Rules settled while building the `Tray` widget (Phase 4, step 5):

- `Tray::new()` takes `icon_size` (16 by default) and `menu_style`, the menu popup's style (light text on dark gray by default). Both take a value or a closure.
- It is built from `crabshell-ui`'s widgets. `cx.tray()` needs a context, which a keyed child's view gets, so the tray is one such child holding a row of the items. `Prop::shared` lets one property feed many widgets.
- A click opens the menu popup at once, so it grabs the pointer with the click's serial. The popup is clear and empty until the app's layout arrives. It closes once the tray stops following its opening: the app's menu went, a read failed (the first one too), or the app left. `menu_style` is the `Menu`'s style, so submenus get it too.
- When the popup goes, the tray stops following the app's menu, unless the menu was opened again since. `open_menu` numbers each opening for that.
- A widget doesn't know its output's scale, so pixmaps are picked for twice the icon size. An item with no icon shows `image-missing`.

## Crates

One Cargo workspace with seven crates. Each crate owns one area, so an agent can work inside one without touching the others.

| Crate                | Owns                                                                                                                                                                                                                                                                  | Depends on           | Main external crates                                                                                                                              |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crabshell-reactive` | Signals, memos, effects, owners, flush                                                                                                                                                                                                                                | none                 | `slotmap`                                                                                                                                         |
| `crabshell-ui`       | `Cx`, widgets, styles, layout, text layout, hit testing, focus, invalidation                                                                                                                                                                                          | reactive             | `taffy`, `cosmic-text`, `fontconfig`, `unicode-segmentation`                                                                                      |
| `crabshell-render`   | Drawing into pixmaps, damage tracking, glyphs, icons, images                                                                                                                                                                                                          | ui                   | `tiny-skia`, `resvg`, `png`, `swash`, `zune-jpeg`, `gif`, `image-webp`                                                                            |
| `crabshell-wayland`  | Event loop, windows, popups, outputs, pointer, keyboard, text input, clipboard, primary selection, data control, shm buffers, idle, toplevels, session lock, workspaces, shortcut inhibitors, global shortcuts, focus grabs                                           | reactive, ui, render | `smithay-client-toolkit` (with `xkbcommon`), `calloop`, `wayland-protocols`, `wayland-protocols-wlr`, `wayland-scanner`, `wayland-server` (tests) |
| `crabshell-services` | Workspaces, tray, audio, clock, power, Bluetooth, network, media players, notifications, desktop entries, programs, files, folders, sockets, PAM, greetd, polkit, Hyprland, niri, logind, brightness, HTTP, system stats, locale, PipeWire's graph, the screen reader | reactive, ui         | `zbus`, `serde_json`, `libpulse-binding`, `jiff`, `calloop`, `serde`, `inotify`, `rustix`, `nonstick`, `nix`                                      |
| `crabshell`          | Public API, `run()`, `prelude`, `cx.persist`, `cx.ipc`, `cx.quantize_colors`, `cx.toplevels`, `cx.session_lock`, `cx.exit`, `cx.window_manager`, `cx.global_shortcut`, `cx.clipboard`, `cx.icon_path`, `cx.reload`                                                    | all of the above     | `serde`, `serde_json`, tracing-subscriber, `signal-hook`, `wayland-server` (tests)                                                                |
| `crabshell-cli`      | The `crabshell` binary with `init`, `run`, `check`, `ipc` and the supervisor                                                                                                                                                                                          | none                 | `calloop`, `rustix`, `serde_json`, `inotify`, `zbus`                                                                                              |

`Cx` lives in `crabshell-ui` and carries a map of services keyed by type. `crabshell-services` adds `cx.sway()`, `cx.audio()`, `cx.tray()`, `cx.clock()` and the services of Phases 9 to 12 through an extension trait, so `crabshell-ui` never depends on services.

### Dependency policy

- Pin every external version once, in the workspace `Cargo.toml` under `[workspace.dependencies]`.
- Add a crate only when it saves more than about 200 lines or implements a protocol or file format. Add it to the table above first.
- `nix` 0.31.3 (MIT), with only its `user` feature, looks users up by uid through NSS with a safe `getpwuid_r`, and is the version calloop already uses. `wayland-server` (MIT) builds the fake compositor the wayland and `crabshell` tests run against, and is a dev-dependency only.
- Rust crates must be MIT, Apache-2.0, BSD, ISC, Zlib or Unicode licensed. `cargo deny check licenses advisories` enforces this with `deny.toml`, in CI on every push, and fails on a security advisory against any dependency unless `deny.toml` ignores it with a reason.
- HTTP goes through the system's `curl`, not a TLS crate: rustls with ring and native certificates would add 9 crates and 1.4 MiB to the counter example, and curl reads the system's certificates and proxy settings. Locale names come from glibc's `locale`, since `pure-rust-locales` kept about 0.15 MiB resident.
- System C libraries (libpulse, fontconfig, libxkbcommon, libpam) are linked dynamically. sctk speaks the Wayland protocol in pure Rust, so libwayland-client isn't linked.
- Use edition 2024 on the latest stable toolchain. No nightly features.

## Performance targets

The baseline is Quickshell running the same bar, with workspaces, tray, volume and clock, on the same machine and output. Use Quickshell's `Quickshell.I3` module for workspaces, and check that it connects to Sway's socket. Don't compare against numbers found online.

| Metric                                          | Target                                                     | Measured                                                                     | How to measure                                                      |
| ----------------------------------------------- | ---------------------------------------------------------- | ---------------------------------------------------------------------------- | ------------------------------------------------------------------- |
| Idle memory (RSS), v0.1 bar on one 1080p output | 30 MB or less, and at most a quarter of Quickshell's       | 22.4 MiB, 18% of Quickshell's 125.2 MiB; the counter example 19.7 MiB        | `ps -o rss= -p <pid>` after 60 s idle                               |
| Idle wakeups                                    | 1 per minute with a minute clock and nothing else changing | The bar: 2 context switches at the minute boundary, none between. Counter: 0 | `perf stat -e 'syscalls:sys_exit_epoll_wait' -p <pid> -- sleep 300` |
| Startup to first frame                          | 100 ms or less                                             | 13.4 ms for the counter, counted from the first start                        | The shell logs the time from `main` to its first committed buffer   |
| Workspace switch to screen                      | Within one frame, 16 ms at 60 Hz                           | Median 0.5 ms, slowest 0.7 ms                                                | The shell logs the time from the IPC event to the commit            |
| Rebuild after a one-line edit                   | 10 s or less                                               | 1.0 s                                                                        | The supervisor logs each build's duration                           |
| Time a reload leaves the screen without a bar   | 200 ms or less                                             | None: 107 screenshots through a reload all show it                           | The CLI's exit check screenshots the private Sway through a reload  |
| Back after `kill -9`                            | 2 s or less                                                | 1.03 s                                                                       | `crates/crabshell-cli/tests/exit_check.rs`                          |
| Supervisor RSS with the shell up                | No target                                                  | 5.6 MiB                                                                      | `ps -o rss= -p <pid>`                                               |

Memory, wakeup, first-frame and workspace-switch rows measured on 2026-11-22 in release builds on headless Sway, at 0.9.5; the last four rows on 2026-11-18, after Phase 19. The counter's 19.7 MiB is down slightly from 19.9 MiB at 0.9 and up from 19.5 MiB at 0.8.2, mostly the larger `Style` (a gradient holds eight colors, a center and a radius) and a mask region on every widget; it was 18.0 MiB at 0.7, before reading whether a screen reader is on. The memory, wakeup, first-frame and workspace-switch rows come from `measure_the_counter_bar` on one 1920×1080 output at scale 1. The last four rows are the template bar under `crabshell run`, from the CLI's exit check. Phase 0 was the go/no-go gate: the spike used 13.2 MiB against Quickshell 0.3.2's 125.2 MiB for the same bar, with the full data in `spike/RESULTS.md`.

This table holds the latest numbers. A change that moves one updates its row; older runs are in git history.

## Phases

Seven phases to v0.1, then the phases in "Beyond v0.1". A phase is done only when its exit check passes. Phase 0 is a go/no-go gate for the whole project.

1. **Phase 0, spike.** A static text bar on Sway built from one layer-shell surface, one shm buffer, tiny-skia, and cosmic-text with fontconfig loading. Also prove one zbus call runs on calloop's executor.
   - Exit: the bar shows in a headless Sway screenshot and on your real session. RSS and first-frame time are recorded in Performance targets, next to a measured Quickshell baseline.
   - Gate: if RSS isn't clearly below Quickshell's, stop and rethink before Phase 1.
2. **Phase 1, reactive core.** `crabshell-reactive` as specified above.
   - Exit: every required reactive-core test passes.
3. **Phase 2, widgets, layout and rendering.** Every widget except `PopupWindow`, `Menu` and `Tray`, plus styles, damage tracking, fractional scale, pointer input, and `per_output` with hotplug.
   - Exit: an example bar with a counter button matches golden screenshots on headless Sway at scales 1, 1.5 and 2. With no clock, idle wakeups are zero. `swaymsg output HEADLESS-2 unplug` drops that output's window, and the arena's slot count returns to where it started.
4. **Phase 3, clock, workspaces and audio.**
   - Exit: a workspace switch repaints within one frame, per the log. Scrolling changes the volume, and `pactl get-sink-volume @DEFAULT_SINK@` agrees. After a suspend and resume, the clock is right within 1 s. You check that last one by hand.
5. **Phase 4, tray and popups.** Watcher, host, DBusMenu, `PopupWindow`, `Menu` and `Tray`.
   - Exit: tests with a fake tray item pass on a private `dbus-daemon`. By hand, `nm-applet --indicator`, `blueman-applet`, KeePassXC and one Electron app such as Discord all show icons, their menus open, clicks reach the app, and all of them survive a reload.
6. **Phase 5, CLI and reload.** `init`, `run`, `check`, the watcher, `cx.persist`, crash restarts and `last-good`.
   - Exit: a scripted test checks four cases. A syntax error leaves the old shell running and sends a notification. Fixing it brings up the new shell within build time plus 200 ms. After `kill -9` the shell is back within 2 s. A config that panics at startup three times falls back to `last-good`.
7. **Phase 6, the v0.1 release.** Template, README, API docs on docs.rs, published crates, and measured numbers in the performance table.
   - Exit: on a clean user account, you go from `cargo install crabshell-cli` to a running bar in under 15 minutes by following the README.

### Beyond v0.1

After v0.1, the phases bring every feature Quickshell has, so any Quickshell setup can be rebuilt in Crabshell. Phases 7 to 13 follow Quickshell 0.3.2's modules, from its public docs; its source stays unread. Phases 14 to 19 bring what configs take from Qt itself. Every phase keeps the four rules: lighter than Quickshell, idle means asleep, a config is plain Rust, and one bad binding never takes the shell down.

| Phase | Area                             | What it brings                                                                                                                  |
| ----- | -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| 7     | Windows and input                | Floating windows, layer-shell options, input masks, keyboard and text input, scrolling lists, images and clipping               |
| 8     | Animation and effects            | Animations that run only while something moves, gradients, shadows and the compositor's blur                                    |
| 9     | Desktop services                 | Notifications, media players, audio nodes, battery and power profiles, Bluetooth, networking, idle monitor and inhibitor        |
| 10    | Launchers and system integration | Desktop entries, processes, files and sockets, IPC handlers, color quantizing, screencopy views and toplevel management         |
| 11    | Lock screen, greeter and polkit  | Session lock surfaces, PAM, a greetd greeter and a polkit agent                                                                 |
| 12    | More compositors                 | Hyprland's IPC and protocols, i3 outputs and events, `ext-workspace-v1`, global shortcuts and the shortcut inhibitor            |
| 13    | Parity check                     | Every Quickshell 0.3.2 type compared, each gap closed or written down with a reason                                             |
| 14    | Time and motion                  | Timers, delays and loops, more easing curves, springs, a per-frame signal, exit transitions, and translate, scale and rotate    |
| 15    | Layout, pointer and views        | Flex and grid layout, `visible` and `enabled`, drag, long press, cursors, a slider, kinetic scrolling, scroll bars, grid views  |
| 16    | Drawing and effects              | A canvas, sweep gradients, per-corner radii, image filters, masks, content shadows, GIFs and wallpaper transitions              |
| 17    | Text                             | Text styles, spans with notification markup and links, measuring, bundled fonts, selection and a multi-line editor              |
| 18    | System and outside data          | HTTP and cover art, folder views, the clipboard, logind, brightness, system stats, niri's IPC, icon fallbacks, locale and sound |
| 19    | Parity remainder                 | Native PipeWire's link graph, `NMSettings` write and the Wi-Fi scanner, a screen reader, reload without the blink and on demand |

### Phases 14 to 19: what configs take from Qt

Phase 13 matched Quickshell's own types, but most of what a Quickshell config draws and animates comes from Qt. On 2026-11-15 a survey counted what five public configs use: caelestia's shell, end-4's illogical-impulse, noctalia-shell 4.7.7, DankMaterialShell and the maintainer's rime, read from their QML and Qt's and Quickshell's public docs. Phases 14 to 19 bring what they use, most used first. A timer, a loop or a kinetic scroll is in the event loop only while it runs and gets an idle test, and every new callback runs inside `catch_unwind`.

Phases 14 to 19 are built; see Writing a config. With Phase 19's PipeWire link types, every Quickshell 0.3.2 type has a Crabshell counterpart.

Left out, with the reason:

- User GLSL shaders (`ShaderEffect`, which caelestia and DankMaterialShell use mostly for wallpaper transitions). A CPU renderer can't run them, and the later vello backend wouldn't take them either. Phase 16's built-in transitions cover the common case.
- Video (`MediaPlayer`, `VideoOutput`). It needs ffmpeg or GStreamer, both LGPL C libraries, CPU color conversion each frame, and RSS far past 30 MB. mpvpaper plays video wallpapers.
- Perspective transforms (`Matrix4x4`), since the renderer is 2D affine.
- Elastic and Bounce curves, `PathAnimation`, `AnimationController`, `PinchHandler`, `PathView`, `SwipeView` and `TableView`, which one of the five configs uses at most.
- What a Rust config already has. `States` and `PropertyChanges` are an enum and a `match`. `Qt.callLater` is the coalescing each flush does. `ListModel`, `DelegateModel` and `ObjectModel` are a signal holding a `Vec`. `layer.enabled` is what opacity and clipping already do. `WebSocket`, `Qt.labs.settings`, `qsTr`, `Qt.md5` and `StandardPaths` are crates and std. `Qt.openUrlExternally` is `xdg-open` through `exec_detached`. Controls' `Switch`, `CheckBox`, `ComboBox`, `TabBar` and `StackView` are a `Button`, `PopupWindow`, `Menu` or `children_keyed` with styles.
- Variable-font axes other than weight, such as Material Symbols' fill, since cosmic-text 0.19 sets only the weight axis.

### Still open

The hand checks are tracked in [issue #2](https://github.com/w0x7y/crabshell/issues/2) and the upstream reports, with their drafts, in [issue #3](https://github.com/w0x7y/crabshell/issues/3).

- Hand checks that need the desktop: the greeter example against a real greetd on a real VT, peak meters on a real PulseAudio rather than pipewire-pulse, and Phase 6's clean user account that installs Rust through rustup first.
- Phase 18's and 19's hand checks passed on 2026-11-19, except niri, which isn't installed here, and a camera left out of `screencasts()`, since this laptop's camera isn't in PipeWire.
- Phase 18 limits: a temperature sensor that appears after the first sample is listed only once a listed one goes; Russian and Polish `%B` names are the in-date form; and a clipboard image file outlives a crash until the next shell's first `history()`.
- Phase 19 limits: a PipeWire node's description that changes later isn't seen, and a screen reader can't edit a field or find a character's place on screen.
- nonstick 0.1.2 frees its own copy of a PAM answer without zeroing it, and no `pam_setcred` runs, so Kerberos tickets aren't refreshed on unlock. Crabshell's crates forbid the `unsafe` a fix of their own would need, so both wait on nonstick.
- A config-local PAM directory, as Quickshell's `PamContext.configDirectory`, waits on nonstick too: it has no `pam_start_confdir`, and the crates forbid the `unsafe` a call of their own would need. A lock screen's own PAM stack goes in `/etc/pam.d`.
- An animated full-screen mask region costs about 22 to 25 ms a frame at scale 2, most of it in the tiles a hole's edge crosses.
- Proposed in 0.9 but not built: an API that sizes a panel from its content without a `geometry` signal, and a plain `Transition::delay` in `cx.animate` that sleeps instead of ticking frames, as a sequence's hold does.
- 0.9 limits: `Tray` has no `mask_region`, since it isn't built from the widgets' shared methods; a popup made in the turn an image's source changes loads that image at one pixel before its first configure, a wasted decode for a large file; and a status told by the last draw of a loop turn runs its effects before the loop sleeps, but a window those effects change waits for its next wakeup to draw.
- cosmic-text 0.19 keeps the leading space on an elided wrapped line. Crabshell works around it until an upgrade fixes it upstream.
- ttf-parser 0.25.1, which cosmic-text 0.19 pulls in through fontdb 0.23, is marked unmaintained (RUSTSEC-2026-0192). cosmic-text's main branch already moved to fontdb 0.24, which drops it, so the advisory is ignored in `deny.toml` until a cosmic-text release does.
- Settled limits: window views pair windows by app id and title, the most the protocols give; neither `geometry` nor `mapped_geometry` can say where a window is on its output, which Wayland doesn't tell a client; Hyprland 0.42 and later are supported; and the dashboard's 38.6 MiB with a 1080p wallpaper is no breach of rule 1, whose cap is for the counter bar, since a 1080p picture and its window buffer take about 16 MiB.
- Crabshell has no wrapper widgets, since a `Stack` or `Button` with a padded style covers `WrapperItem`, `WrapperRectangle` and `WrapperMouseArea`, and `Style::clip` covers `ClippingRectangle`.

### Breaking changes in 0.9

The gaps found porting a Quickshell config shipped as 0.9. Against 0.8:

- New public fields break struct literals and exhaustive patterns: `TextMetrics::ink`, `DesktopEntry::command`, `CaptureFrame::logical_width` and `logical_height`, `CaptureView::crop`, and `InputView::selection_color` for a renderer of one's own.
- A panel is laid out once before its first configure, and keeps each side it sent as non-zero through any configure; only a side sent as 0 takes the compositor's size.
- `max_length` writes a cut value back to its signal.
- `CaptureRequest::size` is infinite for a view with a crop, which asks for the source's whole frame.

0.9.1 fixes a layout bug: an absolute child with `right` or `bottom`, or two opposite insets, inside a parent smaller than the window now sits in that parent's padding box, where 0.9 and earlier measured from the window.

0.9.4 adds a public `decode_size` field to `ImageFilters`, which breaks struct literals that don't use `..ImageFilters::default()`.

0.9.5 fixes a layout crash in taffy 0.15.1's rounding pass when a container becomes empty or hidden after holding absolute children by rounding layouts in place in `place`, and speeds up layered and masked animations (kept layers from the first moving frame, large buffer reuse within a draw, straight-edge mask skipping, and word and byte blending).

## Rules for AI agents

They live in [AGENTS.md](../AGENTS.md), with the commands that test a change.

## Risks and open questions

The biggest risk is the reload loop. A rebuild per edit, 1.0 s for a one-line change, is the price of compiled configs, still slower than Quickshell's QML reload. The second is the size of the goal: Quickshell exposes about 240 QML types, so full parity is many phases past v0.1.

| Risk                                      | Why it matters                                                              | Mitigation                                                                                            |
| ----------------------------------------- | --------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| Reload misses the goal                    | A rebuild per edit, against Quickshell's QML reload                         | Fast dev profile, a small config crate, build time measured in Phase 5. Phase 19 removes the blink.   |
| You review Rust you can't fully judge yet | Bugs in the reactive core or event loop hide well                           | Small changes, required tests, headless Sway screenshots, and plain-language explanations from agents |
| Tray apps bend the spec                   | Electron, Qt and Ayatana apps each implement StatusNotifierItem differently | Phase 4 gets 4-6 weeks and tests real apps                                                            |
| Text looks worse than Qt's                | v0.1 has grayscale antialiasing only                                        | Compared side by side in Phase 0                                                                      |
| Font loading slows startup                | Scanning every system font can blow the 100 ms target                       | Load only the files fontconfig matches                                                                |
| Sway-first shrinks the audience           | Most Quickshell users run Hyprland or niri                                  | Hyprland came in Phase 12, niri's IPC in Phase 18                                                     |
| Configs need a Rust toolchain             | A higher entry bar and a slow first build                                   | Template and README, with the full first run timed in Phase 6                                         |
| No accessibility in v0.1                  | Screen readers can't read the shell                                         | Add AccessKit (AT-SPI) in Phase 19                                                                    |

Open questions:

- [x] Project name: Crabshell.
- [x] Which other wlroots compositors to test: Sway (SwayFX included) and labwc; river and Wayfire go untested and are reached through the same protocols.
- [x] Should a reload start the new shell before stopping the old one, to remove the blink? Yes; Phase 19 builds it.
- [x] Stay on libpulse or switch to native PipeWire? libpulse stays for volume, sinks and meters; Phase 19 adds native PipeWire only for the link graph and video nodes.
- [x] When to add Hyprland and niri: Hyprland came in Phase 12, and niri's IPC in Phase 18.
- [x] Should the shell exit with 1 when `f` panics for every output at startup? Yes, so the supervisor falls back to `last-good`.
- [x] Install cargo-deny and add a deny.toml: done, and CI runs it.

## Sources

- [Quickshell source](https://github.com/quickshell-mirror/quickshell) at commit `9b0f339` from 2026-10-21, counted locally. About 62k lines of C++ exposing 241 QML types, licensed LGPL-3.0.
- [Quickshell 0.3.2's type list](https://quickshell.org/docs/v0.3.2/types), from its public docs, read on 2026-11-13 for "Beyond v0.1".
- Everything else comes from memory and wasn't checked against current docs: crate names and APIs, protocol versions, Sway commands and toolchain behavior. Pinned versions are in the repo's Cargo.toml.
