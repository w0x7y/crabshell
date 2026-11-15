// The numbers and lists the pages draw from, taken from docs/design.md at a0b82d8 and from
// Quickshell 0.3.2's public type list. Change them here and every page follows.

// Phase 0, 10 Nov 2026: the same static text bar, the same laptop, SwayFX at 1920x1080, scale 1,
// median of three runs.
const METRICS = [
  {
    name: "memory (rss)",
    unit: "MiB",
    c: 13.2,
    q: 125.2,
    why: "resident memory, 10 s after the first frame",
    less: "less",
  },
  {
    name: "proportional memory",
    unit: "MiB",
    c: 6.2,
    q: 56.5,
    why: "pss: shared libraries split between processes",
    less: "less",
  },
  {
    name: "private dirty memory",
    unit: "MiB",
    c: 1.3,
    q: 30.3,
    why: "memory only this process holds",
    less: "less",
  },
  {
    name: "launch to first frame",
    unit: "ms",
    c: 22,
    q: 219,
    why: "from starting the program to its first frame",
    less: "faster",
  },
  {
    name: "threads",
    unit: "",
    c: 2,
    q: 20,
    why: "threads running in the process",
    less: "fewer",
  },
];

// Each phase's checklist: yes (done), part (partly), no (not yet) or fail (tried and failed).
const PHASES = [
  {
    n: 0,
    title: "spike",
    state: "done",
    when: "passed 10 nov",
    items: [
      ["yes", "static text bar on sway from one layer-shell surface"],
      ["yes", "rss at 10.5% of quickshell's, the go/no-go gate"],
      ["yes", "one zbus call on calloop's executor"],
    ],
  },
  {
    n: 1,
    title: "reactive core",
    state: "done",
    when: "passed 10 nov",
    items: [
      ["yes", "signals, memos, effects and owners"],
      ["yes", "all 11 required tests"],
    ],
  },
  {
    n: 2,
    title: "widgets, layout and rendering",
    state: "done",
    when: "passed 11 nov",
    items: [
      ["yes", "widgets, styles and damage tracking"],
      ["yes", "fractional scale and pointer input"],
      ["yes", "per_output with monitor hotplug"],
      ["yes", "goldens at scales 1, 1.5 and 2; zero idle wakeups"],
    ],
  },
  {
    n: 3,
    title: "clock, workspaces and audio",
    state: "done",
    when: "passed 11 nov",
    items: [
      ["yes", "a workspace switch shows within one frame"],
      ["yes", "scrolling changes the volume, and pactl agrees"],
      ["yes", "the clock is right after suspend and resume"],
    ],
  },
  {
    n: 4,
    title: "tray and popups",
    state: "done",
    when: "passed 13 nov",
    items: [
      ["yes", "tray watcher, host and dbusmenu"],
      ["yes", "PopupWindow, Menu and Tray widgets"],
      ["yes", "nm-applet, blueman, keepassxc and discord work"],
      ["yes", "all of them survive a reload"],
    ],
  },
  {
    n: 5,
    title: "cli and reload",
    state: "done",
    when: "passed 13 nov",
    items: [
      ["yes", "crabshell init, check and run"],
      ["yes", "rebuild and reload on save, state kept by cx.persist"],
      ["yes", "crash restarts and the fallback to the last good build"],
      ["yes", "exit check: all four cases on headless sway"],
    ],
  },
  {
    n: 6,
    title: "the v0.1 release",
    state: "done",
    when: "done 13 nov",
    items: [
      ["yes", "config template, pinned to the design doc"],
      ["yes", "measured numbers in the performance table"],
      ["yes", "a readme that takes a new user from install to bar"],
      ["yes", "api docs on docs.rs"],
      ["yes", "crates published on crates.io"],
      [
        "yes",
        "cargo install to a built bar in 3 minutes, and the bar runs on hyprland too",
      ],
    ],
  },
  {
    n: 7,
    title: "windows and input",
    state: "done",
    when: "passed 14 nov",
    items: [
      ["yes", "floating windows and more layer-shell options"],
      ["yes", "input regions and masks"],
      ["yes", "keyboard, text fields, input methods and the clipboard"],
      ["yes", "scrolling lists, images and clipping"],
      ["yes", "exit check: the launcher example on headless sway"],
    ],
  },
  {
    n: 8,
    title: "animation and effects",
    state: "done",
    when: "passed 14 nov",
    items: [
      ["yes", "animations that run only while something moves"],
      ["yes", "gradients, shadows and background blur"],
      [
        "yes",
        "checked whether a gpu backend is needed: not yet, a frame draws in under 1 ms",
      ],
      ["yes", "exit check: the effects example on headless sway"],
      [
        "yes",
        "checked by eye: the maintainer's quickshell bar rebuilt in crabshell",
      ],
    ],
  },
  {
    n: 9,
    title: "desktop services",
    state: "done",
    when: "passed 14 nov",
    items: [
      ["yes", "notification server and media players"],
      ["part", "audio nodes and their volumes, without level meters"],
      ["yes", "battery and power profiles"],
      ["yes", "bluetooth and networking"],
      ["yes", "idle monitor and inhibitor"],
      ["yes", "exit check: the desktop example on headless sway"],
      ["yes", "checked by hand against the laptop's real daemons"],
    ],
  },
  {
    n: 10,
    title: "launchers and system integration",
    state: "done",
    when: "passed 14 nov",
    items: [
      ["yes", "desktop entries and their actions"],
      ["yes", "processes, files, sockets and json files"],
      ["yes", "an ipc handler driven from the command line"],
      ["yes", "color quantizing, screencopy and toplevel management"],
      ["yes", "hover as a signal and shadows cut out under glass"],
      ["yes", "exit check: the overview example on headless sway"],
    ],
  },
  {
    n: 11,
    title: "lock screen, greeter and polkit",
    state: "done",
    when: "passed 14 nov",
    items: [
      ["yes", "session lock surfaces with pam"],
      ["yes", "a greetd greeter"],
      ["yes", "a polkit agent"],
      ["yes", "exit check: the lock example on headless sway"],
      ["no", "checked by hand against the real pam, greetd and polkit"],
    ],
  },
  {
    n: 12,
    title: "more compositors",
    state: "done",
    when: "passed 15 nov",
    items: [
      ["yes", "hyprland: workspaces, monitors, windows, dispatch and events"],
      [
        "part",
        "hyprland: focus grab and global shortcuts, not run on hyprland",
      ],
      ["yes", "i3: events, outputs and commands"],
      ["part", "niri: only through the generic protocols, untested"],
      ["yes", "generic workspaces and the shortcut inhibitor"],
      ["yes", "exit check: the hyprland example against a fake hyprland"],
      ["no", "checked by hand on a real hyprland"],
    ],
  },
  {
    n: 13,
    title: "parity check",
    state: "next",
    when: "up next",
    items: [
      [
        "no",
        "every quickshell 0.3.2 type built, or written down with a reason",
      ],
    ],
  },
];

