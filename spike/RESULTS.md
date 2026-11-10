# Phase 0 spike results

Measured 2026-11-10 with the release build on the live SwayFX session.

Every check the spike set out to make passed. The bar starts in about 18 ms and holds about 13 MiB of RSS. It wakes up zero times while idle. One zbus call ran correctly on calloop's futures executor with no tokio. Two things were not tested here: the `closed` event and scales other than 1.0 (see "Not verified"). Quickshell drawing the identical bar uses 9.5 times the RSS and takes 10 times as long to its first frame (see "Quickshell baseline").

## Results

| Metric | Run 1 | Run 2 | Run 3 | Median | Design target |
|---|---|---|---|---|---|
| `main()` to first commit with a buffer | 17.71 ms | 17.50 ms | 18.99 ms | **17.71 ms** | ≤ 100 ms |
| VmRSS 10 s after the first frame | 13,580 KiB | 13,420 KiB | 13,608 KiB | **13.3 MiB** (13,580 KiB) | ≤ 30 MB, ≤ ¼ of Quickshell |
| Idle wakeups over 5 s, all threads | 0 | 0 | 0 | **0** | 0 with no clock |
| `fonts ready` (part of startup) | 15.51 ms | 15.30 ms | 16.52 ms | 15.51 ms | |
| D-Bus `ListNames` on calloop's executor | 2.20 ms, 46 names | 2.11 ms, 46 names | 2.41 ms, 46 names | 2.20 ms | works |

Each run was `timeout 15 target/release/crabshell-spike`, with stderr captured. All three exited through the SIGTERM handler ("event loop stopped, exiting cleanly"). A separate SIGINT test exited with code 0 through the same path.

**Quickshell's running RSS: 230,280 KiB (224.9 MiB), 52 threads.** That is the user's full config, not a like-for-like baseline. It is Quickshell 0.3.2 (`qs -c rime -n`, PID 2728, up 5 h 49 min), read from `/proc/2728/status`. The like-for-like comparison, Quickshell drawing the spike's exact bar, is in the next section.

## Quickshell baseline

**Verdict: GO.** The spike's RSS is 10.5 % of Quickshell's for the same bar (13.2 vs 125.2 MiB). The design's target is at most 25 %.

`baseline/quickshell/shell.qml` draws the spike's bar in Quickshell 0.3.2: one `PanelWindow` on eDP-1, top/left/right anchors, 30 px tall, exclusive zone 30, colour #16181d, and a `Text` "Crabshell" in sans-serif at 15 px, #e6e8ee, 8 px from the left. There are no services and no animations. It ran as a second instance with `qs -p baseline/quickshell`, alongside the user's `qs -c rime -n`, which kept running untouched.

Quickshell draws with Qt's threaded render loop on OpenGL through EGL: GPU dmabuf buffers, its default. I didn't force Qt's software backend.

Median of 3 runs each, interleaved (Quickshell, spike, Quickshell, …), measured by the same script:

| Metric | Quickshell | Spike | Spike ÷ Quickshell |
|---|---|---|---|
| **RSS** (headline) | **128,196 KiB (125.2 MiB)** | **13,524 KiB (13.2 MiB)** | **10.5 %** |
| PSS | 57,899 KiB (56.5 MiB) | 6,359 KiB (6.2 MiB) | 11.0 % |
| Private_Dirty | 31,036 KiB (30.3 MiB) | 1,364 KiB (1.3 MiB) | 4.4 % |
| Threads | 20 | 2 | |
| Idle wakeups over 5 s, all threads | 0 | 0 | |
| Launch to first frame | 219.35 ms | 21.67 ms | 9.9 % |

Per run, Quickshell then spike:
- RSS: 128,284 / 128,196 / 128,028 KiB, and 13,524 / 13,520 / 13,608 KiB.
- PSS: 57,927 / 57,899 / 57,848 KiB, and 6,359 / 6,346 / 6,465 KiB.
- Private_Dirty: 31,036 / 31,056 / 31,016 KiB, and 1,360 / 1,364 / 1,364 KiB.
- First frame: 219.35 / 222.03 / 217.96 ms, and 21.67 / 21.74 / 19.72 ms.

PSS splits shared pages among the processes that map them. The second `qs` shares its Qt and Mesa libraries with the user's running `rime` instance, which lowers its PSS. RSS doesn't depend on what else is running, so it is the headline number. Private_Dirty is memory only that process uses.

**How each run was measured.** The same script, `baseline/harness.py`, ran both programs (see "How to rerun"):
1. Take the wall-clock time just before the fork/exec.
2. Find the first frame from the program's own log.
3. Count voluntary context switches over all threads from 4 s to 9 s after the first frame (the method under "Idle wakeups").
4. At 10 s after the first frame, read `/proc/<pid>/smaps_rollup` (Rss, Pss, Private_Dirty) and the `Threads:` line of `/proc/<pid>/status`.
5. Send SIGTERM at 15 s after launch, like `timeout 15`.

