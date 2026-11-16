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
| C libraries        | Fine when the system ships them            | Links libpulse, fontconfig, libxkbcommon and libpam at runtime. Wayland itself is pure Rust.                                                  |
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
crabshell = "0.5"

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
- Event handlers (`on_click`, `on_right_click`, `on_middle_click`, `on_scroll`, and since Phase 15 `on_release`, `on_double_click`, `on_long_press`, `on_drag`, `on_wheel`, `on_back`, `on_forward` and `on_drop`) receive `&Cx` and run on the main thread.
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

API rules settled in Phase 10, launchers and system integration:

- `Button::hovered(signal)` writes whether the pointer is over the button or anything inside it, inner buttons included, so an outer button can show controls on hover; `hover_style` still shows only on the deepest button. Buttons may share one signal, which is `true` while any of them has the pointer. `Shadow::cut_out()` leaves a shadow out under its box, as CSS always does, so translucent glass shows what is behind it.
- `cx.process(|| command)` runs a program and gives a `Process`: `running()`, `exit_status()`, `stdout()` and `stderr()` signals, `on_line` for each line it prints, `with_stdin` with `write` and `close_stdin`, `kill` and `restart`. `command` returns a `std::process::Command` or a list such as `["uptime", "-p"]` (`IntoCommand`), and runs again when a signal it read changes. `cx.exec_detached(command)` starts a program that runs on its own, as a launcher does.
- `cx.socket(path)` connects to a Unix socket: `connected()`, `on_line`, `write`, `reconnect` and `close`. `cx.file_view(path)` gives a `FileView` whose `text()` follows the file and whose `write` replaces it. `cx.json_file(path, default)` gives a `Signal<T>` kept in step with a JSON file both ways.
- `cx.desktop_entries()` gives the installed applications as `DesktopEntry`s with their `DesktopAction`s, `lookup(app_id)` for a window's entry, `launch(id)` and `launch_action(id, action)`.
- `cx.ipc(target, function, handler)` comes from `ShellExt`, with a handler `Fn(&[String]) -> Result<String, String>`. `crabshell ipc <target> <function> [args...]` calls it and prints its `Ok` text or reports its `Err`; `crabshell ipc` lists the handlers.
- `cx.quantize_colors(path, depth)` comes from `ShellExt` and gives a `ReadSignal<Vec<Color>>` of up to 2^`depth` main colors of an image, found off the loop's thread; `path` takes a value or a closure.
- `cx.toplevels()` comes from `ShellExt` and gives `Toplevels`, a `Copy` handle: `list()` is a `ReadSignal<Vec<Toplevel>>` of every program's windows, and `activate`, `close`, `set_maximized`, `set_minimized` and `set_fullscreen` take a window's `id`.
- `ScreencopyView::new(source)` shows a `CaptureSource`, an output by name or a window by its `Toplevel` id, once or `live`, sized by its style and drawn with a `fit`; every property takes a value or a closure.
- The prelude adds `untrack`, `Process`, `IntoCommand`, `Socket`, `FileView`, `DesktopEntries`, `DesktopEntry`, `DesktopAction`, `Toplevels`, `Toplevel`, `ScreencopyView` and `CaptureSource`. The overview example shows them together.

API rules settled in Phase 11, lock screen, greeter and polkit:

- `cx.session_lock(SessionLock::new().locked(...).style(...).surface(|cx, output| ...))` comes from `ShellExt` and gives a `ReadSignal<bool>`, `true` while the compositor says the session is locked. Every property takes a value or a closure. Turning `locked` false is the only unlock, and the signal turns false once the compositor has it, so a config may exit then. If the lock's context goes or the shell exits or crashes, the session stays locked. So keep `locked` in a `cx.json_file` under `$XDG_RUNTIME_DIR`: the shell after a reload or a crash locks again, and a reboot starts unlocked. `cx.persist` covers a reload only.
- `cx.pam(service)` gives a `Pam` for a file in `/etc/pam.d`: `start(user)`, `None` for the shell's own user, `respond(text)` and `abort()`, with `state()` and `messages()` signals. Take the password first, start on Enter and answer as soon as PAM asks: an attempt ended while PAM waits for an answer counts as a failed login where `pam_faillock` counts them.
- `cx.greetd()` gives `Greetd` for a greeter: `create_session(user)`, `respond(answer)`, `start_session(command, env)` and `cancel()`, with `state()` and `messages()` signals. `cx.exit(code)` comes from `ShellExt` and ends the shell with that status, as a greeter does once its session starts.
- `cx.polkit_agent()` makes the shell the session's polkit agent and gives a `PolkitAgent`: `request()`, a `ReadSignal<Option<AuthRequest>>`, and `respond(text)`, `cancel()` and `pick_identity(name)`.
- The prelude adds `SessionLock`, `Pam`, `PamState`, `PamResult`, `PamMessage`, `Greetd`, `GreetState`, `GreetMessage`, `PolkitAgent`, `AuthRequest` and `AuthState`. The lock example shows the lock with PAM, and the greeter example greetd.

API rules settled in Phase 12, more compositors:

- `cx.sway()` also gives `outputs()`, a `ReadSignal<Vec<SwayOutput>>` with each output's name, focus, whether it is active, its workspace, position, size and scale, as Quickshell's `I3Monitor`; `dispatch(command)`, which runs any Sway command as `swaymsg` does; and `on_event(kinds, handler)`, which calls `handler` with each `SwayEvent`, its `SwayEventKind` and Sway's JSON, of the kinds given while the asking context lives, as Quickshell's `I3IpcListener`.
- `cx.window_manager()` comes from `ShellExt` and gives `WindowManager`, a `Copy` handle over `ext-workspace-v1` with Quickshell's names: `windowsets()` and `projections()` are the workspaces and the groups they show in, with their outputs, and `windowsets_on(output)` is what a bar on that output lists, as `ScreenProjection`. `activate`, `deactivate`, `remove`, `assign(id, projection)` and `create_windowset(projection, name)` take ids, and a windowset's `can_` fields say which the compositor takes.
- `PanelWindow::inhibit_shortcuts` and `FloatingWindow::inhibit_shortcuts` take a value or a closure: while true and the window has the keyboard, the compositor's own shortcuts reach the window as keys. `shortcuts_inhibited(signal)` has the compositor write whether it honors that.
- `cx.global_shortcut(app_id, name, description)` comes from `ShellExt` and gives a `ReadSignal<bool>`, true while the compositor's binding for it is held, as Hyprland's `bind = SUPER, D, global, crabshell:launcher`. Calls with the same app id and name share one registration.
- `focus_grab(signal)` on a `PanelWindow` or `FloatingWindow` takes a `Signal<bool>`: while it is true, the windows given that signal, with their popups, hold the keyboard and pointer, and a click elsewhere has the compositor clear the grab and write `false`. Writing `false` ends it.
- `cx.hyprland()` comes from `ServicesExt` and gives `Hyprland`, a cheap clone. `workspaces()`, `monitors()` and `toplevels()` are `ReadSignal`s of `HyprlandWorkspace`, `HyprlandMonitor` and `HyprlandToplevel`, which name each other by name, as `Workspace::output` does. `focus_workspace(target)` switches to `2` or `name:web` in the syntax Hyprland reads: Lua, with the name quoted as a Lua string, whenever its config is Lua, as `hyprland.lua` can be from 0.55 and always is from 0.57; otherwise `workspace X`, where a name with whitespace or a control character is logged and not sent. `dispatch(command)` sends a raw dispatch: whenever Hyprland's config is Lua, the command is a Lua expression run inside the compositor, so never put window titles, classes or workspace names into it. `on_event(handler)` gets every raw `HyprlandEvent` until the asking context goes, and `parse(n)` splits its data into at most n fields.
- `on_event` on a Hyprland or Sway handle whose context went logs that and does nothing.
- The prelude adds `Sway`, `SwayOutput`, `SwayEvent`, `SwayEventKind`, `WindowManager`, `Windowset`, `WindowsetProjection`, `Hyprland`, `HyprlandWorkspace`, `HyprlandMonitor`, `HyprlandToplevel` and `HyprlandEvent`. The hyprland example shows a bar on Hyprland.

API rules settled in Phase 13, parity:

- `cx.outputs()` comes from `ShellExt` and gives a `ReadSignal<Vec<OutputInfo>>` of every output, as Quickshell's `screens` and `ShellScreen`: its name, description, make, model, mode, whole `scale` and the `fractional_scale` windows draw at, logical `x`, `y`, `width` and `height`, `width_mm` and `height_mm`, and `transform`. An output is listed before `per_output` builds for it, and the list follows every change the compositor tells. `crabshell::Turn` and `crabshell::Mode` name the transform's and mode's types.
- `Style::padding_top`, `padding_right`, `padding_bottom` and `padding_left` set one side each and keep the others; `Padding` has `top`, `right`, `bottom` and `left`. A row's or column's own `.padding(v, h)` still wins on both sides of its axis, and `merge` takes the top style's whole padding.
- `Adjust` adds `RESIZE_X`, `RESIZE_Y` and `RESIZE`, and `ALL` includes them, as Quickshell's `All` does; in 0.2 `ALL` was flip and slide only. The default still flips and slides. A popup the compositor resizes is drawn at the size it gives.
- `FloatingWindow::mask` and `PopupWindow::mask` take a value or a closure, as `PanelWindow::mask` does. A popup a click opened holds the pointer, so a click outside its mask reaches the shell's own window below, but over another program it closes the popup.
- `compositor_opacity` and `visible_mask` on `PanelWindow`, `FloatingWindow` and `PopupWindow` take a value or a closure, as Quickshell's `HyprlandWindow`. The first is the opacity Hyprland shows the whole window at, blur included, unlike a style's `opacity`, which Crabshell paints. The second is a hint naming the part that holds anything to see, and must cover every pixel that isn't fully transparent.
- `TrayItem::category` is a `TrayCategory`. `cx.power_profiles().degradation()` gives a `PerformanceDegradation`: `None`, `LapDetected`, `HighTemperature`, or `Other` for a reason the daemon adds later. `BluetoothAdapter::state` and `BluetoothDevice::state` give the power and connection state beside `powered` and `connected`.
- `cx.notifications().on_closed(|notification, reason| ...)` hears each notification that leaves the history and its `CloseReason`, while the asking context lives.
- `PamResult::Error { kind, message }`, where `kind` is a `PamError`: `StartFailed`, `TryAuthFailed` or `InternalError`.
- `cx.network()` adds `connectivity()`, `NetworkState::Disconnecting`, each device's `fail_reason`, `wifi_mode`, `has_link` and `link_speed`, each Wi-Fi network's `security`, from which `secured` follows, and `saved_connections()`, each with `uuid`, `id`, `kind` and its `settings` as a `serde_json::Value` without secrets. `forget(uuid)` and `clear_secrets(uuid)` act on one. Read the list with `with`, since each `get` clones it.
- `Process` and `Socket` take `on_split(marker, handler)`; `on_line` is `on_split("\n", ...)`, and `""` hands over each read. A handler that sets another marker has the rest split at it. `FileView::error()` gives `Option<FileError>` with a `FileErrorKind` (`NotFound`, `PermissionDenied`, `NotAFile` or `Other`) and the message, cleared by a read or a write that works. `cx.socket_server(path)` gives a `SocketServer`: `on_connection(|cx, socket| ...)` gets each client as a `Socket` with a context of its own, and `close()` stops it.
- `AudioNode` and `AudioStream` add `volumes` and `channels`, each channel's volume and `AudioChannel` position, set with `set_sink_volumes`, `set_source_volumes` and `set_stream_volumes`. `cx.audio().peaks(target)` gives a `ReadSignal<Vec<f32>>` meter for a `PeakTarget`, a value or a closure, as Quickshell's `PwNodePeakMonitor`. A meter wakes the shell 25 times a second while its target plays, so build one only while it shows.
- The prelude adds `OutputInfo`, `TrayCategory`, `PerformanceDegradation`, `BluetoothAdapterState`, `BluetoothDeviceState`, `CloseReason`, `PamError`, `Connectivity`, `ConnectionFailReason`, `WifiMode`, `WifiSecurity`, `SavedConnection`, `FileError`, `FileErrorKind`, `SocketServer`, `AudioChannel` and `PeakTarget`. Several of these, and the new fields and variants, change published types, so they ship as 0.3.

API rules settled in Phase 14, time and motion:

- `cx.after(delay, f)` runs `f` once after `delay`, and `cx.interval(period, f)` every `period`, the first time one period in. Each gives a `Timer`, a `Copy` handle: `stop()`, `restart()`, which counts again from now and runs a one-shot that fired again, and `running()`, a signal a binding can read. A timer runs from the moment it is made, so one that waits for a first change is stopped on the next line. It goes with the asking context.
- `Easing` adds Sine, Quart, Quint, Expo, Circ and Back, each `In`, `Out` and `InOut`, and `Spline(&[[c1x, c1y, c2x, c2y, x, y], ...])`, Qt's `BezierSpline`, for curves of several segments such as Material 3's emphasized one.
- `Transition::delay(ms)` waits before it moves, style fades included, and `Transition::alternate()` makes a cycle come back.
- `cx.animation(target, transition)` gives an `Animation`, a `Copy` handle with `value()` and `running()`, which is true from a new target, through its delay, until it settles. An effect on `running()` is where whatever follows an animation goes, as Qt's `onFinished`. `cx.animate` is its `value()`.
- `cx.cycle(running, transition)` gives eased progress from 0 to 1 that repeats while `running` is true, for spinners and pulses. `cx.spring(target, Spring::new(stiffness, damping))` follows an `f32` on a spring that keeps its speed when the target moves; the default is 700 and 0.9. `cx.frames(running)` gives the time since `running` turned true, updated each frame, as Qt's `FrameAnimation`.
- `Style::translate(x, y)`, `scale(s)`, `rotate(degrees)` and `origin(x, y)` are `const`. They move a widget and everything inside it where it is drawn and hit, never the layout, and they fade with the `transition`. Popups and anything else anchored to a widget keep its laid-out rectangle.
- `.exit(duration)` after `children_keyed` takes a value or a closure, read as each key leaves. The removed child stays for that long while `cx.leaving()`, a `ReadSignal<bool>`, is true, so its own `cx.animate` can move it out, and an exit of 0 removes it at once. A leaving child still takes input, so a handler can check `cx.leaving()`. To move a new child in, set a signal after making its `cx.animate`; the next flush starts the move.
- The prelude adds `Timer`, `Animation`, `Spring` and `std::time::Duration`. The motion example shows them together. `Transition` has two new public fields, `delay` and `alternate`, and `Easing` new variants, so a 0.3 config that builds a `Transition` from its fields or matches every `Easing` no longer builds, and they ship as 0.4.

API rules settled in Phase 15, layout, pointer and views:

- `Style` gets `grow`, `shrink`, `basis`, `min_width`, `min_height`, `max_width`, `max_height`, `align_self`, `justify_self`, `left`, `top`, `right`, `bottom`, `z` and `span(columns, rows)`, all `const`. Each merges per field, and a number that isn't finite counts as unset. All but `z` change the layout at once; a `z` change repaints.
- Any of `left`, `top`, `right` and `bottom` takes a widget out of its parent's flow, as CSS's `position: absolute`: it is placed from those edges of the parent's padding box and no longer sizes the parent. `align_self` places a child across a row or column, or vertically in a stack's or grid's cell, and `justify_self` horizontally in a cell; rows and columns ignore `justify_self`. `z` orders siblings for drawing and for the pointer, lowest first, keeping child order for equal ones; Tab keeps child order.
- `Grid::new()` places children row by row into `columns` equal columns, 1 by default, with rows as tall as their content, and takes `gap`, `row_gap`, `column_gap`, `padding(v, h)`, `child`, `children_keyed`, `exit` and `style`. `Row` and `Column` take `wrap`, which starts a new line for children that don't fit, `gap` apart, and `reverse`, which runs a row right to left and a column bottom to top, so `Justify::Start` packs at the right or the bottom.
- Every widget, `Tray` included, takes `visible(...)` and `geometry(signal)`. Hidden, a widget frees its space, isn't drawn, hit or reached by Tab, drops the focus, unhovers and lets go of a held press, and keeps its bindings and state; a field inside it can't take the focus, and when it shows again the first field in Tab order whose `focus` is still true takes it. After each layout that moves or resizes a widget, its laid-out rectangle is written to its `geometry` signal, in window logical pixels, with scroll offsets but not transforms; a hidden widget writes nothing. A window laid out from another's geometry catches up in the same loop iteration.
- `Button` gets `on_release`, which runs when the left button comes up after going down on it, wherever the pointer is, before anything else the release runs; `on_double_click`, which replaces the second `on_click` when two clicks on the same button come within 400 ms and 5 logical pixels, timed by the compositor; and `on_long_press`, which runs once the left button has been held 800 ms without moving 10 logical pixels, after which the release doesn't click.
- `on_drag(|cx, drag|)` gets a `Drag`: its `DragPhase` (`Start`, `Move`, `End`), the `start` and `position` in the button's own coordinates through its transforms, its laid-out `size`, and at `End` whether it was `dropped`. A drag starts when a held left button moves more than 10 logical pixels, Qt's `startDragDistance`, or at the press for a button with no `on_click`, `on_double_click` or `on_long_press`, so a slider moves from the first touch. While it drags, the button gets every motion inside the window or out, it and the buttons around it stay hovered, and it doesn't click. It ends with `End` on release, on leaving the window, or when the button is turned off; a button removed or a window dropped mid-drag runs nothing more.
- `pointer_position(signal)` writes the pointer's place in the button's own coordinates while the pointer is over it or inside it, following a drag of it or of a button inside it anywhere, and `None` when it leaves, only on a change.
- `on_wheel(|cx, wheel|)` gets every scroll event on both axes as a `Wheel`: per axis a `WheelAxis` with `pixels`, `notches` (`axis_value120` over 120) and `stop`, and the `ScrollSource` (`Wheel`, `Finger`, `Continuous` or `WheelTilt`). Positive is down and right, unlike `on_scroll`, which keeps whole vertical steps with up positive. With both, both run, and either takes the scroll from a scroll view. `on_back` and `on_forward` run on a click of the mouse's side buttons.
- `cursor(...)` takes any `CursorShape`, every `wp_cursor_shape_device_v1` shape. A text field keeps the I-beam, and a disabled button shows what is around it.
- `enabled(...)`: off, a button runs no handler and shows no hover, pressed or focus style, Tab skips it, and the pointer passes through it to the button around it, as through a disabled Qt `MouseArea`. It isn't inherited. Turning it off while held lets go, ending a drag with `End`, and drops the focus. `focus(...)` takes the keyboard focus whenever it turns true, unless the button is disabled or hidden.
- A touchscreen's first finger is a left-button pointer, so a tap clicks and a moving finger drags.
- `.displaced(transition)` after `children_keyed`, beside `.exit`, takes a value or a closure, read untracked at each change of the list. When its keys are added, leave once their exit ends, or move, each child of the container whose place in it changes slides from where it was drawn to its new place, through a translate that eases to nothing, so it is hit where it is drawn and the window isn't laid out each frame. A new child shows in its place, a transition of 0 jumps, and other layout changes, such as a resize, still move children at once.
- `Button::drag_data(data)` takes a value or a closure of `Option<T>` for any `T: Clone + 'static`. While it holds a value, the button drags as one with `on_drag` does; while it holds `None`, it clicks, and drags only with its own `on_drag`, so `editing.get().then_some(id)` drags only in an edit mode. `Button::on_drop(|cx, value: T| ...)` takes drops of that type only, and `Button::drop_hovered(signal)` is written `true` while a drag carrying a value it takes is over it, as Qt's `DropArea.containsDrag`. Releasing over it runs `on_drop` with a copy of the value; releasing anywhere else drops nothing. `Drag::dropped` tells the source at `End`, and is true only when the target's `on_drop` ran and returned. A drag reaches only buttons in its own window.
- `Slider::new(value)` takes the `Signal<f32>` it writes back, clamped to `range(min, max)` (0 to 1 by default) and snapped to `step` (0, continuous, by default), both values or closures, and only on a change. A value set from outside moves the handle, past either end it shows at that end, and NaN shows at the start. A press moves the value there and drags with no threshold. The wheel moves a step a notch, or a twentieth of the range without a step, a touchpad one step per 15 pixels, and Left/Down and Right/Up do the same while it has the focus. `style` is the track (120 by 4 unless it sets a size), `fill_style` runs from the start to the handle's middle, and `handle_style` is the handle (16 by 16 unless it sets a size). `vertical` runs bottom to top, and `dragging(signal)` is Qt's `pressed`; it turns false when the slider goes while held.
- `ScrollView::horizontal` lays the child out at full width and scrolls along x, taking sideways wheel and touchpad input and, with no sideways input, a turned wheel. The offset is then the x offset.
- `ScrollView::kinetic` and `overshoot`, both on by default and also on `ListView` and `GridView`: a touchpad fling, or a drag of the child with the left button or a finger, keeps moving at most 2500 px/s, slowing by 1500 px/s², and a touchpad, drag or fling goes past either end against a pull, within a quarter of the box, then springs back. The offset signal stays within the content. A wheel notch still moves 48 px at once.
- `ScrollView::scroll_bar(style)`, also on the lists, takes a value or a closure of `Option<Style>` and draws a handle inside the border along the right or bottom edge while the child is longer than the box, cut to the view's rounded corners. Its `background` and `radius` draw it and its `width` (6 by default) is its thickness. Dragging it scrolls and a press beside it pages; it takes the press over the view's own content, but not over a button drawn above the view from outside it.
- `ListView` rows may differ in height. Each is at least `row_height` tall and grows to fit its content, and its height, measured after layout, is kept by key; a row not built yet counts as `row_height`. `ListView::horizontal(...)` runs the rows left to right, and `row_height` and `height` then mean widths. A `current` past the end is scrolled to once the items reach it.
- `GridView::new(items, key, view)` lays out cells of `cell_size(width, height)` in as many columns as fit, at least one, building only the rows in view plus one each side, and takes `height`, `current`, `offset`, `style`, `scroll_bar`, `kinetic` and `overshoot` as `ListView` does.
- `.highlight(style)` on `ListView` and `GridView` takes a value or a closure of `Option<Style>` and draws a box behind the `current` item. It moves to a new current item over the style's `transition`, takes its new size at once, and is drawn moved, so a move doesn't lay the window out. The default is none. While a list or grid moves, a change of `current` scrolls nothing and only the highlight follows it, so `current` can follow the hovered row.
- Every `Option<T>` passes as a plain value, so `.drag_data(Some(id))` works, and so does `Transition`.
- `Window::pointer_motion_at`, `pointer_button_at` and `pointer_scroll_at` take the compositor's event times, which time double clicks and flings. `Window::pointer_scroll` takes a `Wheel`; `ScrollDelta` and `Window::pointer_scroll_end` are gone.
- The control example shows the Phase 15 views together: a `Slider` on the default sink, chips in a wrapping `Row` with one turned off by `enabled(false)` and dimmed by its own style, since a disabled button shows no style of its own, a month in a `Grid` with the days before the 1st in one spanned cell, and a `ListView` of notifications whose `current` follows each row's `hovered`, so its `highlight` slides to the row under the pointer. At rest the list keeps its current row in view, so hovering a row cut off at the bottom scrolls it whole, as Qt's does; the pointer stays on that row, so nothing scrolls further.
- To bind a slider to an outside value, write the outside value into the slider's signal only while `dragging` is false, and send the signal's value out only when it differs from the outside one, so neither side loops and a held handle doesn't jump back to a late reply. A date shown without a clock waits for the next midnight with `cx.after`, re-armed each day in a fresh child context, so it wakes once a day instead of every minute.
- The prelude adds `Grid`, `Rect`, `Point`, `Drag`, `DragPhase`, `Wheel`, `WheelAxis`, `ScrollSource`, `CursorShape`, `Slider` and `GridView`. `Spacer` is no longer a unit struct, `Style` and `Drag` have new public fields, `PointerButton` and `CursorShape` have new variants, and `ScrollDelta` is gone, so Phase 15 ships as 0.5.

API rules settled in Phase 16, drawing and effects:

- `Canvas::new(|pen| ...)` takes a paint closure that runs in an effect, inside `catch_unwind` with no borrow held, once the canvas is first laid out, again when its laid-out size changes, and again when a signal it read changes. Each run replaces the drawing and repaints only the canvas's box, never the layout, and a drawing equal to the last repaints nothing. `pen.size()` is the canvas's logical size, coordinates are logical pixels from its top-left, and the drawing is cut to its box and corners. A canvas takes `style`, `visible` and `geometry` like every widget. A panic is logged as `<path>/Canvas.paint` and disables the closure, as any binding's, so the last drawing stays, even through a resize.
- `pen.fill(&path, paint)` fills by the nonzero rule and `pen.stroke(&path, &stroke)` draws along the path; a paint is a `Color` or a `Gradient` laid over the canvas's box, as a style's is. `Path::new()` takes `move_to`, `line_to`, `quad_to`, `cubic_to`, `arc(center, radius, start, sweep)` and `close`, all with `Point`s; arc angles are in degrees, clockwise from 3 o'clock as an HTML canvas's arc goes, a negative sweep runs counterclockwise, and a sweep is at most a whole turn. `Path::rect`, `rounded_rect`, `circle` and `ellipse` build whole shapes. `Stroke::new(width, paint)` takes `cap(Cap::Butt | Round | Square)`, `join(Join::Miter | Round | Bevel)` and `dash(pattern, offset)`; an odd dash pattern repeats, as `setLineDash` does, and a width of 0 draws nothing. Paths and strokes are plain data, so a config can build them outside the closure.
- `Style::radii(top_left, top_right, bottom_right, bottom_left)` rounds each corner on its own, and `radius(r)` sets all four; the field holds a `Radii`, with `Radii::all(r)` and `corners()`. Each corner is clamped to half the box's shorter side, as Qt's `Rectangle` does, so one large corner beside small ones stays smaller than CSS would draw it. The background, gradient, border, a clip with its children and the pointer, a shadow, the scroll bar's cut, a canvas and a style fade all follow each corner.
- `Gradient::sweep(angle, from, to)` runs once around the middle, clockwise from `angle` degrees with 0 at 12 o'clock, as CSS's `conic-gradient(from …)`, takes `stop` as the other gradients do, and fades into another sweep with as many colors. `GradientShape` adds `Sweep(angle)`.
- `Style::border_dash(on, off)` dashes the border along its middle line, in logical pixels, stretched or squeezed so a whole number of dashes goes around, with one dash centered on the end of the top-left corner. An `on` of 0 draws round dots `off` apart, middle to middle. The dash is a field of its own, merged on its own, so a hover style can dash a border the style under it sets; with no border it draws nothing, and a fade takes the end style's dash at once.
- `Color::hsl(h, s, l)`, `hsla(h, s, l, a)`, `with_alpha(a)`, `mix(other, t)`, `lighter(amount)` and `darker(amount)` are `const fn`s. Hue is in degrees and the rest run from 0 to 1; `mix` blends each sRGB channel and the alpha, and `lighter` and `darker` add to the HSL lightness, as Sass's `lighten` does.
- `Icon::tint` and `Image::tint` take a value or a closure of `Option<Color>` and paint every pixel in that color, keeping its alpha times the tint's; `None`, the default, draws the picture as it is, and a `Color` passes as `Some`.
- `Image::brightness`, `contrast` and `saturation` follow CSS's `brightness()`, `contrast()` and `saturate()`, where 1 leaves the picture as it is, and `blur` is a Gaussian's standard deviation in logical pixels, as CSS's `filter: blur()`, 0 by default; a `Shadow`'s blur is CSS's `box-shadow` blur, twice its deviation, so the same number blurs an image twice as much. A number that isn't finite counts as unchanged and one below 0 as 0. They apply once, in the order tint, brightness, contrast, saturation, blur, to the picture scaled to its box, and a change repaints without a layout.
- An `Image` of a GIF with more than one frame plays while it is drawn, at its own delays, and loops as its file says; a delay under 20 ms counts as 100 ms. `Image::playing`, true by default, pauses it on its frame. A GIF with a `blur` shows its first frame, still, since the blur is for still pictures such as album art and wallpapers.
- `Style::mask(Mask)` is `const` and a paint property. `Mask::ellipse()`, `Mask::gradient(gradient)` and `Mask::image(path)` keep the widget and everything inside it only where the mask is opaque, as Qt's `OpacityMask`: the mask's alpha multiplies theirs, and nothing shows outside the widget's box. A gradient mask uses only its colors' alpha. `Mask::image` takes a `&'static str`, so a style stays `const`; a path made at run time is leaked once, outside the style's closure. The file is read through `Image`'s loaders and caps, and one that is missing or refused is logged once for its path and the widget drawn unmasked. A mask doesn't change where the pointer hits.
- `Shadow::follow_content()` is `const`, beside `cut_out`: the shadow is cast by what the widget and its children draw, through their alpha, as Qt's `MultiEffect` shadow is, instead of by its box, and is offset, spread and blurred as a box shadow is. With a mask it is cast by the masked shape, and cut out it is multiplied by one minus the content's alpha. A content shadow reaching more than 64 logical pixels past its content, its spread plus one and a half blurs, isn't drawn and adds nothing to the painted area; it is logged once with the widget's path, and `Shadow::casts_from_content()` says whether a shadow is within that. It costs a blur each time the content repaints, so a box takes a box shadow. A fade takes the end style's mask, and whether its shadow follows the content, at once.
- `Image::transition(...)` takes an `ImageTransition`, a value or a closure: `none()`, the default, shows a new source at once, and `crossfade(transition)`, `wipe(transition, angle)`, `circle(transition)` and `pixelate(transition)` bring it in over the old picture over a Phase 8 `Transition`, so its delay and easing work as elsewhere, and are `const`. A wipe's angle is CSS's: 0 sweeps toward the top, 90 toward the right. Both pictures are drawn at the image's size, fit and filters. A new source mid-transition starts again from the picture most of the box shows, the one leaving before halfway and the one coming in from halfway on, and going back to that picture shows it at once. A first source, a hidden image or a transition of 0 ms shows at once. A source that can't be loaded keeps the old picture, logged once, though the clock still runs and repaints the box for the transition's length, since only the renderer learns the file is bad.
- The dashboard example shows Phase 16's drawing together: a battery ring on a `Canvas` from `cx.upower()`, a dim full circle under an arc with round caps in a sweep gradient started a little before 12 so the cap there isn't drawn in the end's color; a CPU graph on a `Canvas`, a polyline over a fading linear fill and dashed quarter lines; symbolic icons tinted in the text's color; a player card with the art sharp in front of a blurred, darkened copy faded out by a gradient mask, and a title with a content shadow; and a wallpaper, an `Image` in a `Layer::Background` panel on all four edges, sized from `cx.outputs()`, that crossfades when `crabshell ipc wallpaper set` gives it a new absolute path.
- Procfs files change without an inotify event, so a value read from one, such as the CPU's use, is sampled with `cx.interval` in a context that lives only while it shows; hiding frees the context, and the sampling with it. The example's `open` and `close` set its `shown` signal only on a change, and the effect that builds it reads it through a `cx.memo`: a `set` notifies even with the value a signal already has, so without that, opening an open dashboard builds it again.
- `IntoProp` is in the prelude, so a config's own widget can take `impl IntoProp<T>`, a value or a closure, as the toolkit's do.
- The prelude adds `Canvas`, `Pen`, `Path`, `Paint`, `Stroke`, `Cap`, `Join`, `Radii`, `Mask`, `ImageTransition` and `IntoProp`. `Style::radius` holds a `Radii`, `Style` and `Shadow` have new fields, `GradientShape` and `Content` new variants, and a clip's cut corners no longer pass the pointer, so Phase 16 ships with Phase 15 as 0.5.

## Process model and reload

Two processes run. The `crabshell` CLI, installed once per machine, is the supervisor. The shell is the user's own compiled binary, which links the `crabshell` library.

The CLI has four commands; v0.1 had the first three:

- `crabshell init` copies the template into `~/.config/crabshell/`. It refuses if the directory already exists.
- `crabshell run` builds the config, starts the shell, watches for edits and supervises.
- `crabshell check` runs `cargo check` and prints errors, for editors and scripts.
- `crabshell ipc [<target> <function> [args...]]` calls a running shell's `cx.ipc` handlers, or lists them (Phase 10).

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
- The template line `--path` replaces is built from the CLI's own major and minor version, which the library shares, so a release bump that misses the template fails a test.
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

Rules settled for IPC and the supervisor (Phase 10):

- The shell serves `cx.ipc` handlers at `$XDG_RUNTIME_DIR/crabshell/ipc-<display>.sock`, named for the last part of `WAYLAND_DISPLAY` (`wayland-0` without it), in the directory `persist` makes private. It binds only at the first registration, replaces a file already there, and removes it at exit only if its device and inode are still its own, so an old shell never removes a newer one's. A newer registration under the same names replaces the older and outlives the older one's owner.
- One request per connection: a line of JSON, `{"target","function","args"}` or `{"list":true}`, answered with `{"ok"}` or `{"error"}`. Requests are capped at 64 KiB and replies at 1 MiB; at most 16 connections are open, each closed 5 s after it opened. Connections are edge-triggered, so one waiting for its request sleeps. An accept that fails while a connection waits, as with no file descriptors left, rests the listener for 1 s instead of spinning. A panicking handler, or the drop of one replaced, is logged with where it was registered; the handler is disabled and the client told.
- `crabshell ipc` finds the socket the same way, gives the exchange 5 s, reads the reply to its newline, and exits 0, 1 for an error or no shell, or 2 for bad arguments.
- The supervisor starts the shell with glibc's `mmap` threshold fixed at 128 KiB (`GLIBC_TUNABLES=glibc.malloc.mmap_threshold=131072`, added to the user's own unless that sets it). Without it, glibc raises the threshold to the size of each large block freed, so a second decoded wallpaper's buffers stay in the heap: 41 MiB for a 4K image, measured. The shell's own spawns take it back out, and the supervisor's socket variable too.
- Before it exits for a reload or a lost compositor, the shell stops every program `cx.process` started, since `exit` skips the drops that would. On SIGTERM or SIGINT, as from `crabshell run` on Ctrl-C or a session ending, it does the same and removes its IPC socket, then dies by that signal as before. The handlers come from `signal-hook` and only write to a pipe the loop reads: blocking the signals for a signalfd would block them in every program the shell starts too, which inherit the mask.
- `cx.exit(code)` does the same, then exits with `code`. glibc's `exit` drops the services as well, which stops the programs, but Rust doesn't promise that, so the stop stays explicit.

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
- `owner.is_disposed()` says whether an owner was disposed, itself or with its parent, so a handle can refuse quietly without making a node to find out (Phase 12).

More required tests:

- [ ] An effect whose captured value panics when dropped is disabled, and later flushes still run every other effect.
- [ ] Exiting with an owner still alive, by returning from `main` or calling `process::exit(0)`, gives exit status 0.
- [ ] Rebuilding children 10 times with `effect_scoped` leaves the arena's slot count where it was after the first run.
- [ ] Disposing 50 child owners that read one long-lived signal returns that signal's subscriber count to where it started.

## Widgets, layout and styles

v0.1 shipped ten widgets, Phase 7 added five, Phase 10 one, Phase 15 three, Phase 16 one, and the phases after it add the rest a full shell needs (see "Beyond v0.1"). All but one are in `crabshell-ui`. `Tray` is in `crabshell-services`, because it needs the tray service and `crabshell-ui` can't depend on services; the prelude re-exports it. Users build everything else by combining them.

| Widget           | What it does                                                                                                                                                        |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `PanelWindow`    | A layer-shell surface on one output with anchors, size, exclusive zone, layer, margins, keyboard focus and an input mask.                                           |
| `FloatingWindow` | An ordinary `xdg_toplevel` window with a title, size limits, maximized, fullscreen, `visible` and an input mask, shown with `cx.floating`.                          |
| `PopupWindow`    | An `xdg_popup` anchored to a widget's rectangle, with an input mask. Tray menus use it.                                                                             |
| `Row`, `Column`  | Flex layout with `gap`, `padding`, `align`, `justify`, `wrap` and `reverse`.                                                                                        |
| `Stack`          | Children drawn on top of each other in one cell, or placed absolutely by their style's insets.                                                                      |
| `Grid`           | Children row by row in equal columns, spanning cells by their style.                                                                                                |
| `Spacer`         | Takes the leftover space in a `Row` or `Column`.                                                                                                                    |
| `Text`           | One line by default, with `ellipsis` and `max_lines`.                                                                                                               |
| `Icon`           | A freedesktop icon name, an SVG or PNG path, or raw ARGB pixels from a tray item, tinted or as it is.                                                               |
| `Button`         | A clickable box with hover, pressed and focus styles, a cursor and `enabled`, plus click, drag, long-press, wheel, key and drop handlers.                           |
| `Image`          | A PNG, JPEG, GIF, WebP or SVG file in its box, filled, contained or covered, with filters, an animated GIF playing and transitions between sources.                 |
| `Canvas`         | A box a paint closure draws paths, arcs, curves and dashed strokes in, cut to its corners.                                                                          |
| `ScrollView`     | Shows part of a taller or wider child, cut to its box, scrolled by the wheel, the touchpad, a drag, its scroll bar or its offset signal, with flings and overshoot. |
| `ListView`       | A scrolling list, down or across, that builds only the rows in view, with rows of any height, a `current` row kept in view and a highlight.                         |
| `GridView`       | A scrolling grid of cells of one size that builds only the rows in view, with a `current` cell and a highlight, for pickers.                                        |
| `TextInput`      | A one-line text field with a caret, selection, placeholder, password dots, the clipboard and input methods.                                                         |
| `Slider`         | A handle on a track that writes back a value in a range, dragged, scrolled or moved by keys.                                                                        |
| `Menu`           | A vertical list of buttons built from a DBusMenu layout, with separators, checkmarks and submenus as nested popups.                                                 |
| `Tray`           | One icon button per tray item, each opening its `Menu`. Built only from the widgets above.                                                                          |
| `ScreencopyView` | An output or a window, captured once or live, sized by its style.                                                                                                   |
| `SessionLock`    | A lock screen with a surface on every output while `locked`, shown with `cx.session_lock`.                                                                          |

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

### Styles

