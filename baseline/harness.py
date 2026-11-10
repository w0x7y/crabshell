"""Measure one run of a layer-shell bar, the same way for the spike and for Quickshell.

Run from the repository root, once per run; it prints one JSON object:

    python3 -I baseline/harness.py target/measure/spike1 spike -- target/release/crabshell-spike
    python3 -I baseline/harness.py target/measure/qs1 qs -- \\
        qs -p baseline/quickshell --no-color --log-times \\
        --log-rules 'qt.scenegraph.time.renderloop=true'

What it does:
- Launches the command, writing its stdout to <log_prefix>.out and stderr to
  <log_prefix>.err, and notes the wall-clock time just before the fork/exec.
- First frame = timestamp of the program's own log line written just after its
  first buffer commit, minus the launch time:
    spike: the "first frame committed" line (tracing, RFC 3339 UTC, microseconds).
    qs:    the first qt.scenegraph.time.renderloop "frame rendered ... swap=N" line
           with N >= 1 (local time, milliseconds). Qt first logs an empty pass with
           swap=0 that commits no buffer. Checked against WAYLAND_DEBUG, the chosen
           line matches the first wl_surface attach+commit to within 1 ms. qs must be
           run with --log-times and the --log-rules above for this line to appear.
  WAYLAND_DEBUG is deliberately unset: its logging slows the spike's startup by ~20 ms.
- Idle wakeups: voluntary context switches summed over all threads, from first
  frame + 4 s to + 9 s. Each time a thread blocks (epoll_wait, a futex, ...) and
  later wakes, the kernel adds one. Needs no perf or strace.
- At first frame + 10 s: VmRSS and Threads from /proc/<pid>/status, and Rss, Pss,
  Private_Dirty from /proc/<pid>/smaps_rollup.
- Sends SIGTERM at launch + 15 s, like `timeout 15`.
"""

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

SPIKE_MARK = re.compile(r"^(\S+Z)\s+INFO .*first frame committed")
QS_MARK = re.compile(
    r"^(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d{3}) DEBUG qt\.scenegraph\.time\.renderloop: "
    r".*frame rendered in \d+ms, sync=\d+, render=\d+, swap=[1-9]\d*"
)
RUN_SECONDS = 15


def parse_args():
    # Everything after `--` is the command to measure, passed through untouched.
    argv = sys.argv[1:]
    split = argv.index("--") if "--" in argv else len(argv)
    parser = argparse.ArgumentParser(
        description="Measure one run of a layer-shell bar.",
        usage="%(prog)s LOG_PREFIX {spike,qs} [--screenshot PNG] [--region GEOMETRY] -- COMMAND...",
    )
    parser.add_argument("log_prefix", help="writes LOG_PREFIX.out and LOG_PREFIX.err, e.g. target/measure/qs1")
    parser.add_argument("kind", choices=["spike", "qs"], help="which log line marks the first frame")
    parser.add_argument("--screenshot", metavar="PNG", help="grab the bar with grim 1.5 s after the first frame")
    parser.add_argument(
        "--region",
        metavar="GEOMETRY",
        default="0,0 1920x30",
        help="grim -g geometry for --screenshot (default: %(default)s; use '0,48 1920x30' if "
        "another 48 px top bar pushes this one down)",
    )
    args = parser.parse_args(argv[:split])
    args.command = argv[split + 1 :]
    if not args.command:
        parser.error("give the command to measure after --")
    return args


def first_frame_unix(kind, line):
    """Unix time (seconds) of the first-frame log line, or None if it isn't one."""
    if kind == "spike" and (m := SPIKE_MARK.match(line)):
        return datetime.fromisoformat(m.group(1)).timestamp()
    if kind == "qs" and (m := QS_MARK.match(line)):
        return datetime.strptime(m.group(1), "%Y-%m-%d %H:%M:%S.%f").timestamp()  # local time
    return None


def voluntary_switches(pid):
    """{tid: (thread name, voluntary context switches)} for every thread of pid."""
    out = {}
    for task in Path(f"/proc/{pid}/task").iterdir():
        try:
            status = (task / "status").read_text()
        except OSError:
            continue  # the thread exited between listing and reading
        fields = dict(line.split(":", 1) for line in status.splitlines() if ":" in line)
        out[task.name] = (fields["Name"].strip(), int(fields["voluntary_ctxt_switches"]))
    return out


def proc_fields(path, names):
    """The first number on each `name:` line of a /proc file, for the given names."""
    found = {}
    for line in Path(path).read_text().splitlines():
        key, _, rest = line.partition(":")
        if key in names:
            found[key] = int(rest.split()[0])
    return found


def sleep_until(t):
    time.sleep(max(0.0, t - time.time()))


def main():
    args = parse_args()
    prefix = Path(args.log_prefix)
    prefix.parent.mkdir(parents=True, exist_ok=True)
    out_path, err_path = Path(f"{prefix}.out"), Path(f"{prefix}.err")

    env = dict(os.environ)
    env.pop("WAYLAND_DEBUG", None)
    with open(out_path, "w") as out, open(err_path, "w") as err:
        t0 = time.time()
        proc = subprocess.Popen(args.command, env=env, stdout=out, stderr=err)
    deadline = t0 + RUN_SECONDS

    # Follow the program's log until its first-frame line shows up.
    first_frame = None
    with open(err_path if args.kind == "spike" else out_path) as log:
        while first_frame is None and time.time() < deadline:
            line = log.readline()
            if not line:
                time.sleep(0.002)
                continue
            first_frame = first_frame_unix(args.kind, line)
    if first_frame is None:
        proc.send_signal(signal.SIGTERM)
        sys.exit(f"no first-frame line within {RUN_SECONDS} s; see {out_path} and {err_path}")

    if args.screenshot:
        sleep_until(first_frame + 1.5)
        subprocess.run(["grim", "-g", args.region, args.screenshot], check=True)

    sleep_until(first_frame + 4)
    before = voluntary_switches(proc.pid)
    sleep_until(first_frame + 9)
    after = voluntary_switches(proc.pid)
    wakeups = {}
    for tid in before.keys() & after.keys():
        woke = after[tid][1] - before[tid][1]
        if woke:
            wakeups[f"{after[tid][0]}/{tid}"] = woke

    sleep_until(first_frame + 10)
    status = proc_fields(f"/proc/{proc.pid}/status", {"VmRSS", "Threads"})
    rollup = proc_fields(f"/proc/{proc.pid}/smaps_rollup", {"Rss", "Pss", "Private_Dirty"})

    sleep_until(deadline)
    proc.send_signal(signal.SIGTERM)
    try:
        exit_code = proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        exit_code = "killed after 5 s"

    print(json.dumps({
        "first_frame_ms": round((first_frame - t0) * 1000, 2),
        "vmrss_kib": status["VmRSS"],
        "rss_kib": rollup["Rss"],
        "pss_kib": rollup["Pss"],
        "private_dirty_kib": rollup["Private_Dirty"],
        "threads": status["Threads"],
        "wakeups_5s": sum(wakeups.values()),
        "wakeups_by_thread": wakeups,
        "exit_code": exit_code,
    }))


main()