The wakeup window ends before 10 s so it can't catch the spike's one-shot "VmRSS after 10 s" timer, which is measurement code, not bar behaviour.

**How first frame was measured.** It is the time from launch to the timestamp of a log line each program writes right after committing its first buffer:
- **Spike:** its "first frame committed" line, microsecond precision.
- **Quickshell:** run with `--log-times --log-rules 'qt.scenegraph.time.renderloop=true'`; the first "frame rendered … swap=N" line with N ≥ 1, millisecond precision. Qt logs an empty "frame rendered in 0ms … swap=0" pass before the real frame, with no buffer commit behind it. On three trial runs with `WAYLAND_DEBUG=1`, the chosen line matched the first `wl_surface.attach` + `commit` to within 1 ms.
- **Why not `WAYLAND_DEBUG`:** I first timed both from the protocol log. wayland-rs writes each debug line unbuffered in several pieces, which pushed the spike's own first frame from about 18 to about 41 ms, an overhead the two libraries don't share. So the final runs don't set it.
- **Comparing with the table at the top:** in the final runs the spike's own "start of `main()` to commit" was 18.35 ms (median). The 21.67 ms here adds about 3 ms of exec, dynamic linking and runtime setup before `main()`. Both columns of the table above include that.

**Screenshot.** `baseline/screenshot.png` holds the same 30 rows (y = 48–77) as `spike/screenshot.png`. They match: same background, font, size, colour and left edge, and the text spans x = 8–72 in both, so the glyph advances are identical. Two differences show up at 8× zoom, both rendering rather than layout:
- Qt uses subpixel (RGB) antialiasing, following the system's fontconfig setting, where the spike uses grayscale.
- Qt's baseline sits 1 px higher, with faint antialiasing rows above and below. The glyphs are the same height: 11 strong rows in both.

Quickshell doesn't handle SIGTERM, so each run ended through the default signal action (exit status −15). The spike exited with code 0.

## Machine and session

| | |
|---|---|
| Machine | AMD Ryzen 5 7535U (6 cores, 12 threads), 14.8 GiB RAM, CPU governor `powersave` |
| OS | Gentoo Base System 2.18, kernel 6.18.60-gentoo-dist-bin |
| Compositor | SwayFX 0.6, based on sway 1.12.0 |
| Output | eDP-1, 1920×1080 at 60 Hz, **scale 1.0** |
| Scaling path taken | fractional: wp_fractional_scale_v1 and wp_viewporter were both offered; preferred scale 120/120 |
| Configure received | 1920×30 logical, buffer 1920×30 |
| Rust | rustc 1.99.1 (latest stable, checked with `rustup check`), edition 2024 |
| System libraries (pkg-config) | wayland-client 1.26.0, fontconfig 2.18.3, libpulse 17.0, xkbcommon 1.13.2 |
| Linked at runtime (`ldd`) | libfontconfig (+ freetype, expat, png16, z, bz2), libc, libm, libgcc_s. Not libwayland-client, not xkbcommon, not libpulse. |
| Fonts | 2,340 font files installed; `sans-serif` resolves to Noto Sans, `/usr/share/fonts/noto/NotoSans-Regular.ttf` |

## Crate versions

Pinned with `=` in `[workspace.dependencies]`. `Cargo.lock` pins the rest.

