// The numbers and lists the pages draw from, taken from docs/design.md at 3ec1d89 and from
// Quickshell 0.3.2's public type list. Change them here and every page follows.

// Phase 0, 10 Nov 2026: the same static text bar, the same laptop, SwayFX at 1920x1080, scale 1,
// median of three runs.
const METRICS = [
  { name: "memory (rss)", unit: "MiB", c: 13.2, q: 125.2, why: "resident memory, 10 s after the first frame", less: "less" },
  { name: "proportional memory", unit: "MiB", c: 6.2, q: 56.5, why: "pss: shared libraries split between processes", less: "less" },
  { name: "private dirty memory", unit: "MiB", c: 1.3, q: 30.3, why: "memory only this process holds", less: "less" },
  { name: "launch to first frame", unit: "ms", c: 22, q: 219, why: "from starting the program to its first frame", less: "faster" },
  { name: "threads", unit: "", c: 2, q: 20, why: "threads running in the process", less: "fewer" }
];

// Each phase's checklist: yes (done), part (partly), no (not yet) or fail (tried and failed).
const PHASES = [
  { n: 0, title: "spike", state: "done", when: "passed 10 nov", items: [["yes", "static text bar on sway from one layer-shell surface"], ["yes", "rss at 10.5% of quickshell's, the go/no-go gate"], ["yes", "one zbus call on calloop's executor"]] },
  { n: 1, title: "reactive core", state: "done", when: "passed 10 nov", items: [["yes", "signals, memos, effects and owners"], ["yes", "all 11 required tests"]] },
  { n: 2, title: "widgets, layout and rendering", state: "done", when: "passed 11 nov", items: [["yes", "widgets, styles and damage tracking"], ["yes", "fractional scale and pointer input"], ["yes", "per_output with monitor hotplug"], ["yes", "goldens at scales 1, 1.5 and 2; zero idle wakeups"]] },
  { n: 3, title: "clock, workspaces and audio", state: "done", when: "passed 11 nov", items: [["yes", "a workspace switch shows within one frame"], ["yes", "scrolling changes the volume, and pactl agrees"], ["yes", "the clock is right after suspend and resume"]] },
  { n: 4, title: "tray and popups", state: "done", when: "passed 13 nov", items: [["yes", "tray watcher, host and dbusmenu"], ["yes", "PopupWindow, Menu and Tray widgets"], ["yes", "nm-applet, blueman, keepassxc and discord work"], ["yes", "all of them survive a reload"]] },
  { n: 5, title: "cli and reload", state: "done", when: "passed 13 nov", items: [["yes", "crabshell init, check and run"], ["yes", "rebuild and reload on save, state kept by cx.persist"], ["yes", "crash restarts and the fallback to the last good build"], ["yes", "exit check: all four cases on headless sway"]] },
  { n: 6, title: "the v0.1 release", state: "done", when: "done 13 nov", items: [["yes", "config template, pinned to the design doc"], ["yes", "measured numbers in the performance table"], ["yes", "a readme that takes a new user from install to bar"], ["yes", "api docs on docs.rs"], ["yes", "crates published on crates.io"], ["part", "cargo install to a built bar in 3 minutes; the clean-account run is skipped for now"]] },
  { n: 7, title: "windows and input", state: "next", when: "up next", items: [["no", "floating windows and more layer-shell options"], ["no", "input regions and masks"], ["no", "keyboard and text input"], ["no", "scrolling lists, images, clipping and wrapper widgets"]] },
  { n: 8, title: "animation and effects", state: "later", when: "after v0.1", items: [["no", "animations that run only while something moves"], ["no", "gradients, shadows and background blur"], ["no", "a gpu backend, if the cpu renderer can't keep up"]] },
  { n: 9, title: "desktop services", state: "later", when: "after v0.1", items: [["no", "notification server and media players"], ["no", "pipewire nodes and levels"], ["no", "battery and power profiles"], ["no", "bluetooth and networking"], ["no", "idle monitor and inhibitor"]] },
  { n: 10, title: "launchers and system integration", state: "later", when: "after v0.1", items: [["no", "desktop entries and their actions"], ["no", "processes, files, sockets and json files"], ["no", "an ipc handler driven from the command line"], ["no", "color quantizing, screencopy and toplevel management"]] },
  { n: 11, title: "lock screen, greeter and polkit", state: "later", when: "after v0.1", items: [["no", "session lock surfaces with pam"], ["no", "a greetd greeter"], ["no", "a polkit agent"]] },
  { n: 12, title: "more compositors", state: "later", when: "after v0.1", items: [["no", "hyprland: workspaces, monitors, focus grab, global shortcuts"], ["no", "i3 and niri"], ["no", "generic window-manager protocols"]] },
  { n: 13, title: "parity check", state: "later", when: "last", items: [["no", "every quickshell 0.3.2 type built, or written down with a reason"]] }
];