- `Style` has `background`, `gradient`, `shadow`, `color`, `font` (family, size, weight), `padding`, `radius` (each corner's, through `radii`), `border` (width, color), `border_dash`, `opacity`, `mask`, `clip`, `width`, `height`, `min_width`, `min_height`, `max_width`, `max_height`, `grow`, `shrink`, `basis`, `align_self`, `justify_self`, `left`, `top`, `right`, `bottom`, `span`, `z`, `translate`, `scale`, `rotate`, `origin` and `transition`. Every field is optional, and an unset field falls back to the widget's default. `clip`, the sizes, the flex and grid fields and the insets are layout properties; `z` is a paint property.
- `Text` inherits `color` and `font` from the nearest ancestor that sets them. Nothing else inherits, and there are no selectors or classes.
- Colors are sRGB with alpha. `Color::rgb`, `Color::rgba` and `Color::hex` are all `const fn`.

### Input

- Pointer events come from `wl_pointer`, and a touchscreen's first finger from `wl_touch` as a left button. Hit testing walks the tree from the top, in z order, and picks the deepest enabled `Button` under the pointer.
- The cursor shape is set through `wp_cursor_shape_v1`: the button's `cursor`, the pointing hand by default, and an I-beam over text fields.
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

- A screencopy view draws nothing until its first frame. A new source drops the frame it showed, and a frame the same as the one shown repaints nothing, so a live view of its own output settles. `Window::captures` lists the views shown with a box, for the platform.

- A button's `hovered` signal is `true` while the pointer is over a node on the path from the window down to the hovered button, so leaving an inner button for its outer one's text keeps the outer one hovered. It is written only on a change, once per signal for buttons sharing it, and goes `false` when the pointer leaves the window, the button is removed, its window is dropped, or a floating window is hidden. The signal belongs to the config: the button only writes it.

Rules settled while building `crabshell-ui` (Phase 8):

- One clock on the UI thread drives every animation, and the platform advances it only while one runs. An animation's first tick is its time 0. One that replaces a running one counts from the clock's last tick, so it moves at the next tick, and the first animation after a rest counts from its own first tick. A transition of 0 ms sets the value in the same flush.
- A new target while a value moves starts a new animation from where the value is. An animation whose value's owner is gone stops. One that panics is logged as `animation cx.animate at <file:line>` or `<widget path>.transition` and stops, and finished ones are dropped inside `catch_unwind` too.
- A style fades only on a paint change, on a widget already laid out, when the new style's transition is longer than 0. It blends from what showed, so leaving a hover mid-fade fades back from there. A color only one side sets fades from or to its transparent self, a border from or to 0 wide, a radius from 0 and an opacity from 1. A text color only one side sets, and gradients of different shapes or numbers of colors, change at once. A fading widget counts as drawing, so a layout that moves it repaints both places.
- A shadow's painted area is its shape grown by 1.5 blurs, past which the blur fades below a 255th. A fading shadow's area covers its start and its end, and a spread that swallows the box still reaches between the edges it would have. A shadow with a number that isn't finite is never drawn.
- `Gradient::stop` panics past four colors, which is a compile error in a `const`.

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
- A fling's speed is the moves of the last 100 ms before the release or the touchpad's stop; under 50 px/s it doesn't fling. It stops when it slows to 0, on a write to the offset from outside, on content that shrinks under it, on a press or wheel, or when its view or window goes. Overshoot is shown as `limit × (1 − e^(−0.5 × raw / limit))`, with a limit of a quarter of the box, springs back at a stiffness of 150 and a damping of 1, and rests within 0.5 px and below 10 px/s. It moves the child in the place pass only, as the offset does, never through a layer. The pointer leaving mid-gesture or mid-drag springs back without a fling.
- The scroll bar's handle is as long as the shown part, at least 16 px, placed by the offset shown, and drawn inside the view's clip after its children. It repaints where it was and is when it moves, comes or goes.
- A list row is measured after each layout that resizes it. A newly shown row costs one layout, or a second in the same call when its length differs from what it was counted as. A row that starts above the view grows upward and the offset moves with it, so the rows shown stay put, and a running fling carries on from the moved offset. A row measured in the flush that changes the items keeps its length, moved to its new index. The list calls `key` on every item when the items change and keeps each key's index.
- `current` on a row not built yet scrolls by the estimate, then shows the row whole once it is measured. Items that keep it in range don't pull the view back, and neither do rows measured later. While the view moves in a fling, a drag of its content, a touchpad gesture or a spring-back, as Qt's ListView with its tracked item, a change of `current` scrolls nothing, and the motion's end scrolls nothing either; a scroll-bar drag doesn't count as moving.
- A highlight slides only when its target changes, a new current index or new items; a move from a measurement or a new column count jumps.
- A grid's first show costs two layouts, since its width is known only after one. A new column count finds the top-left cell where the old columns showed it and keeps it in view, then the current one; a grid keeps `current` in view when it, the height, the cells' height or the column count changes, never for a width that keeps the columns.

Rules settled while building `crabshell-ui` (Phase 16):

- A canvas's closure waits for the first layout, then gets each new size through the layout's measure hook, after the tree is released. A run keeps at most 16,384 path commands and dash lengths, and its strokes may make 100,000 dashes in all. A drawing holding a number that isn't finite or is beyond 1e6 logical pixels, a negative radius or width, or a dash pattern of negative or non-finite lengths is skipped, and so is one past either budget; the first one skipped is logged once per canvas. A path's length for the dash count starts at its first point when it doesn't start with a move, as it is drawn.
- `hsl` follows CSS Color 4's conversion; the hue wraps, the rest clamp, and a number that isn't finite gives opaque black. A NaN passed to `with_alpha`, `mix`, `lighter` or `darker` leaves the color unchanged, and their amounts clamp to 0 through 1. Every 8-bit color survives the trip through HSL and back.
- A clip hides its children from the pointer past its corners as well as under its border; the inside curve is the corner less the border. In 0.4 the pointer still reached children in a clip's cut-off corners. A fade moves each corner on its own, and an unset side counts as square corners.
- Each image has an id from a process-wide counter. `Window::repaint_image(id)` repaints that image's box and returns false for an image that is removed, hidden or not laid out.
- A content shadow is a step on the way from a node out to the window, inside its transform, so it turns with the widget: every descendant's painted area and damage grow by its reach, and the node's bounds include it, a fading one between its start and its end. A node that casts or cast a content shadow repaints its old and new bounds when it changes, as a clip does, so the shadow of children that overflow or are scaled follows an instant change. A mask leaves bounds alone; one that comes, goes or changes repaints the subtree's bounds without a layout.
- An image's transition runs on the animation clock while it moves and repaints the image's box each tick, nothing during its delay. Its end lets go of the old picture with one last repaint, and a replaced transition or a window that goes stops it. It is logged as `<image path>.transition`. An image node's drop records its id, and `dropped_images()` hands each one out once, so a renderer can let go of what it kept for it; past 4,096 unread ids the list stops growing.

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
- `Image` files load by extension: PNG through `png`, JPEG through `zune-jpeg`, a GIF's frames through `gif`, WebP through `image-webp`, and SVG through `resvg`. They are halved, then scaled, to the size drawn, and kept in the icon cache.

### Shapes

Solid fills, linear, radial and sweep gradients, boxes with a radius per corner, solid, dashed and dotted borders, box shadows and shadows cast by the content, masks, and a canvas's paths. Blur behind a window is the compositor's, through `ext-background-effect-v1`. v0.1 had no gradients, shadows or blur; Phase 8 added them.

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

Rules settled while building `crabshell-render` (Phase 10):

- A captured frame is turned upright from the transform the compositor reports, and kept scaled down, its shape kept, to the least size that covers its view's box at the largest output scale; never up. Buffers in ABGR, ARGB, XBGR or XRGB 8888 are read, the X formats as opaque. The renderer keeps one scaled copy per view, dropped as soon as the platform ends the view, or 16 draws after its last use.

- A cut-out shadow is masked by the box's own shape, filled as its background is, so its edge meets the background's antialiasing; a sharp one is clipped by the inverted mask, a blurred one multiplied by it, in each tile.
- `quantize_colors(path, depth, size)` decodes with `Image`'s loaders and caps, halves the picture until its longer side is at most `size` (0 keeps it), skips pixels under alpha 128, and splits at the median of the widest channel into at most 2^`depth` boxes, `depth` clamped to 8. Equal channel values stay on one side, so two boxes never give one color. The colors are each box's mean, sorted by pixel count, then by their bytes. A 1920×1080 JPEG takes about 25 ms at size 128, release build.

Rules settled while building `crabshell-render` (Phase 8):

- A gradient is laid out in buffer pixels and moved onto each tile, so tiles agree. A linear one takes CSS's angle, with its line long enough for the ends to meet the corners. A radial one is a circle from the middle to the farthest corner. A clear stop fades each side out in its neighbor's color, as CSS blends with the alpha applied. Nothing is dithered, so a subtle gradient across a wide bar shows steps.
- A shadow is drawn under its box, so a translucent background shows it. A sharp one is a filled shape. A blurred one is a Gaussian of half the blur, worked out for each pixel in buffer pixels: the product of the error function on each axis away from the corners, and eight Gaussian-weighted rows near one. A corner smaller than the spread grows by less, as in CSS, so a square box keeps square corners.
- A full draw of an 1880 pixel wide box with a 12 pixel blur at scale 2 takes 5.1 ms, 0.8 ms of it without the shadow.

Rules settled while building `crabshell-render` (Phase 14):

- A transformed node draws its subtree into a layer holding what lands in the tile, plus 2 pixels, in its own buffer pixels, then blends it in through the transform with bilinear filtering. Opacity shares that layer, and a clip is cut inside it, so the clip follows the turned shape. As with tiles, a curve cut at a layer's edge antialiases a few levels differently near tile edges, and a layer past 4 Mi pixels isn't drawn.
- A turning 200×60 card with text draws in 1.51 ms a frame at scale 2, and the motion example's spinner in 0.2 ms.

Rules settled while building `crabshell-render` (Phase 15):

- Render draws a node's children in the order `NodeView::children` gives, which is z order, so `z` needed no change here.
- A scroll bar's handle is drawn with `draw_box` from its style, inside the view's clip mask after its children, so it is cut to the view's rounded corners. A 6 px handle at scales 1, 1.5 and 2 is pinned by `scroll_bar@*.png`.

Rules settled while building `crabshell-render` (Phase 16):

- A canvas's drawing is scaled from its box's corner, snapped to the pixel grid, and drawn through a mask of its box with the style's corners, in each tile, so a repaint matches a full draw. Strokes go through tiny-skia's own stroker with a miter limit of 4, an arc becomes cubic curves of at most a quarter turn, and a gradient's shader undoes the path transform so it lies over the box in buffer pixels. `canvas@*.png` pins a ring, a gradient-filled graph, dashes and a canvas inside a turned, translucent clip.
- Each corner is clamped to half the shorter side in pixels, and a uniform radius draws as it did. A sweep is laid out in buffer pixels and turned about the middle by its transform, since tiny-skia 0.12 ignores a whole-turn sweep's start angle. In a blurred shadow, a row near a corner takes its reach from that side's corners, and the fast product of error functions holds past the largest corner, so a radius per corner costs about what one does: 7.4 ms against 7.3 ms for an 1880 px box with a 12 px blur at scale 2.
- A dashed border is stroked along its middle line in buffer pixels with miter joins, butt caps, and round caps for dots, a dash centered on the end of the top-left corner so a square corner there is filled. Each tile keeps the dashes whose box comes within twice the width plus a pixel of it, so a full draw of an 1880 px wide box at scale 2 takes 6.1 ms dotted and 3.2 ms dashed, against 0.8 ms solid. A number that isn't finite, a negative one, an `off` of 0 or a period under a pixel draws the border solid. Another square corner can still lose part of its outer half-width when a dash ends right by it; laying the dashes out per side, as browsers do, would fix it. `corners@*.png`, `sweep@*.png` and `dashes@*.png` pin the new drawing.
- Image filters run once on the picture scaled to its box in buffer pixels, and the cache keeps the result, keyed by the file, size, fit, tint and each number, with the blur in buffer pixels; each filter clamps before the next, on premultiplied pixels, and a tinted icon is cached apart from the plain one. The blur is three box blurs each way, which the Filter Effects spec allows, with pixels past the edge repeating the edge so a blurred picture keeps opaque edges, and its deviation clamped to the picture's longer side, so any larger number, infinity included, ends quickly. Blurring a 1920×1080 picture takes about 50 to 80 ms once per size, on the loop's thread. `tinted_icons@*.png`, `image_filters@*.png` and `blurred_images@*.png` pin them.
- A GIF's frames are composed on its canvas as each frame's disposal says, then scaled and filtered as a still picture, all at its first draw. A frame not wholly on its canvas is refused, by the decoder's consistency check and again before it is composed; a bad first frame refuses the file, and a later one stops the animation on the last good one, logged. Decoding may come to 64 Mi canvas pixels, and the frames kept to 8 Mi pixels at the size drawn, within what the picture cache's 32 MiB has left after the pictures a recent draw used and any waiting animation's frames; past any of these, the first frame shows, logged once.
- The renderer keeps each GIF's place by its image's id. A drawn frame is due its delay after the last was due, or after the draw if that has passed; `next_image_frame` gives the earliest due time and `advance_images` moves the due GIFs on and returns their ids. A GIF waits again only once drawn, so one hidden, off its window, clipped out or in a window that can't draw wakes the loop at most once more, and a waiting GIF's frames stay cached.
- A mask, a content shadow, opacity and a transform share one layer: the mask is applied after the contents are drawn, any clip already cut inside it, the content shadow is worked out from what is left and put under it, and opacity and the transform then blend the result in. An ellipse mask is filled antialiased on the box's pixel edges, a gradient mask is laid out as a background is, and an image mask is read at the box's size with `Fit::Fill`, paused on a GIF's first frame. `masks@*.png` and `content_shadows@*.png` pin them.
- A content shadow is cast in cells of a grid laid from the corner of the area it can show in, each at least 256 pixels and four reaches a side, at most once a draw, when a tile first needs it, from the subtree drawn into a layer over the cell moved back by the offset and grown by the reach and 2 pixels. That layer starts at the same pixel in every draw, since content drawn into a layer that starts elsewhere antialiases a few levels differently. The offset and spread are whole pixels, the spread a square dilation or erosion, and the blur follows SVG's `feGaussianBlur` with integer box sums, so repaints match full draws. A full draw at scale 2 of a 13 px line with a 6 px content shadow takes 1.1 ms, a 64 px icon 0.8 ms, and a 400×300 card with a blur of 40 35 ms.
- Images are drawn by copying opaque rows and blending the rest, which takes a 1080p still from 12.3 ms to 3.6 ms a full repaint at scale 1, release build, and a file's modification time is looked at once a draw. A transition blends both pictures per pixel in the picture's own buffer pixels, premultiplied, then lays them over what is under them. A wipe's edge and a circle's rim are antialiased over a pixel, the circle growing from nothing to past the corners; pixelate grows blocks to a twentieth of the longer side halfway and shrinks them back, each block showing its middle pixel, with the pictures swapping through the middle half. A GIF being left shows its first frame. A 1920×1080 frame at scale 1 takes 3.6 ms for a crossfade, 6.8 ms for a 45° wipe, 6.6 ms for a circle and 5.0 ms for pixelate. `image_transitions@*.png` pins each halfway.
- The file a transition leaves is resolved once per transition, so every tile draws the same. Its picture is held only while a draw uses it: it is dropped when the transition ends, is replaced, is switched to none or its image is dropped, and when its own window, named by its damage history, draws without drawing it. A picture another image showed within the last 16 draws, or another transition still leaves, is kept, so two outputs crossfading the same wallpaper hold it once. Two 1080p pictures, 16.6 MB, are cached during a crossfade and one after.

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

The control example's tests pin its date with `CONTROL_TODAY`, which only they set, and run on a 3360×2100 output, whole in logical pixels at every quarter scale and tall enough for its 632-pixel panel at scale 2. A touchpad gesture from the virtual pointer carries its send time; seen from outside, a fling is told from the fingers' own motion by where it rests, so the exit check flings past the list's end and checks that the scroll bar's handle rests at the bottom.

The dashboard's golden runs the shell as a child of the test binary, with `XDG_DATA_DIRS` pointing at `tests/fixtures` and `cx.set_icon_theme("crabshell-fixtures")`, a name no theme in `~/.icons` or the machine's data directories takes, while `HOME` and `XDG_DATA_HOME` stay the user's so the user's fonts still load. With several windows the logged first frame may be another window's, so a golden waits for a pixel of the window it checks, then for two equal screenshots. A check that memory was freed runs with the `GLIBC_TUNABLES` the supervisor gives every shell; without it, glibc raises its mmap threshold to a large block once one is freed, the next block lands on the heap, and RSS keeps its high-water mark, as a shell run without the supervisor does.

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
- Blur binds `ext_background_effect_manager_v1` version 1. A surface gets its effect object the first time it asks, and the region is cut to the surface's size and sent before the commit that applies it, and again when the size changes, as the mask is. Without the protocol, or with the compositor's blur off, a window that asks is logged once per shell and shows unblurred. Blur that turns on later shows from each window's next frame. No compositor here has the protocol (SwayFX 0.6, wlroots 0.20, labwc 0.20), so only the path without it is tested.
- `Stats::drawing` adds up the time spent drawing frames.

Rules settled while building `crabshell-wayland` (Phase 9):

- Idle binds `ext_idle_notifier_v1` and `zwp_idle_inhibit_manager_v1`, both version 1. A watch is a notification on the first seat, so a watch made before any seat exists never fires, and one whose seat goes stays as it was. A panicking callback is logged and the shell goes on.
- A window's idle inhibitor is made once its surface is configured, while its `inhibit_idle` is true, and goes with the surface. Headless Sway honors one on a layer surface.

Rules settled while building `crabshell-wayland` (Phase 10):

- Toplevels bind `zwlr_foreign_toplevel_manager_v1`, versions 1 to 3, at the first `watch_toplevels`, so a shell that never asks hears nothing. Without it, that is logged once and the list stays empty.
- A handle counts at its first `done`, and changes at each later one. The watch hears the whole list once per dispatch that changed something. `closed` drops and destroys a handle and clears it as any other's parent, as the protocol doesn't promise to say; `finished` empties the list for good. Outputs are named from the outputs the platform knows, one not announced yet left out until it is.
- Titles and app ids are cut to 4 KiB at a character; past 1024 toplevels the rest are destroyed and logged once. Activate uses the first seat; fullscreen names no output and needs version 2.
- Headless Sway honors activate, fullscreen and close, ignores maximize and minimize, and never says a window's parent; labwc does all of them, so its tests cover those.
- Screencopy binds its protocols when a shown window first holds a view, and the ext toplevel list when a view of a window first does. Each view of a shown window gets a session for its source, waiting while the source isn't there, with a buffer of the size and format announced; a format `wl_shm` doesn't take too is never used. Sources past 8192 pixels a side or 24 million pixels are logged and show nothing.
- A one-shot view ends its session after its frame. A live view asks for the next frame once its window drew the last one, so it takes no more than the window shows and nothing while the window can't draw; a frame the same as before asks again at once, which the compositor answers only when the source changes. A view whose box outgrows its frame, or a higher scale, captures again.
- `buffer_constraints` starts a new session; any other failure is tried again once. A stopped source ends the session and keeps the last frame; an output announced again gets a new session.
- Window views pair an `ext_foreign_toplevel_handle_v1` with the wlr toplevel of the same app id and title, oldest unpaired first, freeing a pair whose wlr toplevel went. Two windows with the same app id and title may swap.

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
- `hyprland_surface_manager_v1` (hyprland-protocols v0.8.0, beside the other two) is bound at versions 1 to 2 the first time a window asks for anything but the defaults, and a surface gets its object then; without it, that is logged once, and version 1 has no visible regions, also logged once. The visible region is in buffer pixels, as the protocol says: grown by a logical pixel each way so curves' soft edges stay inside, cut to whole buffer pixels and to the buffer, and sent again on a new size or scale. An empty region is sent as none, which Hyprland reads as no hint. It passed a hand check on Hyprland 0.57.2 on 2026-11-15, at version 2: a fade blended the panel and a floating window with what is behind them, Hyprland skipped drawing outside the visible mask, the region was sent again in buffer pixels at scale 1.5, and hiding and showing a window three times made a new object each time with no protocol error.
- A panel reconfigures layer-shell only when a layer-shell setting changes. A change to its mask, blur, compositor opacity or visible mask alone is sent before the next frame, which commits it, and a floating window commits its toplevel only when its own settings change; a hint that sends nothing commits nothing.

Rules settled while building `crabshell-wayland` (Phase 14):

- One calloop `Timer` serves every config timer. After each flush it is armed at the earliest deadline, moved when that changes and removed when none runs. Deadlines are monotonic, so they pause while the machine sleeps. A hand-driven `EventLoop::dispatch` arms a timer made outside the loop only after its first wait.

Rules settled while building `crabshell-wayland` (Phase 15):

- After the flush, taking popups, closing, creating surfaces, updating toplevels, and the windows' layout and draw run in passes, at most 4 an iteration. A layout that writes `geometry` flushes, and another pass runs while that left a lock change, new popups, a window that wants to close, changed surface settings, or a configured window that needs a layout or can draw its damage now. A window waiting for its frame callback doesn't count, so an animation stays at one pass, and a feedback that never settles stops after the fourth pass and goes on at the next event, never by itself.
- `BTN_SIDE` and `BTN_BACK` are Back, and `BTN_EXTRA` and `BTN_FORWARD` are Forward. One scroll frame becomes one `Wheel` with both axes. Notches come from `axis_value120`, or `discrete` times 120 on older compositors. A frame with no source is `Wheel` when it has notches and `Continuous` otherwise. A frame with nothing in it, or a number that isn't finite, sends nothing.
- Pointer buttons, motion and scrolling and touch carry `wl_pointer`'s and `wl_touch`'s event times to the window; an enter carries none.
- `wp_cursor_shape_v1` sends every shape. On a version 1 device, `DndAsk` falls back to `ContextMenu` and `AllResize` to `Move`. The cursor is checked after every loop turn, and only a change is sent.
- `wl_touch` is bound through sctk when a seat has it. The first finger down is a motion and a left press on its surface, its motions move, its lift releases and leaves, and a cancel leaves without a click. Other fingers are ignored until it lifts. A down or lift sets the serial popups grab with and the held state `start_move` checks. Headless Sway has no virtual touch device, so touch is unit-tested only.
- Sway keeps sending a held button's motion to its surface outside it, with negative or too-large coordinates. A virtual pointer's finger swipe arrives with source `Finger` and `axis_stop` on both axes.

Rules settled while building `crabshell-wayland` (Phase 16):

- One calloop timer serves the GIFs. After each loop turn it is armed at `Renderer::next_image_frame` and removed when that is `None`; when it fires, it advances the GIFs and repaints each one's box through `Window::repaint_image`.

## Services

v0.1 had four services in `crabshell-services`, and Phase 9 added six; the phases after it add Quickshell's others (see "Beyond v0.1"). Each one owns its connection, runs on the main thread, and hands out `ReadSignal`s plus a few methods. Every service is created on first use and shared by all bars, so two outputs never mean two connections.

| Service           | Signals and methods                                                                                                                                                                                                                                                                                                                 | Source                                                                      | Crate                         |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- | ----------------------------- |
| Workspaces        | `workspaces()` gives `Vec<Workspace>` with id, name, output, focused, visible, urgent. `focus_workspace(name)`, and since Phase 12 `outputs()`, `dispatch(command)` and `on_event(kinds, handler)`.                                                                                                                                 | Sway IPC at `$SWAYSOCK`                                                     | Own framing with `serde_json` |
| Hyprland          | `workspaces()`, `monitors()`, `toplevels()` with focus, each monitor's workspace and urgency. `focus_workspace(target)`, `dispatch(command)`, `on_event(handler)`.                                                                                                                                                                  | Hyprland's sockets in `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/` | Own framing with `serde_json` |
| Tray              | `items()` gives `Vec<TrayItem>` with id (bus name and path), app_id, title, status, category, icon_name, icon_pixmaps, attention_icon_name, icon_theme_path, tooltip, item_is_menu and menu. `activate`, `secondary_activate`, `scroll`, `menu()`, `open_menu`, `open_submenu`, `close_menu`, `menu_event`.                         | StatusNotifierItem and `com.canonical.dbusmenu` on the session bus          | `zbus`                        |
| Audio             | `default_sink()` gives `Option<Sink>` with name, volume_percent, muted. `change_volume(percent)`, `toggle_mute()`. Since Phase 13 each node's and stream's `volumes` and `channels`, `set_sink_volumes`, `set_source_volumes`, `set_stream_volumes` and `peaks(target)`.                                                            | PulseAudio protocol, served by pipewire-pulse on PipeWire systems           | `libpulse-binding`            |
| Clock             | `clock(precision)` gives a `jiff::Zoned`                                                                                                                                                                                                                                                                                            | System time and time zone                                                   | `jiff`                        |
| UPower            | `devices()`, `display_device()` give `PowerDevice`s with kind, state, percent, present, times to empty and full, energy rate. `on_battery()`.                                                                                                                                                                                       | UPower on the system bus                                                    | `zbus`                        |
| Power profiles    | `active()`, `available()` give `PowerProfile`s, and `degradation()` why performance is held back. `set(profile)`.                                                                                                                                                                                                                   | power-profiles-daemon on the system bus                                     | `zbus`                        |
| Bluetooth         | `adapters()`, `devices()` with name, address, powered, discovering, state, connected, paired, trusted, battery. `set_powered`, `start_discovery`, `stop_discovery`, `connect`, `disconnect`, `pair`, `set_trusted`, `forget`.                                                                                                       | BlueZ on the system bus                                                     | `zbus`                        |
| Network           | `wifi_enabled()`, `connectivity()`, `devices()` with kind, state, connection, last failure, Wi-Fi mode and cable link, `wifi_networks()` with SSID, strength, secured, security, connected, `saved_connections()` with uuid, id, kind and settings. `set_wifi_enabled`, `scan`, `connect`, `disconnect`, `forget`, `clear_secrets`. | NetworkManager on the system bus                                            | `zbus`                        |
| Media players     | `players()` gives `Player`s with status, track, artists, art, length, position and what each can do. `play_pause`, `next`, `previous`, `seek`, `set_position`, `set_volume`, `set_shuffle`, `set_loop`, `raise`.                                                                                                                    | MPRIS on the session bus                                                    | `zbus`                        |
| Notifications     | `list()`, the history, and `popups()`, the ones still showing. `dismiss`, `invoke`, `clear`, `on_closed(handler)`.                                                                                                                                                                                                                  | The shell serves `org.freedesktop.Notifications` on the session bus         | `zbus`                        |
| Desktop entries   | `applications()` gives `DesktopEntry`s with name, comment, icon, exec, categories, keywords, actions and `no_display`. `lookup(app_id)`, `launch(id)`, `launch_action(id, action)`.                                                                                                                                                 | `.desktop` files in the XDG data directories                                | `inotify`, `rustix`           |
| Programs          | `cx.process(command)` gives a `Process`: `running`, `exit_status`, `stdout`, `stderr`, `on_line`, `on_split`, `write`, `close_stdin`, `kill`, `restart`. `cx.exec_detached(command)`.                                                                                                                                               | Programs the shell starts                                                   | `rustix`                      |
| PAM               | `cx.pam(service)` gives a `Pam`: `state()` and `messages()`. `start(user)`, `respond(text)`, `abort()`.                                                                                                                                                                                                                             | libpam, a thread per attempt                                                | `nonstick`                    |
| greetd            | `state()` and `messages()`. `create_session(user)`, `respond(answer)`, `start_session(command, env)`, `cancel()`.                                                                                                                                                                                                                   | greetd's socket at `$GREETD_SOCK`                                           | Own framing with `serde_json` |
| polkit agent      | `request()` gives an `Option<AuthRequest>` with the action, message, icon, identities, state, info and errors. `respond(text)`, `cancel()`, `pick_identity(name)`.                                                                                                                                                                  | polkit on the system bus, and its `polkit-agent-helper-1`                   | `zbus`                        |
| Files and sockets | `cx.file_view(path)` with `error()`, `cx.json_file(path, default)`, `cx.socket(path)` with `on_split`, `cx.socket_server(path)`.                                                                                                                                                                                                    | Files and Unix sockets                                                      | `inotify`, `serde_json`       |

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
- Audio: sinks, sources and streams are read whole on each of their events. A failed read keeps the last list. A sink's monitor isn't listed as a source, but can be the default source. Setting a volume keeps the channels' balance. Peak meters came in Phase 13.

Rules settled while building `crabshell-services` (Phase 10):

- Programs run in a process group of their own, with their outputs through non-blocking pipes, so a script can open `/dev/stdout`. A program starts on the loop's next turn, so `on_line` and `with_stdin` set at once apply. Each output keeps up to 1 MiB in its signal, and nothing after a character that didn't fit; lines reach `on_line` past that, and a line over 1 MiB is dropped and logged. Input written before the start waits for it; `close_stdin` closes once everything written is sent; 4 MiB unread closes it at once.
- A program's end comes from a pidfd. What is left in its pipes is read then, up to 1 MiB each, as a program it started may hold them open and keep printing. A run reaped behind the shell's back ends with no status rather than spinning. A restart, `kill` or the context going sends `SIGTERM` to the group; a stopped run, and every `exec_detached` program, is reaped through a pidfd that leaves the loop once the child is gone. A panicking command stops the run before and is logged with where `cx.process` was called; the next change tries again.
- Programs the shell starts don't inherit the supervisor's socket variable or the shell's own glibc tunable.
- Sockets connect at once and never retry on their own. A refused write stops writing, but reading goes on to the end, which, like a reset, still passes on the last line. 4 MiB unwritten closes the socket. Lines read before the config closes or reconnects aren't passed on after. A socket closes with its context and never connects after.
- Files are read on the main thread, up to 16 MiB, as UTF-8. They are followed through inotify on their directory, or while it is missing the nearest directory above it, watched again when a directory on the way comes, goes or moves; read again on a close after writing, a rename or a removal, not on a create, when nothing is written yet. A write goes through a new file renamed over the old, making the directory if it is missing, following a link and keeping the permissions.
- A JSON file writes only a value the config set, not the one it started from, and never writes back what it just read. The config's `Serialize`, `Deserialize` and `PartialEq` are caught: a panic is logged with the path, and the value and the file go on.
- One inotify instance per shell, in the loop only while a watch is alive.
- Desktop entries come from `applications/` under `$XDG_DATA_HOME` and each of `$XDG_DATA_DIRS`, recursively, links followed and each directory read once. The first file with an ID decides it, even one left out for `Hidden`, its type or a missing `TryExec`. `NoDisplay` and `OnlyShowIn`/`NotShowIn` against `XDG_CURRENT_DESKTOP` set `no_display`. Localized keys follow `LC_ALL`, `LC_MESSAGES` and `LANG` in the spec's order. Exec is parsed strictly by the spec's quoting rules with no shell: `%f %F %u %U` and the deprecated codes are dropped, `%i`, `%c` and `%k` expand, and a malformed line or an unknown code runs nothing. A `Terminal` entry runs in `xdg-terminal-exec`; one that only D-Bus can start is refused for now.
- The list is read on the loop's next turn after the first call, and again once the files have been still for 100 ms after a change to an entry, a directory or a link; the timer exists only after a change. Files over 64 KiB, lines over 4 KiB, more than 10,000 entries and anything but regular files are left out. `lookup` tries the ID, then the ID ignoring case, then `StartupWMClass`, then the ID's last dotted part.

Rules settled while building `crabshell-services` (Phase 11):

- greetd: one connection, made at the first request and again after one is lost, with one request in flight, as greetd-ipc(7) has it. A message is its length as a native-endian `u32`, then JSON, at most 64 KiB; texts are cut to 4 KiB and the last 64 informative messages kept. Informative and error messages are acknowledged at once, and an error on a create or an answer cancels the attempt so another can begin. A new session cancels the one greetd holds first, and replies to an attempt a queued cancel ends show nothing. `respond` goes only while greetd asks and `start_session` only once authenticated; otherwise they are dropped and logged. greetd joins a session's command with spaces and runs the line with sh(1), so `start_session` documents that an entry with spaces needs its own quotes, rather than quoting entries itself: greetd-ipc(7) calls the command a command line, and whether every greetd setup runs it through sh is undocumented. An answer is written straight into one buffer, zeroed once sent, and never logged.
- PAM: each attempt runs `pam_authenticate` on a thread of its own through `nonstick`, and its channel is in the loop only while it runs. A success also runs the account step, where `pam_faillock` forgets past failures; its refusal is only logged. An abort, a second start or the asking context going refuses the waiting question, which ends the thread; under `pam_faillock` that counts as a failed login. `start(None)` finds the user by uid in `/etc/passwd`. PAM's auth error is `Failure`. Since Phase 13 the rest is an `Error` with a kind: `StartFailed` when the transaction can't start or a name holds a NUL, `InternalError` for the shell's own conversation refusing an answer or an attempt ending without a result, and `TryAuthFailed` for any other `pam_authenticate` error. Texts are cut to 4 KiB and the last 64 messages kept, and an answer with a NUL is refused before libpam. Answers are zeroed once handed on, except the copy nonstick makes for libpam.
- polkit: the first `cx.polkit_agent()` registers as `unix-session` from `XDG_SESSION_ID`, else `unix-process`, at `/org/crabshell/PolkitAgent`, and again whenever polkit's name gets a new owner; a refusal is logged. Only the unique name that took the registration may call; others get `AccessDenied`. One request shows and 8 wait; past that, or with a bad cookie or no known user, polkit gets `Failed`.
- polkit: each try runs `polkit-agent-helper-1` as the first identity, the session's user first, with the cookie on its input and a line per prompt or message. From polkit 127, systemd starts the helper as root for each connection to `/run/polkit/agent-helper.socket` and the helper may no longer be setuid, so the agent connects there first and sends the user's name as a line before the cookie. Only when nothing listens there does it spawn the helper, logging why if the socket is there but refuses. `SUCCESS` answers polkit with an empty return; after 3 failures it gets `Failed`, and a helper that can't start answers `Failed` at once. Answers with a line break or NUL are refused, and a cancel kills the helper. Texts are cut to 4 KiB, with at most 64 identities.
- No test reaches a real account: PAM's tests run a scripted backend, and the one libpam test uses `other` and skips unless it is `pam_deny` only. polkit's run on a private bus with a fake helper, and greetd's against a fake socket. Two ignored tests reach a real polkit and a real greetd, and do nothing unless their test passwords are set; they are for a throwaway container with a test user.

Rules settled while building `crabshell-services` (Phase 12):

- Sway: the connection subscribes to `workspace`, to `output` from the first `outputs()`, and to each kind a handler asks for, never to the rest. Sway has no unsubscribe, so an event of a kind only a gone handler wanted makes the service connect again with what is still wanted, once Sway answered every request sent, since it drops a client that hangs up with requests unread. The lists stay until the new ones arrive; an event never subscribed to changes nothing.
- Sway: `GET_OUTPUTS` is sent again once per read that held an output or workspace event. An output without `active` or `rect`, as a VR headset, shows as inactive, and one without a scale reads 1. A bad list is logged and the last one kept.
- Sway: handlers run after the read, with nothing borrowed, in the order given, and an event must be UTF-8 JSON within the 4 MiB cap. A handler that panics is logged with where `on_event` was called and taken away. `dispatch` sends the text as written; over 4 MiB or while disconnected it is logged and dropped. The services' Sway tests also run against a private headless Sway, ignored by default.
- Hyprland: the event socket opens first, then `j/workspaces`, `j/monitors`, `j/clients` and `j/activewindow` are read. Each request has a connection of its own, goes in one write with its writing then shut down, and ends when Hyprland closes it. At most 8 requests are out at once, under Hyprland's listen backlog of 10, and the rest queue in order, at most 64 dispatches of them. Connects never wait: while the backlog is full, a request waits at the head of the queue for one of the shell's requests to end or a new one to be made, and on the event socket a full backlog counts as unreachable.
- Hyprland: an event reads again the lists it can change, a burst read at once reads each list once, and an event during a read reads once more after it. `activewindow` reads the windows too, since a class change comes only that way. Both versions of an event count, for a Hyprland without the v2. The lists show once every read a burst of events started is answered, so a frame doesn't mix one event's new lists with old ones; a list read again for a later event holds nothing back.
- Hyprland: without the signature or `XDG_RUNTIME_DIR`, or with the event socket missing, the lists stay empty, that is logged once, and nothing retries. When the event socket closes while its file is still there, the shell connects once again and reads every list, keeping the old ones meanwhile, and urgency ends for the window the first focused-window read names and for each window the first window list lacks; one that closes before any event came, or whose file is gone, empties the lists for good.
- Hyprland: `j/status` is read once, at the first `focus_workspace`. A `configProvider` of `lua` means a switch is `hl.dsp.focus({ workspace = "…" })`, with backslash and quote escaped and control characters written as `\ddd`; `hyprlang`, or `unknown request` from Hyprland before 0.55, means `workspace …`. Any other reply is logged and read as the old syntax. Switches wait for the answer, at most 64, and go if it can't be asked.
- Hyprland: workspace references match by id when both have one, otherwise by the `address` 0.57 added, and named workspaces sort first. A shown special workspace is active, and focused on the focused monitor. `fullscreen` is a flag in older Hyprland and a mode later, where 2 is fullscreen and 1 maximized; on 0.41 and older a maximized window counts as fullscreen. Urgency comes only from `urgent` events and ends on that window's `activewindowv2` or `closewindow`.
- Hyprland: a reply over 8 MiB, or one that isn't JSON, is logged and the last list kept, and an entry that doesn't parse is left out. Texts are cut to 4 KiB at a character, at most 4096 entries per list and 4096 urgent windows are kept, and event lines past 1 MiB are dropped. A dispatch over 8 KiB isn't sent, and any reply but `ok` is logged. Handlers run after the event's reads are sent, with nothing borrowed, inside `catch_unwind`, and one that panics is logged and not called again.
- Hyprland is tested against a fake Hyprland built from its docs and its BSD-3 source; on 2026-11-15 its IPC also passed a hand check on Hyprland 0.57.2 with a Lua config. Niri gets no service of its own, since Quickshell has none: it is reached only through the generic protocols, untested.

Rules settled while building `crabshell-services` (Phase 13):

- A service reads a signal's old value untracked before it sets it, and runs `on_closed` handlers untracked, so an effect that calls a service never runs again for what the service shows. A D-Bus daemon's watch ends as soon as the root owner goes, not at its next message, and clears what it held.
- Tray: `Category` is read with the rest. A missing one is `ApplicationStatus`; an unknown one is logged at debug and read as `ApplicationStatus`.
- Power profiles: `PerformanceDegraded` that is `""` or missing is `None`; a value the daemon's docs don't name is `Other`, logged at debug.
- Bluetooth: `PowerState` is read beside `Powered`; without it, or with a value BlueZ's docs don't list, the state comes from `Powered`. A `connect` or `disconnect` shows Connecting or Disconnecting until BlueZ answers that call or `Connected` changes to where it was headed. A newer call replaces the state, an older call's answer then changes nothing, and BlueZ leaving ends them all.
- Notifications: `on_closed` handlers run after the lists change, in the order given, with nothing borrowed, inside `catch_unwind`; one that panics is logged with where `on_closed` was called and removed. A dismiss, a clear or an action of a non-resident notification is `Dismissed`, `CloseNotification` is `CloseRequested`, and a transient popup timing out or the oldest leaving a full history is `Expired`. A popup that isn't transient and times out closes nothing.
- Network: NM's states map as 100 `Connected`, 40 to 90 `Connecting`, 110 `Disconnecting`, 30 and 120 `Disconnected`, anything else `Unavailable`. A failure's reason comes from `StateReason` in state 120 (7 `NoSecrets`, 8 `WifiClientDisconnected`, 9 and 10 `WifiClientFailed`, 11 `WifiAuthTimeout`, 53 `WifiNetworkLost`, else `Unknown`) and is kept until the device enters states 40 to 100; a whole re-read forgets it. Security is the strongest key management an access point offers: Suite-B 192, SAE, PSK, 802.1X on RSN, then PSK and 802.1X on WPA, then OWE; the privacy bit alone is `Wep`, and nothing is `Open`. A cable's link is the carrier bit of `InterfaceFlags`, else `Carrier`.
- Network: saved connections are read only from the first `saved_connections()`, over a bus connection of their own with two match rules, and read again whole on each Settings signal or `Updated`. The first 256 by number are read, a reply over 64 KiB is left out, and at most 1 MiB of memory is kept, counted as what the JSON holds, names included; past that a connection is listed with `settings: null`. A byte array is an array of numbers, a dictionary an object with non-string keys written as their JSON, a structure an array, and a file descriptor or NaN `null`. Secrets are never asked for. Writing settings isn't there: it needs NetworkManager's typed values.
- I/O: a marker may be several bytes and is found across reads; `""` hands over each read, keeping a character a read cut. Lines go to the handler one at a time, with nothing borrowed; one that sets another marker has everything after its own line split at it, and a marker set outside a handler splits what waits on the loop's next turn. A program's lines split at a newline until a handler says otherwise. A line over 1 MiB is dropped, counting only bytes known to belong to it.
- I/O: a file is checked with stat, opened without waiting and checked again, so a pipe, a directory or a device reads as `NotAFile` and never blocks the shell.
- I/O: a socket server listens at once, without making directories. It replaces a socket file nothing listens on and leaves alone, logged, a live socket, a file that isn't a socket or a link. At most 64 connections are open; one past that is closed at once and logged. Each connection's context goes on the loop's turn after it ends. A client that only stops sending has its last line passed on, and its connection stays open for the reply until the config closes it, a write fails, or 5 s pass, so a config that answers closes it. One that hangs up while still sending ends it at once; one that hangs up after it stopped sending keeps its place until a write fails or the 5 s pass, as nothing reads it any more. An accept error that leaves a client waiting rests the listener for 1 s. Stopping closes the listener and every connection, and removes the socket file only if its device and inode are the ones the server made. The file gets the umask's permissions, so keep it under `$XDG_RUNTIME_DIR`.
- Audio: per-channel volumes are rounded as `volume_percent` is, at most `PA_CHANNELS_MAX` channels, and a short channel map is padded with `Unknown`. A per-channel setter needs one percent per channel, each at most 100, or sends nothing.
- Audio: a peak meter is a record stream with `PEAK_DETECT`, 32-bit floats at 25 a second, one frame per fragment, in the target's channel map: a sink through its monitor, a source itself, a stream through its sink's monitor kept to that stream. The target is a memo, so a sink's volume or mute changing doesn't reopen it. A meter opens once its target is listed and closes when its context goes, its target changes or leaves, or the connection drops; a stream closed before the server made it waits for the server and is then ended. A sink's or stream's meter is passive and is corked, showing zeros, while its sink isn't running or its stream is paused; a source's meter keeps recording. Peaks are each channel's loudest sample since the last read, at most 1, NaN as 0, and set only when they change. The link graph isn't read: the PulseAudio protocol has none, and `AudioStream.sink` names a stream's device.

Rules settled while building the `Tray` widget (Phase 4, step 5):

- `Tray::new()` takes `icon_size` (16 by default) and `menu_style`, the menu popup's style (light text on dark gray by default). Both take a value or a closure.
- It is built from `crabshell-ui`'s widgets. `cx.tray()` needs a context, which a keyed child's view gets, so the tray is one such child holding a row of the items. `Prop::shared` lets one property feed many widgets.
- A click opens the menu popup at once, so it grabs the pointer with the click's serial. The popup is clear and empty until the app's layout arrives. It closes once the tray stops following its opening: the app's menu went, a read failed (the first one too), or the app left. `menu_style` is the `Menu`'s style, so submenus get it too.
- When the popup goes, the tray stops following the app's menu, unless the menu was opened again since. `open_menu` numbers each opening for that.
- A widget doesn't know its output's scale, so pixmaps are picked for twice the icon size. An item with no icon shows `image-missing`.

## Crates

One Cargo workspace with seven crates. Each crate owns one area, so an agent can work inside one without touching the others.

| Crate                | Owns                                                                                                                                                                                       | Depends on           | Main external crates                                                                                                    |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `crabshell-reactive` | Signals, memos, effects, owners, flush                                                                                                                                                     | none                 | `slotmap`                                                                                                               |
| `crabshell-ui`       | `Cx`, widgets, styles, layout, text layout, hit testing, focus, invalidation                                                                                                               | reactive             | `taffy`, `cosmic-text`, `fontconfig`, `unicode-segmentation`                                                            |
| `crabshell-render`   | Drawing into pixmaps, damage tracking, glyphs, icons, images                                                                                                                               | ui                   | `tiny-skia`, `resvg`, `png`, `swash`, `zune-jpeg`, `gif`, `image-webp`                                                  |
| `crabshell-wayland`  | Event loop, windows, popups, outputs, pointer, keyboard, text input, clipboard, shm buffers, idle, toplevels, session lock, workspaces, shortcut inhibitors, global shortcuts, focus grabs | reactive, ui, render | `smithay-client-toolkit` (with `xkbcommon`), `calloop`, `wayland-protocols`, `wayland-protocols-wlr`, `wayland-scanner` |
| `crabshell-services` | Workspaces, tray, audio, clock, power, Bluetooth, network, media players, notifications, desktop entries, programs, files, sockets, PAM, greetd, polkit, Hyprland                          | reactive, ui         | `zbus`, `serde_json`, `libpulse-binding`, `jiff`, `calloop`, `serde`, `inotify`, `rustix`, `nonstick`                   |
| `crabshell`          | Public API, `run()`, `prelude`, `cx.persist`, `cx.ipc`, `cx.quantize_colors`, `cx.toplevels`, `cx.session_lock`, `cx.exit`, `cx.window_manager`, `cx.global_shortcut`                      | all of the above     | `serde`, `serde_json`, tracing-subscriber, `signal-hook`                                                                |
| `crabshell-cli`      | The `crabshell` binary with `init`, `run`, `check`, `ipc` and the supervisor                                                                                                               | none                 | `calloop`, `rustix`, `serde_json`, `inotify`, `zbus`                                                                    |

`Cx` lives in `crabshell-ui` and carries a map of services keyed by type. `crabshell-services` adds `cx.sway()`, `cx.audio()`, `cx.tray()`, `cx.clock()` and the services of Phases 9 to 12 through an extension trait, so `crabshell-ui` never depends on services.

### Dependency policy

- Pin every external version once, in the workspace `Cargo.toml` under `[workspace.dependencies]`.
- Add a crate only when it saves more than about 200 lines or implements a protocol or file format. Add it to the table above first.
- Rust crates must be MIT, Apache-2.0, BSD, ISC, Zlib or Unicode licensed. `cargo deny check licenses` enforces this with `deny.toml`, in CI on every push.
- System C libraries (libpulse, fontconfig, libxkbcommon, libpam) are linked dynamically. sctk speaks the Wayland protocol in pure Rust, so libwayland-client isn't linked.
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

### Phase 10 results

Measured on 2026-11-14 with `measure_the_counter_bar`, release builds on one 1920×1080 headless Sway output at scale 1. "Before" is 207887c, built in a second checkout. The overview example is opened over IPC as it starts, with its applications, another program's window and its picture, the wallpaper's colors and `uname`; its first frame counts the wait for that call.

| Metric                                      | Before Phase 10 | After Phase 10 |
| ------------------------------------------- | --------------- | -------------- |
| Counter example RSS                         | 16.2 MiB        | 16.5 MiB       |
| Counter first frame                         | 9.1 ms          | 10.1 ms        |
| v0.1 bar RSS                                | 19.7 MiB        | 19.9 MiB       |
| Desktop example RSS                         | 20.0 MiB        | 20.3 MiB       |
| Overview example RSS, open                  | none            | 19.4 MiB       |
| Idle wakeups, counter, desktop and overview | 0               | 0              |

The measurement starts its shells with a cleared environment, so without the glibc tunable `crabshell run` sets. One run with it gave 16.9 MiB for the counter, 19.8 for the bar, 20.4 for the desktop and 19.3 for the overview, all within the noise, and first frames of 10.1, 15.2, 14.9 and 67.3 ms. What it saves comes later: 41 MiB kept after a second 4K wallpaper without it, 0.4 MiB with it.

### Phase 11 results

Measured on 2026-11-14 with `measure_the_counter_bar`, release builds on one 1920×1080 headless Sway output at scale 1. "Before" is caa8728, built in a second checkout. The lock example is locked over IPC as it starts and nobody types, so PAM never runs; its first frame counts the wait for that call.

| Metric                                            | Before Phase 11 | After Phase 11 |
| ------------------------------------------------- | --------------- | -------------- |
| Counter example RSS                               | 16.7 MiB        | 16.7 MiB       |
| Counter first frame                               | 10.8 ms         | 9.5 ms         |
| v0.1 bar RSS                                      | 19.9 MiB        | 20.2 MiB       |
| Desktop example RSS                               | 20.5 MiB        | 20.3 MiB       |
| Overview example RSS, open                        | 19.4 MiB        | 19.7 MiB       |
| Lock example RSS, locked                          | none            | 24.5 MiB       |
| Idle wakeups, counter, desktop, overview and lock | 0               | 0              |

Every shell now links libpam, about 0.2 MiB. The lock example's extra RSS is mostly its full-screen surface: 7.9 MiB of pixels at 1920×1080, where a bar holds a strip.

### Phase 12 results

Measured on 2026-11-15 with `measure_the_counter_bar`, release builds on one 1920×1080 headless Sway output at scale 1. "Before" is 0c13780, built in a second checkout. Phase 12's services start only when a config asks for them, and no measured example does, so the numbers show what every shell pays.

| Metric                                            | Before Phase 12 | After Phase 12 |
| ------------------------------------------------- | --------------- | -------------- |
| Counter example RSS                               | 16.7 MiB        | 16.7 MiB       |
| Counter first frame                               | 12.8 ms         | 9.1 ms         |
| v0.1 bar RSS                                      | 19.9 MiB        | 20.2 MiB       |
| Desktop example RSS                               | 20.3 MiB        | 20.5 MiB       |
| Overview example RSS, open                        | 19.5 MiB        | 19.8 MiB       |
| Lock example RSS, locked                          | 24.9 MiB        | 24.8 MiB       |
| Idle wakeups, counter, desktop, overview and lock | 0               | 0              |

### Phase 13 results

Measured on 2026-11-15 with `measure_the_counter_bar`, release builds on one 1920×1080 headless Sway output at scale 1. "Before" is 42cce2d, built in a second checkout, and both were measured one after the other. Phase 13's additions start only when a config asks for them, so the changes are within run-to-run noise; the desktop example reads a few more NetworkManager and Bluetooth properties.

| Metric                                            | Before Phase 13 | After Phase 13 |
| ------------------------------------------------- | --------------- | -------------- |
| Counter example RSS                               | 17.0 MiB        | 16.7 MiB       |
| Counter first frame                               | 17.6 ms         | 9.8 ms         |
| v0.1 bar RSS                                      | 20.3 MiB        | 20.4 MiB       |
| Desktop example RSS                               | 20.3 MiB        | 20.9 MiB       |
| Overview example RSS, open                        | 19.7 MiB        | 19.8 MiB       |
| Lock example RSS, locked                          | 24.8 MiB        | 25.0 MiB       |
| Idle wakeups, counter, desktop, overview and lock | 0               | 0              |

### Phase 14 results

Measured on 2026-11-15 with `measure_the_counter_bar`, release builds on one 1920×1080 headless Sway output at scale 1. "Before" is 6ee49eb, built in a second checkout, and both were measured one after the other while other builds ran, so first frames are noisy. Phase 14 adds code that runs only when a config asks for it, and one shared signal per root context.

| Metric                                            | Before Phase 14 | After Phase 14 |
| ------------------------------------------------- | --------------- | -------------- |
| Counter example RSS                               | 16.9 MiB        | 17.0 MiB       |
| Counter first frame                               | 9.9 ms          | 9.9 ms         |
| v0.1 bar RSS                                      | 20.2 MiB        | 20.5 MiB       |
| Desktop example RSS                               | 20.8 MiB        | 20.2 MiB       |
| Overview example RSS, open                        | 19.6 MiB        | 19.7 MiB       |
| Lock example RSS, locked                          | 24.8 MiB        | 24.9 MiB       |
| Idle wakeups, counter, desktop, overview and lock | 0               | 0              |
| Motion example, a spinner frame at scale 2        |                 | 0.2 ms         |
| Motion example idle wakeups over 60 s             |                 | 0              |

### Phase 15 results

Measured on 2026-11-16 with `measure_the_counter_bar`, release builds on one 1920×1080 headless Sway output at scale 1. "Before" is 2ec3613, built in a second checkout, and both were measured one after the other with nothing else building. Phase 15 adds a few fields to every node and style, and code that runs only when a config uses it.

| Metric                                            | Before Phase 15 | After Phase 15 |
| ------------------------------------------------- | --------------- | -------------- |
| Counter example RSS                               | 17.1 MiB        | 17.2 MiB       |
| Counter first frame                               | 9.7 ms          | 9.6 ms         |
| v0.1 bar RSS                                      | 20.5 MiB        | 20.8 MiB       |
| Desktop example RSS                               | 20.9 MiB        | 21.2 MiB       |
| Overview example RSS, open                        | 20.0 MiB        | 20.1 MiB       |
| Lock example RSS, locked                          | 24.9 MiB        | 25.4 MiB       |
| Idle wakeups, counter, desktop, overview and lock | 0               | 0              |
| Control example RSS                               |                 | 22.2 MiB       |
| Control example first frame                       |                 | 23.4 ms        |
| Control example, a fling frame at scale 2         |                 | 6.1 ms         |
| Control example idle wakeups over 60 s            |                 | 0              |

### Phase 16 results

Measured on 2026-11-16 with `measure_the_counter_bar`, release builds on one 1920×1080 headless Sway output at scale 1. "Before" is 9720005, built in a second checkout, and both were measured one after the other with nothing else building. Phase 16 adds a few fields to every style and node, and code that runs only when a config draws with it.

| Metric                                                     | Before Phase 16 | After Phase 16       |
| ---------------------------------------------------------- | --------------- | -------------------- |
| Counter example RSS                                        | 17.4 MiB        | 17.6 MiB             |
| Counter first frame                                        | 9.4 ms          | 9.8 ms               |
| v0.1 bar RSS                                               | 20.8 MiB        | 20.9 MiB             |
| Desktop example RSS                                        | 21.1 MiB        | 21.2 MiB             |
| Overview example RSS, open                                 | 19.8 MiB        | 20.1 MiB             |
| Lock example RSS, locked                                   | 25.0 MiB        | 25.3 MiB             |
| Control example RSS                                        | 22.1 MiB        | 22.3 MiB             |
| Idle wakeups, counter, desktop, overview, lock and control | 0               | 0                    |
| Dashboard example RSS over a 1080p wallpaper, shown        |                 | 36.7 MiB             |
| Dashboard example RSS, hidden                              |                 | 36.1 MiB             |
| Dashboard example first frame                              |                 | 65.0 ms              |
| Dashboard wakeups over 60 s, hidden                        |                 | 0                    |
| A 1080p wallpaper crossfade frame through the shell        |                 | 7.7 ms               |
| RSS around it under the supervisor's mmap threshold        |                 | 29.8, 37.8, 29.8 MiB |

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

After v0.1, the phases bring every feature Quickshell has, so any Quickshell setup can be rebuilt in Crabshell. Phases 7 to 13 follow Quickshell 0.3.2's modules, from its public docs; its source stays unread. Phases 14 to 19 bring what configs take from Qt itself, found by the survey in "Phases 14 to 19: what configs take from Qt". Each phase gets its own plan, exit check and measurements when it starts, and every one keeps the four rules: lighter than Quickshell, idle means asleep, a config is plain Rust, and one bad binding never takes the shell down. The order is a first guess.

| Phase | Area                             | What it brings, by the Quickshell modules it matches                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| ----- | -------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 7     | Windows and input                | Floating windows, layer-shell options (keyboard focus, layers, margins, exclusion), input regions and masks, keyboard input (libxkbcommon), text input, scrolling lists, images, clipping and wrapper widgets (`PanelWindow`, `FloatingWindow`, `PopupWindow`, `Region`, `WlrLayershell`, `WlrKeyboardFocus`, `Quickshell.Widgets`)                                                                                                                                                                   |
| 8     | Animation and effects            | Animations that run only while something moves, gradients, shadows and the compositor's background blur (`EasingCurve`, `BackgroundEffect`), and the GPU backend if the CPU renderer can't keep up                                                                                                                                                                                                                                                                                                    |
| 9     | Desktop services                 | Notifications server, media players, PipeWire nodes and levels, battery and power profiles, Bluetooth, networking, idle monitor and inhibitor (`Services.Notifications`, `Services.Mpris`, `Services.Pipewire`, `Services.UPower`, `Bluetooth`, `Networking`, `IdleMonitor`, `IdleInhibitor`)                                                                                                                                                                                                         |
| 10    | Launchers and system integration | Desktop entries and their actions, processes, files and sockets, an IPC handler driven from the command line, JSON files, color quantizing, screencopy views, toplevel management (`DesktopEntries`, `Quickshell.Io`, `IpcHandler`, `ColorQuantizer`, `ScreencopyView`, `ToplevelManager`), plus two things the rime rebuild lacked: a widget's hover as a signal, and a shadow cut out under translucent glass                                                                                       |
| 11    | Lock screen, greeter and polkit  | Session lock surfaces, PAM authentication, a greetd greeter and a polkit agent (`WlSessionLock`, `Services.Pam`, `Services.Greetd`, `Services.Polkit`)                                                                                                                                                                                                                                                                                                                                                |
| 12    | More compositors                 | Hyprland (workspaces, monitors, toplevels, focus grab, global shortcuts), i3, niri and generic window-manager protocols (`Quickshell.Hyprland`, `Quickshell.I3`, `Quickshell.WindowManager`, `GlobalShortcut`, `ShortcutInhibitor`)                                                                                                                                                                                                                                                                   |
| 13    | Parity check                     | A comparison against every Quickshell 0.3.2 type, with each gap closed or written down with a reason                                                                                                                                                                                                                                                                                                                                                                                                  |
| 14    | Time and motion                  | Timers, animation sequences with delays, loops and an end callback, more easing curves, springs, a per-frame signal, exit transitions for removed children, and translate, scale and rotate (`Timer`, `FrameAnimation`, `SequentialAnimation`, `ParallelAnimation`, `PauseAnimation`, `SpringAnimation`, `SmoothedAnimation`, `RotationAnimation`, `Item.scale`, `Item.rotation`, `Translate`, `Item.transformOrigin`)                                                                                |
| 15    | Layout, pointer and views        | Per-child sizing and alignment, absolute positions, grids, wrapping rows, `visible` and `enabled`, pointer position, drag, long press, horizontal scroll, every cursor shape, focus set from code, a widget's rectangle as a signal, a slider, kinetic scrolling, a scroll bar, rows of different heights and grid views (`QtQuick.Layouts`, `Grid`, `Flow`, `anchors`, `MouseArea`, `DragHandler`, `WheelHandler`, `FocusScope`, `Slider`, `Flickable`, `ScrollBar`, `GridView`, `TransformWatcher`) |
| 16    | Drawing and effects              | A canvas with paths, arcs and dashes, sweep gradients, a radius per corner, tinted icons and image filters, a cached blur of images, masks, shadows that follow the content, animated GIFs, wallpaper transitions and color helpers (`Canvas`, `QtQuick.Shapes`, `ConicalGradient`, `MultiEffect`, `OpacityMask`, `AnimatedImage`, `Qt.alpha`, `Qt.lighter`, `Qt.tint`)                                                                                                                               |
| 17    | Text                             | Alignment, styled spans with the notification markup and links, line height, letter spacing, font features, decorations, elide modes, text measuring, bundled fonts, drag-select, selectable labels and a multi-line editor (`Text.textFormat`, `Text.onLinkActivated`, `font.letterSpacing`, `font.features`, `TextMetrics`, `FontMetrics`, `FontLoader`, `TextEdit`, `TextArea`)                                                                                                                    |
| 18    | System and outside data          | HTTP and remote cover art, folder views, the clipboard and its history, logind's sleep signal and power actions, screen brightness, system stats, niri's IPC, icon fallbacks, locale names and short sounds (`XMLHttpRequest`, `FolderListModel`, `Quickshell.clipboardText`, `Quickshell.iconPath`, `Qt.locale`, `SoundEffect`)                                                                                                                                                                      |
| 19    | Parity remainder                 | Native PipeWire with its link graph, `NMSettings` write and the Wi-Fi scanner, a screen reader through AccessKit, a reload without the blink and a reload a config can ask for (`PwLink`, `PwLinkGroup`, `PwLinkState`, `PwNodeLinkTracker`, `NMSettings`, `Accessible`, `Quickshell.reload`)                                                                                                                                                                                                         |

Phase 7 passed on 2026-11-14, built in the commits from aa5d8c5 to a0b82d8 after a review per step. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the launcher example on headless Sway: an overlay panel that takes the keyboard, with a text field filtering 500 rows in a `ListView` and an about `FloatingWindow`. It matches its goldens at scales 1, 1.5 and 2, typing filters the list within one frame a key, the wheel scrolls it, the compositor closes the floating window, the window's nodes come back to their count, and the open launcher wakes nothing while idle. Gaps left for later phases: no drag or kinetic scrolling and no scroll bar, no drag-select in text fields, popups that never shrink to fit, and no wrapper widgets of their own, since a `Stack` or `Button` with a padded style covers `WrapperItem`, `WrapperRectangle` and `WrapperMouseArea`, and `Style::clip` covers `ClippingRectangle`.

Phase 8 passed on 2026-11-14, built in the commits from 5c301c4 to 1cb98f5 after a review per step. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the effects example on headless Sway: a bar with a translucent gradient that asks the compositor to blur behind it, tabs whose hover color fades in, an indicator that slides under the picked tab with `cx.animate`, and a card with a soft shadow. It matches its goldens at scales 1, 1.5 and 2. Picking a tab draws 15 frames while the slide and the fade run, then one dispatch sleeps for 2 s. Sway lacks the blur protocol, which is logged once. The CPU renderer draws each of those frames in well under a millisecond, so the GPU backend waits. Gaps left for later phases: blur is untested against a compositor that has `ext-background-effect-v1`, one shadow per style and no inset shadows, no dithering, no springs, and layout values animate only through `cx.animate` driving a style's size, which lays the window out each frame. The same day the maintainer's own Quickshell bar, the rime islands, was rebuilt as a Crabshell config and checked by eye on the laptop's SwayFX session, blur included. What it couldn't rebuild was left out: its panels, now playing, network, Bluetooth, battery and keyboard layout, which later phases bring, and two things no phase lists yet: a shadow cut out under translucent glass, and rotating or scaling a widget.

Phase 9 passed on 2026-11-14, built in the commits from 1954fa0 to afe1fbd after a review per step; agents built MPRIS, notifications, Bluetooth and networking in worktrees of their own, and each was reviewed before its fixes went in. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the desktop example on headless Sway with every Phase 9 service started on private buses and a private pipewire-pulse. It matches its goldens at scales 1, 1.5 and 2, a notification sent with `gdbus` shows as a toast and goes when it expires, and then the shell wakes nothing for 10 s. Each service is tested against a fake daemon on a private bus, and audio also against a private PipeWire. Gaps left for later phases: audio level meters and PipeWire's links, saved network connections and connectivity checks, a Bluetooth pairing agent of the shell's own, and notifications lost while a reload swaps the shell. The same day the rime config gained now playing, Wi-Fi, Bluetooth, the battery and toasts, and the maintainer checked it by hand on the laptop against the real NetworkManager, BlueZ, UPower, power-profiles-daemon, VLC and the session's notification bus. Idle, it woke 12 times a minute, each time for a daemon's signal, such as the Wi-Fi strength or the battery, or for the clock. The check found three things: a config must name its icon theme, since the default is `hicolor`; VLC sends no title for a file without tags; and cosmic-text 0.19 keeps the leading space on the last line of wrapped text that ends in an ellipsis. The player's buttons need a widget's hover as a signal, and the islands a shadow cut out under translucent glass, so both go to Phase 10.

Phase 10 passed on 2026-11-14, built in the commits from 40db193 to 69d4e1b after a review per step; agents built the IPC handler, the color quantizer, toplevels, desktop entries and screencopy in worktrees of their own, and each was reviewed before its fixes went in. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the overview example on headless Sway: closed until an IPC call opens it, it lists the applications from desktop entries in private XDG directories, another program's window with a live picture of it, the wallpaper's main colors and what `uname -s` printed. It matches its goldens at scales 1, 1.5 and 2, wakes nothing for 10 s while open, launches an application on a click, keeps it in a JSON file of recent ones, and lists it first when opened again. Gaps left for later phases: no server sockets, since `cx.ipc` covers calls into the shell; an application only D-Bus can start isn't launched, and the Exec parser refuses entries that break the spec's quoting; a desktop-entry directory behind a link swapped later, as on NixOS, isn't followed until a restart; a file is read again when its writer closes it, not while it writes; views of one source don't share a capture, and live views clipped or scrolled out of sight keep capturing; and window views pair by app id and title, so two windows alike may swap.

Phase 11 passed on 2026-11-14, built in the commits from 5d48dfc to 2e3e624 after a review per step; agents built the session lock, PAM and the polkit agent in worktrees of their own, and each was reviewed before its fixes went in. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the lock example on headless Sway: locked over IPC, it matches its goldens at scales 1, 1.5 and 2, shows typing as dots, says a password PAM refused was wrong, and wakes nothing for 10 s while locked. There its PAM service is `other`, which refuses everyone without asking: no test authenticates against a real account, registers a polkit agent on the real system bus or locks the real session. The greeter example logs in through a fake greetd after a refused password and exits with 0, and the lock example killed while locked locks again when started after. Gaps left for later phases: nonstick frees its own copy of an answer without zeroing it; no `pam_setcred` runs, so Kerberos tickets aren't refreshed on unlock; a PAM module that blocks without asking, such as a fingerprint reader, can't be aborted; users only NSS knows, as from LDAP or systemd-homed, must be named, and polkit offers only users in `/etc/passwd`; polkit's setuid helper is looked for only in `/usr/lib/polkit-1`; and `cx.persist` keeps a lock only across a reload, so a lock screen keeps `locked` in a JSON file under `$XDG_RUNTIME_DIR`.

The lock example passed a hand check against the real PAM `login` service on 2026-11-15, on the maintainer's desktop, inside a nested Hyprland 0.57.2 so the live session was never locked. Locked over IPC, it unlocked with the maintainer's password. Killed while locked, it left Hyprland's own locked screen, locked again by itself when started, and unlocked. The polkit agent first failed there: polkit 128 runs its helper as root from `/run/polkit/agent-helper.socket` and no longer makes it setuid, so the helper the agent spawned ran as the user and polkit counted each try as failed. Once the agent used the socket, with the desktop shell's own agent stopped, the maintainer's password authorized `pkexec`. The same path passed `the_real_polkit_authorizes_through_its_helper_socket` against polkit 128 in a throwaway Arch container, with a wrong answer refused and a right one authorized. The greetd client passed `the_real_greetd_logs_in_and_starts_the_session` against greetd 0.10.3 and its PAM in a throwaway Arch container, run by greetd as its greeter: a wrong password refused, the right one authenticated, and the session started as the test user. That run showed greetd splitting a session's command at spaces, now documented on `start_session`. The greeter example itself, with a compositor on a real VT, hasn't met a real greetd.

Phase 12 passed on 2026-11-15 on the laptop alone, built in the commits from 9dbcb59 to 9f07b3b after a review per step; agents built Hyprland's IPC, i3's events and outputs, the generic workspaces, and the shortcuts and focus grab in worktrees of their own, and each was reviewed before its fixes went in, Hyprland's IPC three times. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the hyprland example on headless Sway against a fake Hyprland: it matches its goldens at scales 1, 1.5 and 2, a click on a workspace sends Hyprland the switch in the syntax its config reads, a workspace event repaints the bar, and then the shell wakes nothing for 10 s. Sway's outputs and events, the generic workspaces and the shortcut inhibitor run against headless Sway. The maintainer kept the desktop out of this phase, so its IPC was checked against a fake built from Hyprland's docs and its BSD-3 source, and its focus grab and global shortcuts only for the missing-protocol path and against fakes. The maintainer accepted the `unsafe` that `wayland-scanner` generates in the Hyprland bindings. Gaps left for later phases: no niri service, since Quickshell has none, and niri untested through the generic protocols; workspace deactivate and remove untested, since Sway and labwc refuse them; `ScreenProjection` a method rather than a type; `HyprlandWindow` covered only by `HyprlandToplevel`'s class, title and state; a Hyprland request waiting on another program's full backlog waits for the next event or call; and on Hyprland 0.41 and older a maximized window counts as fullscreen.

The hand check on Hyprland passed on 2026-11-15, on the maintainer's desktop with Hyprland 0.57.2, inside a nested Hyprland with a Lua config, so the live session was left alone. The hyprland example followed switches, new and closed windows, urgency and focus, kept special workspaces out, and woke nothing for 10 s at 16.8 MB. `focus_workspace` switched by number and by name, and a name holding Lua code was sent as a literal name. The three lists matched `hyprctl`. The global shortcut was listed with its description, a held key read `true` until its release and counted once, the focus grab ended on a click in another window, and the shortcut inhibitor kept a bound key from Hyprland until it was turned off. The check found one bug, a long workspace name spilling out of the example's fixed-width button, fixed in 6ae02d1. A `hyprctl dispatch` of `global` sends only a press, never a release, so a shortcut fired that way stays held; that is Hyprland's doing.

Phases 7 to 12 were released on 2026-11-15: every crate at 0.2.0 on crates.io, tagged `v0.2.0`. `crabshell-cli` 0.2.0 still had 0.1 in the template line `init --path` replaces, so `--path` was ignored. The fix went out alone as `crabshell-cli` 0.2.1, tagged `cli-v0.2.1`. The polkit socket fix went out as `crabshell-services` 0.2.1 and `crabshell` 0.2.1, which pins it.

Phase 13 passed on 2026-11-15 on the laptop, built in the commits from ed3a486 to 76a610b. Agents built seven steps in worktrees of their own: the services' status gaps, NetworkManager, I/O, audio, outputs and padding, popups and masks, and Hyprland's surface. Each was reviewed before its fixes went in, and the I/O step's fixes were reviewed again. Its exit check is the features page, which goes through all 149 Quickshell 0.3.2 types: 125 are built, 11 don't apply to a config written in Rust, and 13 are written down with a reason. Of those 13, 8 are partly built, such as `NMSettings` without `write`. The other 5 aren't built: the PipeWire link graph's four types, which the PulseAudio protocol doesn't carry, and `TransformWatcher`, as widget positions aren't exposed. fmt, clippy, the workspace tests and the wayland (71), crabshell (41) and Sway services (4) headless suites pass on the merged result. Hyprland's surface opacity and visible mask then passed a hand check on Hyprland 0.57.2, in a nested Hyprland on the maintainer's desktop. Gaps left for later: peak meters were tested on pipewire-pulse only, not a real PulseAudio; a half-open socket-server client that then hangs up keeps its place until a write fails or 5 s pass. Phase 13 was released on 2026-11-15 as 0.3.0 of every crate, tagged `v0.3.0`. It breaks 0.2 configs in these places: `PamResult::Error` holds a `kind` and a `message`; `Padding` has four sides; `OutputInfo` has new fields and no `Eq`; `Adjust::ALL` includes resizing; `NetworkState` has `Disconnecting`; and several structs have new public fields.

Phase 14 passed on 2026-11-15 on the laptop, built in the commits from 744295b to e544883. Agents built five steps in worktrees of their own: timers, the animation work, transforms, exit transitions and the exit check. Each was reviewed before its fixes went in. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the motion example on headless Sway with a private pipewire-pulse and session bus. With its OSD held up and a toast shown, it matches its goldens at scales 1, 1.25, 1.5, 1.75 and 2. A `pactl` volume change slides the OSD up from about 0.9 scale; it hides 1.5 s after the last change through one restarted `cx.after`, and once its slide-out ends it lets the pointer through. A program run over IPC turns the bar's spinner, which finishes its turn and rests once the program exits. A notification slides in and, once it expires, slides out over its exit, and everything it made is freed. Then the shell wakes nothing for 60 s. While the spinner turns at scale 2, a frame takes 0.2 ms to draw in a release build, repainting 38×38 pixels. A panicking timer is logged with its place while the shell runs on, which `a_panicking_timer_is_logged_with_its_place_and_the_shell_runs_on` checks. fmt, clippy, the workspace tests and the wayland (72) and crabshell (44) headless suites pass on the merged result. Gaps left for later phases, most of them found by writing the motion example: a timer runs from the moment it is made, so one that waits for a first change is stopped by hand; an animation has one transition, so a different curve in and out takes two; an exit's length is kept equal to the item's own animation by hand; `cx.spring` has no `running()`; `cx.process` can't wait idle, so a program run on demand needs a context per run; a leaving child keeps its space, so the ones after it jump when it goes, which displaced transitions in Phase 15 fix; an exiting list inside a leaving item sees only its own leaving; a screencopy view inside a transform captures at its laid-out size; the default spring's numbers weren't checked against Material's docs; and Elastic and Bounce curves stay out. Phase 14 was released on 2026-11-15 as 0.4.0 of every crate, tagged `v0.4.0`. It breaks 0.3 configs that build a `Transition` from its fields or match every `Easing`, and makes children that overflow their parent clickable.

Phase 15 passed on 2026-11-16 on the laptop, built in the commits from 18cec9b to 14f16e7. Agents built six steps in worktrees of their own: layout, pointer input, displaced transitions with drag and drop, the slider with kinetic scrolling, the list views and the exit check. Each was reviewed before its fixes went in, and the list views and the exit check started on the step before them and were rebased once it landed. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the control example on headless Sway with a private pipewire-pulse and session bus. With ten notifications sent and its date pinned, it matches its goldens at scales 1, 1.25, 1.5, 1.75 and 2. A virtual pointer drags the volume slider's handle to 60% and then past the track's end to 75%, and `pactl` agrees within a percent. A touchpad fling carries the notification list on after the fingers lift, to its end, where it springs back and rests; then the shell wakes nothing for 60 s. In process at scale 2, the highlight slides to the hovered row and stops, and hovering a row cut off at the bottom scrolls it whole and no further. A fling frame takes 6.1 ms to draw at scale 2 in a release build, laying the list out and repainting its 656×360 pixels each frame. fmt, clippy, the workspace tests and the wayland (79), crabshell (47) and Sway services (4) headless suites pass on the merged result. Gaps left for later phases: a drag reaches only buttons in its own window, since a drag between the shell's windows needs Wayland's own drag and drop; a fling lays the window out each frame, since the offset is a layout property, which costs far more than a transform; `geometry` gives the laid-out rectangle, without paint transforms or the window's place on its output; a scroll view scrolls one axis at a time; a mouse drag scrolls a list as a finger does, as Qt's `Flickable` does, so a click that moves 10 px along a list scrolls; a press on a scroll bar's track pages once and doesn't repeat, and a scroll-bar drag doesn't count as moving, so a `current` change during it scrolls; `GridView` has no horizontal mode and no cells of different sizes; a highlight's size jumps while only its place slides; the cursor doesn't change during a drag; touch is tested by unit tests only, as headless Sway has no virtual touch device; a monotonic timer pauses in a suspend, so the control example shows yesterday after a suspend over midnight until its timer fires; and the goldens show no highlight, since the seat has no pointer when they are taken. Phase 15 was released on 2026-11-16 as 0.5.0 of every crate, together with Phase 16, tagged `v0.5.0`. It breaks 0.4 configs in these places: `Spacer` is no longer a unit struct, `Style` and `Drag` have new public fields, `PointerButton` and `CursorShape` have new variants, `ScrollDelta` and `Window::pointer_scroll_end` are gone, `row_height` is a minimum and an estimate, and a mouse drag of a scroll view's content scrolls it.

Phase 16 passed on 2026-11-16 on the laptop, built in the commits from 142a219 to 1ea8688. Agents built six steps in worktrees of their own: the canvas, style drawing, image filters and GIFs, masks and content shadows, wallpaper transitions and the exit check, the last four rebased onto the steps that landed before them. Each was reviewed before its fixes went in, and the reviews found a GIF frame off its canvas that crashed the renderer and a GIF cap that let one outside picture keep 256 MiB, both fixed before merging. Its exit check, in `crates/crabshell/tests/headless.rs`, runs the dashboard example with pinned battery, CPU samples, player and pictures and its icons from a fixture theme, and matches its goldens at scales 1, 1.25, 1.5, 1.75 and 2. Hidden, the dashboard wakes nothing for 60 s and frees its CPU sampling with everything it made; shown, it samples once a second, and opening it again keeps its windows and history. A 1920×1080 wallpaper crossfade through the shell takes 7.7 ms a frame at scale 1 in a release build, over 48 frames; under the supervisor's mmap threshold the shell rests at 29.8 MiB, holds both pictures at 37.8 MiB during it and is back at 29.8 MiB after. fmt, clippy, the workspace tests and the wayland (81), crabshell (51) and Sway services (4) headless suites pass on the merged result. Gaps left for later phases: a GIF's frames and a 1080p blur (50 to 80 ms) are decoded and worked on the loop's thread at the first draw, so a large one stalls the shell once; a GIF hidden while it waits wakes the loop once more; a content shadow costs a blur each time its content repaints, 35 ms for a full draw of a 400×300 card with a blur of 40 at scale 2, so a box takes a box shadow; a dashed border's square corners other than the first can lose part of their outer half-width; one large corner beside small ones stays at half the shorter side, smaller than CSS draws it; a source an image transition can't load still runs its clock for the transition's length; a picture a transition leaves that is also a mask image can be decoded twice; a crossfade through the shell takes twice its time in render alone, likely the copy into the shm buffer, unprofiled; a shell run without the supervisor keeps its RSS high-water mark after a wallpaper changes, 52 MiB against 30; the dashboard with a 1080p wallpaper uses 36.7 MiB, past the v0.1 bar's 30 MB, its picture and window buffer about 16 MiB of it; and remote album art waits for Phase 18's HTTP. Phases 15 and 16 were released together on 2026-11-16 as 0.5.0 of every crate, tagged `v0.5.0`. Besides Phase 15's breaks, `Style::radius` holds a `Radii`, `Style`, `Shadow` and `Drag` have new public fields, `GradientShape` and `Content` new variants, and a clip's cut corners no longer pass the pointer to its children.

### Phases 14 to 19: what configs take from Qt

Phase 13 matched Quickshell's own types, but a Quickshell config is QML, and most of what it draws and animates comes from Qt. On 2026-11-15 a survey counted what five public configs use: caelestia's shell, end-4's illogical-impulse, noctalia-shell 4.7.7 (its last release on Quickshell, before it went native), DankMaterialShell and the maintainer's rime. It read Qt's and Quickshell's public docs and the configs' QML, never Quickshell's source. Phases 14 to 19 bring what they use, most used first. Each item keeps the four rules: a timer, a loop or a kinetic scroll is in the event loop only while it runs and gets an idle test, and every new callback runs inside `catch_unwind`. The exit checks below are first guesses, settled when each phase starts.

**Phase 14, time and motion.** Done; "Phase 14 passed" above says what was built and what was left.

- `cx.after(duration, f)` and `cx.interval(duration, f)`, as handles that stop and restart, on a calloop `Timer` that is in the loop only while it runs. All five configs use `Timer`, for an OSD that hides itself, tooltip delays, debouncing and polling.
- Sequences: a delay on a `Transition`, a `running()` signal an effect can follow where Qt has `onFinished`, and cycles that stop when asked, for spinners and pulses.
- More curves: a multi-segment spline usable in a `const`, which Material 3's emphasized curves need, plus Back, Sine, Expo, Quart, Quint and Circ. A spring that keeps its velocity when the target moves.
- A per-frame signal on the animation clock, as `FrameAnimation` gives.
- Exit transitions: `children_keyed` keeps a removed child painted while it animates out, then disposes it.
- Translate, scale, rotate and their origin as paint properties of `Style`. The subtree draws into a layer, as opacity does, and is composited through a tiny-skia `Transform`. Damage is the transformed bounds, and hit testing maps the pointer back. A slide then no longer lays the window out each frame.
- Exit check: an OSD that slides and scales in on a volume change and hides 1.5 s later through `cx.after`, a spinner while a process runs, and a dismissed notification sliding out. Goldens at all five scales, nothing woken for 60 s afterwards, panicking timer callbacks logged with their paths, and the frame time of a rotating subtree at scale 2.

**Phase 15, layout, pointer and views.** Done; "Phase 15 passed" above says what was built and what was left.

- Layout through taffy, which already builds with its grid feature: per-child grow, shrink and basis, min, max and preferred sizes, `align_self`, absolute positions inside a `Stack` with `z`, a `Grid`, and rows that wrap. `visible` that frees the space, `enabled` that turns off a `Button`'s handlers, hover style and Tab stop, and right-to-left rows.
- Pointer: the position as a signal the widget writes back, release, double-click, long press, drag with a pointer grab, horizontal and touchpad scroll with pixel deltas, the back and forward buttons, every `wp_cursor_shape_v1` shape, and a touch tap as a click.
- Focus set from code, as `TextInput` has, and a widget's rectangle as a signal written after layout, which covers `TransformWatcher` and `QsWindow`'s item mapping.
- Views: a `Slider` that writes back a `Signal<f32>`, kinetic scrolling with overshoot that sleeps at rest, a scroll bar, a highlight that follows the current row, `ListView` rows of different heights, horizontal lists, and a grid view for app and wallpaper pickers. Displaced transitions, so the children after one that is added or leaves move into place instead of jumping. Drag and drop inside the shell comes once drag works.
- Exit check: a control-center example on headless Sway where a slider dragged by a virtual pointer moves the volume and `pactl` agrees, a calendar grid, wrapping chips and a highlight that follows the hovered row. Goldens at all five scales, and a kinetic scroll that comes to rest, then nothing woken for 60 s.

**Phase 16, drawing and effects.** Done; "Phase 16 passed" above says what was built and what was left.

- A `Canvas` widget whose paint closure is tracked and runs inside `catch_unwind`, drawing paths, arcs, cubic curves and dashed strokes with caps and joins, all in tiny-skia. Progress rings, graphs and audio visualizers need it.
- Sweep gradients, which tiny-skia 0.12 has, a radius per corner and dashed borders.
- Image filters on the cached image: tinting symbolic icons, brightness, contrast and saturation, and a blur of a still image such as album art or a lock wallpaper. A live blur of changing widgets waits, since it costs CPU each frame and grows the damage by its radius.
- Masks from a shape or another image, and a shadow that follows the content's alpha, for text and icons.
- Animated GIFs that tick only while shown, with a cap on frames times size.
- Built-in wallpaper transitions: crossfade, wipe, circle reveal and pixelate.
- Color helpers as `const fn`s: alpha, mix, lighter, darker and HSL.
- Exit check: a dashboard example with a battery ring, a CPU graph, tinted symbolic icons and a blurred album-art card, with goldens at all five scales. RSS and first frame against the base commit, and the frame time of a 1080p crossfade, whose two decoded images hold about 16 MB.

**Phase 17, text.**

- As `const` `Style` fields: alignment of wrapped text, line height, letter spacing, font features such as tabular digits, italic, underline and strikethrough, and eliding at the start or the middle. cosmic-text 0.19 has all of them.
- Styled spans, with a parser for the notification spec's markup subset (`b`, `i`, `u`, `a` and `img`), never general HTML, and links that call a handler.
- `cx.measure_text(text, font)`, as `TextMetrics` and `FontMetrics` give, and fonts bundled in the config with `include_bytes!`.
- Drag-select in text fields, selectable labels, and a multi-line editor with input methods.
- Exit check: a notification center showing spec markup with a link that opens, a clock with tabular digits whose width holds still in its goldens, a multi-line editor that takes input-method text, and a first frame within 5 ms of the base commit with a bundled font.

**Phase 18, system and outside data.**

- HTTP GET off the loop thread, with size caps and timeouts, and remote cover art (MPRIS `artUrl` over https) fetched into a cache file. All five configs fetch the weather, most through curl, which `cx.process` already runs. The TLS crate's size and license are checked first.
- A folder view on the existing inotify, for wallpaper pickers.
- The clipboard for configs, read and written over `wl_data_device`, and its history over `ext-data-control-v1`.
- logind's `PrepareForSleep`, so a lock screen locks before suspend, and its power actions. Screen brightness, read from sysfs and set through logind's `SetBrightness`.
- CPU, memory and temperature stats, read only while a config follows them.
- niri's IPC event stream, which noctalia's Quickshell fork and DankMaterialShell read.
- Icon fallbacks and a check for an icon in the theme, as `Quickshell.iconPath` with a fallback and `hasThemeIcon` give.
- Month and day names for the locale, if a pure-Rust locale-data crate passes the license and size checks, and short sounds through the libpulse already linked.
- Exit check: each runs against a fake, such as a local HTTP server for weather and cover art, a fake logind whose `PrepareForSleep` locks the lock example, and a fake niri socket. An idle test for each new wakeup source, and tests for the size caps.

**Phase 19, the parity remainder.**

- Native PipeWire, which brings the link graph, screen-share video nodes and the five types the features page marks as not built, measured against libpulse's RSS. That settles the open question under Risks.
- `NMSettings` write, the Wi-Fi scanner while a config asks for it, and a wired device's own network.
- A screen reader through AccessKit over AT-SPI, if `accesskit_unix` can share zbus on calloop.
- A reload that starts the new shell before stopping the old one, so no frame misses the bar, and a reload a config can ask for, as `Quickshell.reload` does.
- Exit check: the features page shows no type as not built, Orca reads a bar's button labels on a real session by hand, a headless screenshot sequence shows a reload with no frame missing the bar, and native PipeWire's RSS is measured against libpulse.

Left out, with the reason:

- User GLSL shaders (`ShaderEffect`, which caelestia and DankMaterialShell use mostly for wallpaper transitions). A CPU renderer can't run them, and the later vello backend wouldn't take them either. Phase 16's built-in transitions cover the common case.
- Video (`MediaPlayer`, `VideoOutput`). It needs ffmpeg or GStreamer, both LGPL C libraries, CPU color conversion each frame, and RSS far past 30 MB. mpvpaper plays video wallpapers.
- Perspective transforms (`Matrix4x4`), since the renderer is 2D affine.
- Elastic and Bounce curves, `PathAnimation`, `AnimationController`, `PinchHandler`, `PathView`, `SwipeView` and `TableView`, which one of the five configs uses at most.
- What a Rust config already has. `States` and `PropertyChanges` are an enum and a `match`. `Qt.callLater` is the coalescing each flush does. `ListModel`, `DelegateModel` and `ObjectModel` are a signal holding a `Vec`. `layer.enabled` is what opacity and clipping already do. `WebSocket`, `Qt.labs.settings`, `qsTr`, `Qt.md5` and `StandardPaths` are crates and std. `Qt.openUrlExternally` is `xdg-open` through `exec_detached`. Controls' `Switch`, `CheckBox`, `ComboBox`, `TabBar` and `StackView` are a `Button`, `PopupWindow`, `Menu` or `children_keyed` with styles.
- Variable-font axes other than weight, such as Material Symbols' fill, since cosmic-text 0.19 sets only the weight axis.

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
| No accessibility in v0.1                  | Screen readers can't read the shell                                         | Add AccessKit (AT-SPI) in Phase 19                                                                                                   |

Open questions:

- [x] Project name: Crabshell.
- [ ] Which other wlroots compositors to test (river, labwc, Wayfire), and which of them implement `ext-workspace-v1`.
- [ ] Should a reload start the new shell before stopping the old one, to remove the blink? That needs a fix for both bars reserving space at once. Phase 19 tries it.
- [ ] Stay on libpulse after v0.1, or switch to native PipeWire? Phase 19 measures native PipeWire against libpulse.
- [x] When to add Hyprland and niri, and in which order. Hyprland came in Phase 12, and niri's IPC is planned for Phase 18.
- [ ] Should the shell exit with status 1 when `f` panics for every output at startup? Today it keeps running with no bar, so Phase 5's fallback to last-good after three crashes never fires for that config.
- [x] Install cargo-deny and add a deny.toml. Done in Phase 6, and CI runs it.

## Sources

- [Quickshell source](https://github.com/quickshell-mirror/quickshell) at commit `9b0f339` from 2026-10-21, counted locally. About 62k lines of C++ exposing 241 QML types, licensed LGPL-3.0.
- [Quickshell 0.3.2's type list](https://quickshell.org/docs/v0.3.2/types), from its public docs, read on 2026-11-13 for "Beyond v0.1".
- Everything else comes from memory and wasn't checked against current docs: crate names and APIs, protocol versions, Sway commands and toolchain behavior. Phase 0 checked the stack's crates against their source and found four changes: sctk 0.21 uses delegate_dispatch2! and has no fractional-scale helper, cosmic-text 0.19 changed set_text, and zbus 6 replaced async-io with zruntime. Pinned versions are in the repo's Cargo.toml.