// How far the phase underway is: a part done counts half.
const phaseProgress = (p) => {
  const weight = { yes: 1, part: 0.5 };
  return (
    Math.round(
      (p.items.reduce((sum, [s]) => sum + (weight[s] || 0), 0) /
        p.items.length) *
        100,
    ) + "%"
  );
};
const railCells = (list) =>
  list
    .map((p) =>
      p.state === "next"
        ? `<i class="next" style="--done:${phaseProgress(p)}" title="phase ${p.n}: ${p.title}"></i>`
        : `<i class="${p.state === "done" ? "done" : ""}" title="phase ${p.n}: ${p.title}"></i>`,
    )
    .join("");

// Every type in quickshell 0.3.2's public docs: [type, status, note]. Status is yes, part, no,
// fail or na. A number as the note is the phase it's planned for.
const Q = {
  Quickshell: [
    [
      "BoundComponent",
      "na",
      "qml component plumbing; rust closures build widgets",
    ],
    ["ColorQuantizer", "yes", "cx.quantize_colors(path, depth)"],
    ["DesktopAction", "yes", "DesktopAction"],
    ["DesktopEntries", "yes", "cx.desktop_entries()"],
    ["DesktopEntry", "yes", "DesktopEntry"],
    [
      "EasingCurve",
      "yes",
      "Easing, with CSS's curves and Bezier, in a Transition",
    ],
    ["Edges", "yes", "Edge and Anchors"],
    ["ElapsedTimer", "na", "std::time::Instant"],
    [
      "ExclusionMode",
      "yes",
      "exclusive_zone: a size, 0 to take none, -1 to ignore others; auto by default",
    ],
    ["FloatingWindow", "yes", "FloatingWindow, shown with cx.floating"],
    ["Intersection", "yes", "Region: union, subtract, intersect and xor"],
    [
      "LazyLoader",
      "yes",
      "children_keyed over a list of one or none builds and frees on demand",
    ],
    ["ObjectComparison", "na", "PartialEq"],
    ["ObjectModel", "na", "a signal holding a Vec"],
    [
      "PanelWindow",
      "yes",
      "anchors, size, exclusive zone, layer, margins, keyboard focus and a mask",
    ],
    ["PersistentProperties", "yes", "cx.persist"],
    [
      "PopupAdjustment",
      "part",
      "Adjust: flip and slide; popups are never resized",
    ],
    [
      "PopupAnchor",
      "yes",
      "anchored to the widget that opened it, with edges and gravity",
    ],
    ["PopupWindow", "yes", "PopupWindow, opened with cx.open_popup"],
    ["QsMenuAnchor", "yes", "tray menus open beside their icon"],
    ["QsMenuButtonType", "yes", "MenuToggle: checkbox and radio"],
    ["QsMenuEntry", "yes", "MenuEntry"],
    ["QsMenuHandle", "yes", "TrayMenu"],
    ["QsMenuOpener", "yes", "the Tray opens and follows menus"],
    [
      "QsWindow",
      "part",
      "panels, popups and floating windows; masks on panels only",
    ],
    [
      "Quickshell",
      "part",
      "monitors through per_output; reload through the supervisor",
    ],
    [
      "QuickshellSettings",
      "part",
      "CRABSHELL_LOG; the supervisor watches the files",
    ],
    ["Region", "yes", "Region, set with PanelWindow::mask"],
    ["RegionShape", "yes", "rectangles, rounded rectangles and ellipses"],
    ["Reloadable", "yes", "state survives a reload with cx.persist"],
    ["Retainable", "no", 8],
    ["RetainableLock", "no", 8],
    ["Scope", "yes", "Cx contexts and owners"],
    ["ScriptModel", "yes", "children_keyed reuses children by key"],
    ["ShellRoot", "yes", "crabshell::run"],
    ["ShellScreen", "part", "Output has a name; no geometry or model yet"],
    ["Singleton", "na", "a rust static, or a service from cx.service"],
    ["SystemClock", "yes", "cx.clock(Precision)"],
    ["TransformWatcher", "no", 7],
    ["Variants", "yes", "cx.per_output and children_keyed"],
  ],
  "Quickshell.Bluetooth": [
    ["Bluetooth", "yes", "cx.bluetooth()"],
    ["BluetoothAdapter", "yes", "BluetoothAdapter"],
    ["BluetoothAdapterState", "part", "powered and discovering"],
    ["BluetoothDevice", "yes", "BluetoothDevice"],
    ["BluetoothDeviceState", "part", "connected, paired and trusted"],
  ],
  "Quickshell.DBusMenu": [
    ["DBusMenuHandle", "yes", "tray menus over dbusmenu"],
    ["DBusMenuItem", "yes", "MenuEntry, read from dbusmenu items"],
  ],
  "Quickshell.Hyprland": [
    ["GlobalShortcut", "part", "cx.global_shortcut, not yet run on hyprland"],
    ["Hyprland", "yes", "cx.hyprland(): lists, focus_workspace, dispatch"],
    ["HyprlandEvent", "yes", "HyprlandEvent with parse(n)"],
    [
      "HyprlandFocusGrab",
      "part",
      "focus_grab on a window, not yet run on hyprland",
    ],
    ["HyprlandMonitor", "yes", "HyprlandMonitor"],
    ["HyprlandToplevel", "yes", "HyprlandToplevel"],
    ["HyprlandWindow", "part", "HyprlandToplevel's class, title and state"],
    ["HyprlandWorkspace", "yes", "HyprlandWorkspace"],
  ],
  "Quickshell.I3": [
    ["I3", "yes", "cx.sway(): workspaces, outputs, dispatch and events"],
    ["I3Event", "yes", "SwayEvent"],
    ["I3IpcListener", "yes", "cx.sway().on_event(kinds, handler)"],
    ["I3Monitor", "yes", "SwayOutput"],
    ["I3Workspace", "yes", "Workspace"],
  ],
  "Quickshell.Io": [
    ["DataStream", "part", "on_line on a process or socket"],
    ["DataStreamParser", "part", "on_line splits at newlines"],
    ["FileView", "yes", "cx.file_view(path)"],
    ["FileViewAdapter", "part", "cx.json_file(path, default)"],
    ["FileViewError", "part", "a read error is logged and text() is None"],
    ["IpcHandler", "yes", "cx.ipc(target, function, handler)"],
    ["JsonAdapter", "yes", "cx.json_file(path, default)"],
    ["JsonObject", "part", "any serde type"],
    ["Process", "yes", "cx.process(command)"],
    ["Socket", "yes", "cx.socket(path)"],
    ["SocketServer", "no", "no server sockets yet; cx.ipc covers calls in"],
    ["SplitParser", "part", "on_line splits at newlines"],
    ["StdioCollector", "yes", "stdout() and stderr()"],
  ],
  "Quickshell.Networking": [
    ["ConnectionFailReason", "no", "a refusal is logged, not shown"],
    [
      "ConnectionState",
      "part",
      "NetworkState: connected, connecting, disconnected or unavailable",
    ],
    ["DeviceType", "yes", "NetworkKind"],
    ["NMSettings", "no", "saved connections aren't listed"],
    ["Network", "yes", "WifiNetwork"],
    ["NetworkBackendType", "na", "NetworkManager is the one backend"],
    ["NetworkConnectivity", "no", "connectivity checks aren't read"],
    ["NetworkDevice", "yes", "NetworkDevice"],
    ["Networking", "yes", "cx.network()"],
    ["WifiDevice", "part", "a NetworkDevice of kind Wifi, with scan"],
    ["WifiDeviceMode", "no", "the mode isn't read"],
    ["WifiNetwork", "yes", "WifiNetwork"],
    ["WifiSecurityType", "part", "secured or open"],
    ["WiredDevice", "part", "a NetworkDevice of kind Ethernet"],
  ],
  "Quickshell.Services.Greetd": [
    ["Greetd", "yes", "cx.greetd()"],
    ["GreetdState", "yes", "GreetState"],
  ],
  "Quickshell.Services.Mpris": [
    ["Mpris", "yes", "cx.mpris()"],
    ["MprisLoopState", "yes", "LoopStatus"],
    ["MprisPlaybackState", "yes", "PlaybackStatus"],
    ["MprisPlayer", "yes", "Player, with position_at"],
  ],
  "Quickshell.Services.Notifications": [
    ["Notification", "yes", "Notification"],
    ["NotificationAction", "yes", "actions as (key, label), and invoke"],
    ["NotificationCloseReason", "part", "sent to apps, not shown to configs"],
    ["NotificationServer", "yes", "cx.notifications()"],
    ["NotificationUrgency", "yes", "Urgency"],
  ],
  "Quickshell.Services.Pam": [
    ["PamContext", "yes", "cx.pam(service)"],
    ["PamError", "part", "PamResult::Error with PAM's text"],
    ["PamResult", "yes", "PamResult"],
  ],
  "Quickshell.Services.Pipewire": [
    [
      "Pipewire",
      "part",
      "sinks, sources and app streams, through pipewire-pulse",
    ],
    ["PwAudioChannel", "no", "channels are averaged"],
    ["PwLink", "no", "the graph's links aren't read"],
    ["PwLinkGroup", "no", "the graph's links aren't read"],
    ["PwLinkState", "no", "the graph's links aren't read"],
    ["PwNode", "part", "AudioNode and AudioStream"],
    ["PwNodeAudio", "part", "volume and mute"],
    ["PwNodeLinkTracker", "no", "the graph's links aren't read"],
    [
      "PwNodePeakMonitor",
      "no",
      "level meters would read the sound many times a second",
    ],
    ["PwNodeType", "part", "sinks, sources and streams"],
    ["PwObjectTracker", "na", "every node is followed"],
  ],
  "Quickshell.Services.Polkit": [
    ["AuthFlow", "yes", "AuthRequest, with respond, cancel and pick_identity"],
    ["PolkitAgent", "yes", "cx.polkit_agent()"],
  ],
  "Quickshell.Services.SystemTray": [
    ["Category", "no", "the item's category isn't read yet"],
    ["Status", "yes", "TrayStatus"],
    ["SystemTray", "yes", "TrayHost and the Tray widget"],
    ["SystemTrayItem", "yes", "TrayItem: icon, pixmaps, tooltip, menu"],
  ],
  "Quickshell.Services.UPower": [
    ["PerformanceDegradationReason", "no", "not read"],
    ["PowerProfile", "yes", "PowerProfile"],
    ["PowerProfiles", "yes", "cx.power_profiles()"],
    ["UPower", "yes", "cx.upower()"],
    ["UPowerDevice", "yes", "PowerDevice"],
    ["UPowerDeviceState", "yes", "BatteryState"],
    ["UPowerDeviceType", "yes", "DeviceKind"],
  ],
  "Quickshell.Wayland": [
    ["BackgroundEffect", "yes", "PanelWindow::blur and FloatingWindow::blur"],
    ["IdleInhibitor", "yes", "inhibit_idle on a window"],
    ["IdleMonitor", "yes", "cx.idle_monitor(timeout)"],
    ["ScreencopyView", "yes", "ScreencopyView"],
    ["ShortcutInhibitor", "yes", "inhibit_shortcuts on a window"],
    ["Toplevel", "yes", "Toplevel"],
    ["ToplevelManager", "yes", "cx.toplevels()"],
    ["WlSessionLock", "yes", "SessionLock, shown with cx.session_lock"],
    ["WlSessionLockSurface", "yes", "the surface builder, one per output"],
    ["WlrKeyboardFocus", "yes", "KeyboardFocus: none, exclusive and on demand"],
    ["WlrLayer", "yes", "Layer: background, bottom, top and overlay"],
    [
      "WlrLayershell",
      "yes",
      "layer, keyboard focus and namespace on PanelWindow",
    ],
  ],
  "Quickshell.Widgets": [
    ["ClippingRectangle", "yes", "Style::clip, following the rounded corners"],
    ["ClippingWrapperRectangle", "yes", "Style::clip with padding"],
    ["IconImage", "yes", "Icon: theme names and pixmaps"],
    [
      "MarginWrapperManager",
      "part",
      "padding on any style, per axis rather than per side",
    ],
    ["WrapperItem", "yes", "a Stack with a padded style"],
    ["WrapperManager", "na", "rust containers take a child directly"],
    [
      "WrapperMouseArea",
      "yes",
      "Button: press, clicks, scroll, keys and hover",
    ],
    [
      "WrapperRectangle",
      "yes",
      "a Stack with background, border, radius and padding",
    ],
  ],
  "Quickshell.WindowManager": [
    ["ScreenProjection", "part", "windowsets_on(output), a method"],
    ["WindowManager", "yes", "cx.window_manager()"],
    ["Windowset", "yes", "Windowset"],
    ["WindowsetProjection", "yes", "WindowsetProjection"],
  ],
};
const ALL_TYPES = Object.values(Q).flat();
const countTypes = (status, list = ALL_TYPES) =>
  list.filter((t) => t[1] === status).length;