// How far the phase underway is: a part done counts half.
const phaseProgress = (p) => {
  const weight = { yes: 1, part: 0.5 };
  return Math.round((p.items.reduce((sum, [s]) => sum + (weight[s] || 0), 0) / p.items.length) * 100) + "%";
};
const railCells = (list) => list.map((p) => p.state === "next"
  ? `<i class="next" style="--done:${phaseProgress(p)}" title="phase ${p.n}: ${p.title}"></i>`
  : `<i class="${p.state === "done" ? "done" : ""}" title="phase ${p.n}: ${p.title}"></i>`).join("");

// Every type in quickshell 0.3.2's public docs: [type, status, note]. Status is yes, part, no,
// fail or na. A number as the note is the phase it's planned for.
const Q = {
  "Quickshell": [
    ["BoundComponent", "na", "qml component plumbing; rust closures build widgets"],
    ["ColorQuantizer", "no", 10], ["DesktopAction", "no", 10], ["DesktopEntries", "no", 10], ["DesktopEntry", "no", 10],
    ["EasingCurve", "no", 8],
    ["Edges", "yes", "Edge and Anchors"],
    ["ElapsedTimer", "na", "std::time::Instant"],
    ["ExclusionMode", "part", "exclusive_zone sets a size; no ignore or auto mode"],
    ["FloatingWindow", "no", 7], ["Intersection", "no", 7], ["LazyLoader", "no", 7],
    ["ObjectComparison", "na", "PartialEq"],
    ["ObjectModel", "na", "a signal holding a Vec"],
    ["PanelWindow", "yes", "anchors, size, exclusive zone and layer"],
    ["PersistentProperties", "yes", "cx.persist"],
    ["PopupAdjustment", "yes", "popups flip and slide on both axes"],
    ["PopupAnchor", "yes", "anchored to the widget that opened it"],
    ["PopupWindow", "yes", "PopupWindow, opened with cx.open_popup"],
    ["QsMenuAnchor", "yes", "tray menus open beside their icon"],
    ["QsMenuButtonType", "yes", "MenuToggle: checkbox and radio"],
    ["QsMenuEntry", "yes", "MenuEntry"],
    ["QsMenuHandle", "yes", "TrayMenu"],
    ["QsMenuOpener", "yes", "the Tray opens and follows menus"],
    ["QsWindow", "part", "panels and popups; no floating windows"],
    ["Quickshell", "part", "monitors through per_output; reload through the supervisor"],
    ["QuickshellSettings", "part", "CRABSHELL_LOG; the supervisor watches the files"],
    ["Region", "no", 7], ["RegionShape", "no", 7],
    ["Reloadable", "yes", "state survives a reload with cx.persist"],
    ["Retainable", "no", 8], ["RetainableLock", "no", 8],
    ["Scope", "yes", "Cx contexts and owners"],
    ["ScriptModel", "yes", "children_keyed reuses children by key"],
    ["ShellRoot", "yes", "crabshell::run"],
    ["ShellScreen", "part", "Output has a name; no geometry or model yet"],
    ["Singleton", "na", "a rust static, or a service from cx.service"],
    ["SystemClock", "yes", "cx.clock(Precision)"],
    ["TransformWatcher", "no", 7],
    ["Variants", "yes", "cx.per_output and children_keyed"]
  ],
  "Quickshell.Bluetooth": [["Bluetooth", "no", 9], ["BluetoothAdapter", "no", 9], ["BluetoothAdapterState", "no", 9], ["BluetoothDevice", "no", 9], ["BluetoothDeviceState", "no", 9]],
  "Quickshell.DBusMenu": [["DBusMenuHandle", "yes", "tray menus over dbusmenu"], ["DBusMenuItem", "yes", "MenuEntry, read from dbusmenu items"]],
  "Quickshell.Hyprland": [["GlobalShortcut", "no", 12], ["Hyprland", "no", 12], ["HyprlandEvent", "no", 12], ["HyprlandFocusGrab", "no", 12], ["HyprlandMonitor", "no", 12], ["HyprlandToplevel", "no", 12], ["HyprlandWindow", "no", 12], ["HyprlandWorkspace", "no", 12]],
  "Quickshell.I3": [
    ["I3", "part", "cx.sway(): workspaces and focus over the i3-compatible ipc"],
    ["I3Event", "no", 12], ["I3IpcListener", "no", 12],
    ["I3Monitor", "part", "a workspace's output name"],
    ["I3Workspace", "yes", "Workspace"]
  ],
  "Quickshell.Io": [["DataStream", "no", 10], ["DataStreamParser", "no", 10], ["FileView", "no", 10], ["FileViewAdapter", "no", 10], ["FileViewError", "no", 10], ["IpcHandler", "no", 10], ["JsonAdapter", "no", 10], ["JsonObject", "no", 10], ["Process", "no", 10], ["Socket", "no", 10], ["SocketServer", "no", 10], ["SplitParser", "no", 10], ["StdioCollector", "no", 10]],
  "Quickshell.Networking": [["ConnectionFailReason", "no", 9], ["ConnectionState", "no", 9], ["DeviceType", "no", 9], ["NMSettings", "no", 9], ["Network", "no", 9], ["NetworkBackendType", "no", 9], ["NetworkConnectivity", "no", 9], ["NetworkDevice", "no", 9], ["Networking", "no", 9], ["WifiDevice", "no", 9], ["WifiDeviceMode", "no", 9], ["WifiNetwork", "no", 9], ["WifiSecurityType", "no", 9], ["WiredDevice", "no", 9]],
  "Quickshell.Services.Greetd": [["Greetd", "no", 11], ["GreetdState", "no", 11]],
  "Quickshell.Services.Mpris": [["Mpris", "no", 9], ["MprisLoopState", "no", 9], ["MprisPlaybackState", "no", 9], ["MprisPlayer", "no", 9]],
  "Quickshell.Services.Notifications": [["Notification", "no", 9], ["NotificationAction", "no", 9], ["NotificationCloseReason", "no", 9], ["NotificationServer", "no", 9], ["NotificationUrgency", "no", 9]],
  "Quickshell.Services.Pam": [["PamContext", "no", 11], ["PamError", "no", 11], ["PamResult", "no", 11]],
  "Quickshell.Services.Pipewire": [
    ["Pipewire", "part", "the default sink, through pipewire-pulse"],
    ["PwAudioChannel", "no", 9], ["PwLink", "no", 9], ["PwLinkGroup", "no", 9], ["PwLinkState", "no", 9],
    ["PwNode", "part", "Sink: the default output only"],
    ["PwNodeAudio", "part", "volume and mute"],
    ["PwNodeLinkTracker", "no", 9], ["PwNodePeakMonitor", "no", 9], ["PwNodeType", "no", 9], ["PwObjectTracker", "no", 9]
  ],
  "Quickshell.Services.Polkit": [["AuthFlow", "no", 11], ["PolkitAgent", "no", 11]],
  "Quickshell.Services.SystemTray": [
    ["Category", "no", "the item's category isn't read yet"],
    ["Status", "yes", "TrayStatus"],
    ["SystemTray", "yes", "TrayHost and the Tray widget"],
    ["SystemTrayItem", "yes", "TrayItem: icon, pixmaps, tooltip, menu"]
  ],
  "Quickshell.Services.UPower": [["PerformanceDegradationReason", "no", 9], ["PowerProfile", "no", 9], ["PowerProfiles", "no", 9], ["UPower", "no", 9], ["UPowerDevice", "no", 9], ["UPowerDeviceState", "no", 9], ["UPowerDeviceType", "no", 9]],
  "Quickshell.Wayland": [
    ["BackgroundEffect", "no", 8], ["IdleInhibitor", "no", 9], ["IdleMonitor", "no", 9], ["ScreencopyView", "no", 10], ["ShortcutInhibitor", "no", 12],
    ["Toplevel", "no", 10], ["ToplevelManager", "no", 10], ["WlSessionLock", "no", 11], ["WlSessionLockSurface", "no", 11], ["WlrKeyboardFocus", "no", 7],
    ["WlrLayer", "yes", "Layer: background, bottom, top and overlay"],
    ["WlrLayershell", "part", "anchors, size, exclusive zone and layer; no margins or keyboard focus"]
  ],
  "Quickshell.Widgets": [
    ["ClippingRectangle", "no", 7], ["ClippingWrapperRectangle", "no", 7],
    ["IconImage", "yes", "Icon: theme names and pixmaps"],
    ["MarginWrapperManager", "part", "padding on Row and Column"],
    ["WrapperItem", "part", "containers size to their children"],
    ["WrapperManager", "na", "rust containers take a child directly"],
    ["WrapperMouseArea", "part", "Button: clicks, scroll and a hover style"],
    ["WrapperRectangle", "part", "Style: background, border and radius"]
  ],
  "Quickshell.WindowManager": [["ScreenProjection", "no", 12], ["WindowManager", "no", 12], ["Windowset", "no", 12], ["WindowsetProjection", "no", 12]]
};
const ALL_TYPES = Object.values(Q).flat();
const countTypes = (status, list = ALL_TYPES) => list.filter((t) => t[1] === status).length;