| Crate | Version | Features |
|---|---|---|
| smithay-client-toolkit | 0.21.1 | `calloop` only (defaults off, so no xkbcommon) |
| calloop | 0.14.5 | `executor`, `signals` |
| tiny-skia | 0.12.0 | `std`, `simd` (no `png-format`) |
| cosmic-text | 0.19.0 | `std`, `swash` (no `fontconfig`, which is fontdb's own config-file parser) |
| fontconfig | 0.11.0 | default (links libfontconfig through yeslogic-fontconfig-sys 6.0.1) |
| zbus | 6.2.1 | `default-rt`, `proxy` |
| tracing | 0.1.44 | default |
| tracing-subscriber | 0.3.23 | `fmt`, `std` (no ANSI colours, since logs go to files) |

Notable transitive versions: wayland-client 0.31.15, wayland-backend 0.3.17, wayland-protocols 0.32.13, calloop-wayland-source 0.4.1, fontdb 0.23.0 (cosmic-text's version; 0.24.0 exists), swash 0.2.10, harfrust 0.5.2, zruntime 0.2.2. The normal dependency graph has 104 crates, the spike included. cosmic-text pulls two versions each of skrifa, read-fonts and font-types.

## Screenshot

`spike/screenshot.png` holds the bar's 30 rows, grabbed with `grim -g "0,48 1920x30"`.

The bar sits at y = 48–77, not y = 0. Quickshell's bar is also a top-layer surface with an exclusive zone, so Sway placed the spike below the 48 px it reserves. I found the rows by scanning a capture for the bar's background colour.

What the pixels show:
- The background reads back as exactly #16181d. That proves the red/blue swap described below gives the right byte order.
- The text starts at x = 8, matching the padding, and spans y = 10–20, centred in the 30 px bar.
- Horizontal edges (baseline, x-height, the tops of b, h and l) fall on single crisp rows.
- Vertical stems are often split across two grey columns. That comes from grayscale antialiasing with cosmic-text's subpixel x positioning. It is sharp, not blurry, but worth comparing with Qt's text, per the design doc's "Text looks worse than Qt's" risk.

## Did zbus work on calloop's executor?

**Yes, with no tokio.** `zbus::Connection::session()` and then `fdo::DBusProxy::list_names()` ran as one `async fn`, scheduled on `calloop::futures::executor`. It returned 46 names in about 2.2 ms, which includes connect, auth and `Hello`.

zbus 6 no longer uses async-io. Its default runtime is zruntime. When a zbus future is polled by a foreign executor and no thread is inside zruntime's `block_on`, zruntime starts one helper thread, named `zruntime`. That thread watches the socket and wakes the future, and calloop's executor polls it again on the main thread. This is the "zbus does its I/O on its own thread" assumption in the design doc. The helper thread lives as long as a connection does.

The spike keeps the connection open after the call, so the idle measurement covers it. The helper thread recorded 0 wakeups in every window.

## Where the time and memory go

**Startup.** About 15.5 of the 17.7 ms is `Text::new`, and fontconfig's `FcInit` dominates it: it parses the config and maps the caches for all 2,340 fonts. `fc-match sans-serif` alone takes 18–28 ms here, after subtracting about 4 ms of process spawn. After fonts, the Wayland setup, first configure, painting and commit take about 2.2 ms.

The governor is `powersave`. One run made straight after a build, with the CPU still clocked up, had fonts ready at 8 ms and the first frame at 9.1 ms. The 100 ms target has plenty of room either way. If startup ever matters more, fontconfig could run on a thread while Wayland connects.

**RSS.** From `/proc/<pid>/smaps` of a run at 13,560 KiB: PSS was 6,381 KiB and Private_Dirty only 1,368 KiB. Most RSS is clean, shareable file pages:

| Mapping | RSS |
|---|---|
| spike binary (7.5 MB file, unstripped) | 4,432 KiB |
| fontconfig cache files (`*.cache-12`) | 4,364 KiB |
| libc, libm, ld.so, libgcc_s | 2,240 KiB |
| libfontconfig, libexpat, libfreetype, libpng16, libz, libbz2 | 1,060 KiB |
| heap | 764 KiB |
| NotoSans-Regular.ttf (mmapped by fontdb) | 288 KiB |
| shm buffer (memfd) | 228 KiB |
| stack, other anonymous memory, vdso | 184 KiB |

The fontconfig caches stay mapped because the fontconfig crate never calls `FcFini`. Its source says older fontconfig versions crash when it is called more than once. Releasing them would need FFI, which means `unsafe`.

## Idle wakeups: how they were measured

Neither `perf` nor `strace` is installed, and I didn't install system packages. Instead a small script snapshots `/proc/<pid>/task/<tid>/status` and `/proc/<pid>/task/<tid>/schedstat` for every thread, 2 s after launch and again 5 s later. `baseline/harness.py` counts wakeups the same way.

It counts `voluntary_ctxt_switches`: each time a thread blocks (in `epoll_wait`, a futex, anything) and then wakes, this goes up by one. It is a superset of epoll wakeups, so 0 is a strong result. Both threads also showed 0 involuntary switches, 0 timeslices and 0 µs of CPU in every window.

The one-shot 10 s RSS timer fires outside the window. With `perf`, the design doc's command is `perf stat -e 'syscalls:sys_exit_epoll_wait' -p <pid> -- sleep 5`.

## What I did differently from the instructions, and why

1. **Screenshot region.** I used `0,48 1920x30` instead of `0,0`, because Quickshell's bar holds the top 48 px (see Screenshot).
2. **Wakeups via `/proc` counters.** perf and strace are not installed (see above).
3. **Kept the D-Bus connection open** after `ListNames` instead of dropping it. A real bar keeps its session connection, so this checks that an open zbus connection stays idle.
4. **No repaint on a configure that changes nothing.** Sway sends a second 1920×30 configure about 1 ms after the first frame. The first build redrew for it. The final build only commits, which applies the ack sctk already sent, and logs "configure with unchanged size". The bar still draws only on configure or scale change, just less often.
5. **Pixel format.** I used `Argb8888`, the format every compositor must support, and swapped red and blue in the colours I paint. wl_shm stores Argb8888 as B,G,R,A bytes in memory, while tiny-skia writes R,G,B,A. Blending treats each channel separately, so the swap is exact, and drawing stays zero-copy with no conversion pass. The alternative, `Abgr8888`, is optional for compositors.
6. **Pure-Rust Wayland backend.** sctk by default uses wayland-rs's Rust implementation of the wire protocol, so libwayland-client is not linked, although the design doc expects it to be. This is fine for CPU drawing. A future wgpu/EGL backend will need sctk's `system` feature, which switches to libwayland.
7. **Default features turned off** wherever the spike doesn't use them (feature table above). xkbcommon is off because v1 has no keyboard input.
8. **Two extra log lines.** "fonts ready N ms", to split startup time, and "first configure" with the output name.
9. **Locale hardcoded to `en-US`** for cosmic-text, because `FontSystem::new_with_locale_and_db` needs one. Detecting it would need another crate.
10. **Crate APIs differ from the prompt's from-memory versions.** I checked them against the published source of each pinned version, which is the code docs.rs renders:
    - sctk 0.21 replaced every `delegate_*!` macro except `delegate_registry!` with one `delegate_dispatch2!`.
    - sctk acks layer configures itself, before calling the handler.
    - sctk has no fractional-scale helper, so the spike binds `wp_fractional_scale_manager_v1` and `wp_viewporter` itself.
    - cosmic-text 0.19's `Buffer::set_text` no longer takes the `FontSystem` and gained an `alignment` argument. Shaping now waits until `shape_until_scroll`.
    - fontconfig 0.11's `find` returns a `Result`, and its `name` is the full name ("Noto Sans Regular"), so the spike takes the family name from fontdb after loading the file.
    - zbus 6 runs on zruntime, not async-io.
11. **Added `.gitignore`** (`/target`), so `git status` stays readable.
12. **Nothing committed**, as asked: `git init` only.

## Not verified

- **The layer surface's `closed` event** is handled (it logs and stops the loop, the same path SIGINT and SIGTERM take), but it never fired. Triggering it on the live session means disabling your only output.
- **Scales other than 1.0.** I tried a separate headless Sway (`WLR_BACKENDS=headless`, scale 1.5). The command sandbox SIGKILLed it about 1 s after start every time, so neither test ran. I removed the stale sockets it left in `/run/user/1000`.

To run both yourself:

```sh
mkdir -p target/measure
printf 'xwayland disable\nswaybg_command -\noutput HEADLESS-1 resolution 1280x720 scale 1.5\n' > target/measure/headless.conf
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman sway -c target/measure/headless.conf &
# Note which wayland-N socket appeared in $XDG_RUNTIME_DIR, then:
WAYLAND_DISPLAY=wayland-N target/release/crabshell-spike &   # expect buffer 1280x45, scale 1.5
WAYLAND_DISPLAY=wayland-N grim -o HEADLESS-1 target/measure/headless.png
swaymsg -s $XDG_RUNTIME_DIR/sway-ipc.$UID.<sway pid>.sock output HEADLESS-1 unplug   # expect "compositor closed the layer surface, exiting"
```

## How to rerun

Run these from the repository root on a Sway or other wlroots session. Each run puts a bar on screen for 15 s.

The checks, and the spike's own log, which gives its first frame and VmRSS 10 s later:

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo build --release
timeout 15 target/release/crabshell-spike 2> run.log; cat run.log
```

The Quickshell comparison, exactly as run for the table above: three interleaved runs of each program. Each run prints one JSON line; take the median of each field. Logs go to `target/measure/`. It needs `qs` (0.3.2 here) and Python 3.11 or newer, plus `grim` if you take a screenshot.

```sh
cargo build --release
for i in 1 2 3; do
  sleep 2
  python3 -I baseline/harness.py target/measure/qs$i qs -- \
    qs -p baseline/quickshell --no-color --log-times --log-rules 'qt.scenegraph.time.renderloop=true'
  sleep 2
  python3 -I baseline/harness.py target/measure/spike$i spike -- target/release/crabshell-spike
done
```

To re-take `baseline/screenshot.png`, add `--screenshot baseline/screenshot.png --region '0,48 1920x30'` before the `--` of one `qs` run. The bar sits at y = 48 here because another Quickshell bar holds the top 48 px; without one, use the default region `0,0 1920x30`. `python3 -I baseline/harness.py --help` lists the options.
