#!/usr/bin/env python3
"""
P11 performance baseline driver for neovibe's own stack (poc/neovide_embed_live's
`perf_baseline` binary), to be compared against the stock-Neovide baseline measured in the
previous phase (see that phase's driver.py, kept alongside this file's sibling directory in the
scratchpad, for the reference methodology this file replicates as closely as neovibe's own API
allows).

Unlike the stock driver -- which WAS the workload driver, talking to nvim directly over a second
RPC socket -- `perf_baseline` is self-driving (LiveHarness exposes no external RPC attach point;
see that binary's own module doc for why). This script's job is narrower and purely external:

  1. Launch `perf_baseline` (t0 = perf_counter() immediately before Popen -- same startup-timing
     anchor the stock driver used).
  2. Continuously drain its stdout on a dedicated reader thread (never let the pipe's ~64KB OS
     buffer back up while this script is busy /proc-sampling -- the stock report flagged this
     exact hazard) into a timestamped line log, and expose `wait_for(marker, timeout)` so the main
     thread can synchronize its own /proc + GPU sampling windows to the binary's own
     `NEOVIBE_PHASE:*` markers.
  3. Discover the real `nvim --embed` child pid the same way the stock driver did (psutil children
     scan), and run the *exact same* ProcSampler / intel_gpu_top sampling code (copied near-
     verbatim from the stock driver.py) over the same idle/typing/scroll windows, at the same
     sample intervals and durations.
  4. Read back `perf_baseline`'s own internal frame-pacing JSON (written to disk, not just stdout)
     for the "Frame timing" category, which -- unlike the stock phase -- this project's own
     LiveHarness/GTK render loop already instruments per-frame (see that binary's module doc).
  5. Confirm zero orphaned nvim processes after the run (pgrep before/after), matching this
     project's own previously-reproduced orphan-process bug class.

See PHASE_REPORT.md (sibling file) for the numbers this produced and every documented methodology
deviation from the stock phase.
"""
import json
import os
import re
import signal
import statistics
import subprocess
import sys
import threading
import time

import psutil

# ----------------------------------------------------------------------------
# Config -- mirrors the stock driver.py's own constants where they overlap.
# ----------------------------------------------------------------------------
NEOVIBE_REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
PERF_BASELINE_BIN = f"{NEOVIBE_REPO}/poc/target/release/perf_baseline"
SCRATCH = "/tmp/neovibe_p11_scratch"
WORKLOAD_FILE = f"{SCRATCH}/workload.rs"  # the SAME file (same md5) the stock phase measured against
RESULTS_DIR = f"{NEOVIBE_REPO}/poc/p11_measurements/results"
SANITY_FILE = "/tmp/neovibe_p11_sanity.txt"
RESULTS_JSON_FROM_RUST = "/tmp/neovibe_p11_results.json"

IDLE_DURATION_S = 13.0  # must match perf_baseline.rs's own IDLE_DURATION_S constant
IDLE_SAMPLE_INTERVAL_S = 0.5
WORKLOAD_SAMPLE_INTERVAL_S = 0.15
GPU_SAMPLE_INTERVAL_MS = 150

STARTUP_TIMEOUT_S = 20.0
PHASE_TIMEOUT_S = 30.0  # generous per-marker wait (warmup alone sleeps 13s)

HZ = os.sysconf("SC_CLK_TCK")

# ----------------------------------------------------------------------------
# /proc sampling helpers -- copied verbatim from the stock phase's driver.py.
# ----------------------------------------------------------------------------


def read_proc_cpu_ticks(pid):
    with open(f"/proc/{pid}/stat", "r") as f:
        data = f.read()
    rest = data[data.rfind(")") + 2:]
    parts = rest.split()
    utime = int(parts[11])
    stime = int(parts[12])
    return utime + stime


def read_proc_rss_kb(pid):
    with open(f"/proc/{pid}/status", "r") as f:
        for line in f:
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    return None


class ProcSampler:
    def __init__(self, pids, interval_s):
        self.pids = list(pids)
        self.interval_s = interval_s
        self.samples = []
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        next_t = time.monotonic()
        while not self._stop.is_set():
            now = time.monotonic()
            row = {}
            for pid in self.pids:
                try:
                    row[pid] = (read_proc_cpu_ticks(pid), read_proc_rss_kb(pid))
                except (FileNotFoundError, ProcessLookupError):
                    row[pid] = None
            self.samples.append((now, row))
            next_t += self.interval_s
            sleep_for = next_t - time.monotonic()
            if sleep_for > 0:
                time.sleep(sleep_for)

    def start(self):
        self._thread.start()

    def stop(self):
        self._stop.set()
        self._thread.join(timeout=5)


def summarize_cpu_mem(samples, pids, label):
    if len(samples) < 2:
        return {"label": label, "error": "not enough samples", "n": len(samples)}
    t0, row0 = samples[0]
    t1, row1 = samples[-1]
    wall = t1 - t0
    total_ticks_delta = 0
    per_pid = {}
    for pid in pids:
        v0 = row0.get(pid)
        v1 = row1.get(pid)
        if v0 is None or v1 is None:
            per_pid[pid] = {"error": "process missing at start or end of window"}
            continue
        ticks_delta = v1[0] - v0[0]
        total_ticks_delta += ticks_delta
        cpu_pct = (ticks_delta / HZ) / wall * 100.0 if wall > 0 else float("nan")
        rss_series = [row.get(pid)[1] for _, row in samples if row.get(pid) is not None]
        per_pid[pid] = {
            "cpu_percent_avg_over_window": round(cpu_pct, 3),
            "rss_kb_avg": round(statistics.mean(rss_series), 1) if rss_series else None,
            "rss_kb_max": max(rss_series) if rss_series else None,
            "rss_kb_min": min(rss_series) if rss_series else None,
        }
    combined_cpu_pct = (total_ticks_delta / HZ) / wall * 100.0 if wall > 0 else float("nan")
    combined_rss_series = []
    for _, row in samples:
        vals = [row.get(pid)[1] for pid in pids if row.get(pid) is not None]
        if vals:
            combined_rss_series.append(sum(vals))
    return {
        "label": label,
        "wall_seconds": round(wall, 3),
        "n_samples": len(samples),
        "combined_cpu_percent_avg": round(combined_cpu_pct, 3),
        "combined_rss_kb_avg": round(statistics.mean(combined_rss_series), 1) if combined_rss_series else None,
        "combined_rss_kb_max": max(combined_rss_series) if combined_rss_series else None,
        "per_pid": per_pid,
    }


# ----------------------------------------------------------------------------
# GPU sampling via intel_gpu_top -- copied verbatim from the stock phase's driver.py.
# ----------------------------------------------------------------------------


def run_gpu_sampler_blocking(duration_s, out_path, interval_ms=GPU_SAMPLE_INTERVAL_MS):
    cmd = ["timeout", f"{duration_s}s", "sudo", "-n", "intel_gpu_top", "-J",
           "-s", str(interval_ms), "-o", out_path]
    subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def parse_concat_json_objects(text):
    t = text.strip()
    if t.startswith("["):
        t = t[1:]
    if t.endswith("]"):
        t = t[:-1]
    dec = json.JSONDecoder()
    objs = []
    idx, n = 0, len(t)
    while idx < n:
        while idx < n and t[idx] in " \t\r\n,":
            idx += 1
        if idx >= n:
            break
        try:
            obj, end = dec.raw_decode(t, idx)
        except json.JSONDecodeError:
            break
        objs.append(obj)
        idx = end
    return objs


def summarize_gpu(objs, pids_str, label):
    def stats_for(sub):
        if not sub:
            return {"n": 0}
        render_busy = [o["engines"]["Render/3D"]["busy"] for o in sub]
        pkg_power = [o["power"]["Package"] for o in sub]
        client_busy = []
        for o in sub:
            for c in o.get("clients", {}).values():
                if c.get("pid") in pids_str:
                    client_busy.append(float(c["engine-classes"]["Render/3D"]["busy"]))
        return {
            "n": len(sub),
            "system_render_busy_pct_avg": round(statistics.mean(render_busy), 3),
            "system_render_busy_pct_max": round(max(render_busy), 3),
            "package_power_w_avg": round(statistics.mean(pkg_power), 3),
            "neovibe_process_render_busy_pct_avg": (
                round(statistics.mean(client_busy), 3) if client_busy else None
            ),
            "neovibe_process_render_busy_pct_max": (
                round(max(client_busy), 3) if client_busy else None
            ),
            "neovibe_process_client_samples_seen": len(client_busy),
        }

    return {"label": label, "overall": stats_for(objs)}


# ----------------------------------------------------------------------------
# stdout marker synchronization
# ----------------------------------------------------------------------------

MARKER_RE = re.compile(r"^NEOVIBE_(PHASE:\S+|READY|RESULT_JSON:.*|TIMING:\S+|FAILED:.*|WINDOW_PRESENTED)")


class StdoutWatcher:
    """Continuously drains the subprocess's stdout (never let the pipe back up -- see this file's
    own module doc) into a timestamped log, and lets the main thread block on a specific marker."""

    def __init__(self, proc):
        self.proc = proc
        self.lines = []  # (t_perf_counter, line)
        self.events = {}  # marker name -> threading.Event
        self.results_json_line = None
        self._lock = threading.Lock()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        for raw in self.proc.stdout:
            t = time.perf_counter()
            line = raw.rstrip("\n")
            with self._lock:
                self.lines.append((t, line))
            if line.startswith("NEOVIBE_PHASE:"):
                name = line[len("NEOVIBE_PHASE:"):]
                self._fire(name, t)
            elif line.startswith("NEOVIBE_READY"):
                self._fire("READY", t)
            elif line.startswith("NEOVIBE_RESULT_JSON:"):
                with self._lock:
                    self.results_json_line = line[len("NEOVIBE_RESULT_JSON:"):]
                self._fire("RESULT_JSON", t)
            elif line.startswith("NEOVIBE_FAILED:"):
                self._fire("FAILED", t)

    def _fire(self, name, t):
        with self._lock:
            ev = self.events.setdefault(name, threading.Event())
            self.events.setdefault(name + "__t", t)
        ev.set()

    def start(self):
        self._thread.start()

    def wait_for(self, marker, timeout):
        with self._lock:
            ev = self.events.setdefault(marker, threading.Event())
        ok = ev.wait(timeout)
        with self._lock:
            t = self.events.get(marker + "__t")
        return (ok, t)

    def dump(self):
        with self._lock:
            return list(self.lines)


# ----------------------------------------------------------------------------
# Main
# ----------------------------------------------------------------------------


def preflight_check():
    out = subprocess.run(["pgrep", "-af", "nvim|perf_baseline"], capture_output=True, text=True)
    return out.stdout.strip()


def kill_pid_tree(pid):
    try:
        p = psutil.Process(pid)
    except psutil.NoSuchProcess:
        return
    children = p.children(recursive=True)
    for c in children:
        try:
            c.terminate()
        except psutil.NoSuchProcess:
            pass
    try:
        p.terminate()
    except psutil.NoSuchProcess:
        pass
    gone, alive = psutil.wait_procs(children + [p], timeout=5)
    for a in alive:
        try:
            a.kill()
        except psutil.NoSuchProcess:
            pass


def main():
    result = {}

    print("=== preflight ===", flush=True)
    pre = preflight_check()
    result["preflight_pgrep"] = pre
    print(pre or "(none)", flush=True)

    for p in (SANITY_FILE, RESULTS_JSON_FROM_RUST):
        if os.path.exists(p):
            os.remove(p)

    env = dict(os.environ)
    env["NEOVIBE_WORKLOAD_FILE"] = WORKLOAD_FILE
    env["NEOVIBE_SANITY_FILE"] = SANITY_FILE
    env["NEOVIBE_RESULTS_JSON"] = RESULTS_JSON_FROM_RUST

    cmd = [PERF_BASELINE_BIN]
    result["launch_cmd"] = cmd
    result["workload_file"] = WORKLOAD_FILE

    print("=== launching perf_baseline (neovibe's own LiveHarness stack) ===", flush=True)
    t0 = time.perf_counter()
    proc = subprocess.Popen(
        cmd, env=env, cwd=NEOVIBE_REPO,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, bufsize=1,
    )
    neovibe_pid = proc.pid

    watcher = StdoutWatcher(proc)
    watcher.start()

    try:
        ok, t_ready = watcher.wait_for("READY", STARTUP_TIMEOUT_S)
        if not ok:
            raise RuntimeError("perf_baseline never reported NEOVIBE_READY -- see stdout log")
        startup_s = t_ready - t0
        result["startup"] = {
            "t0_to_ready_s": round(startup_s, 4),
            "methodology": (
                "t0 = time.perf_counter() immediately before subprocess.Popen([perf_baseline]). "
                "t_ready = perf_counter() at the moment this driver's stdout-reader thread receives "
                "the NEOVIBE_READY line, which perf_baseline prints the instant "
                "LiveHarness::is_ready() first flips true inside its own render callback -- the "
                "direct analog of the stock driver's 'first nvim_list_uis() returning non-empty' "
                "signal, per this phase's task instructions. Same caveat as the stock report: this "
                "is an API-visible readiness signal, not confirmed compositor pixel-paint. UNLIKE "
                "the stock number, this includes real GTK4 application activation (toolkit init, "
                "Wayland connection, D-Bus registration) and Skia/GL context creation BEFORE nvim is "
                "even launched -- see perf_baseline's own NEOVIBE_TIMING breakdown lines for the "
                "attribution (this driver also extracts them below)."
            ),
        }
        print(json.dumps(result["startup"], indent=2), flush=True)

        # Extract the internal NEOVIBE_TIMING:* breakdown lines (all timestamped relative to the
        # Rust process's own t_process_start, i.e. self-contained -- included for the "where did
        # the startup time go" attribution the report discusses).
        timing_breakdown = {}
        for _, line in watcher.dump():
            if line.startswith("NEOVIBE_TIMING:"):
                rest = line[len("NEOVIBE_TIMING:"):]
                name, _, val = rest.partition(" t=")
                try:
                    timing_breakdown[name] = float(val)
                except ValueError:
                    pass
        result["startup"]["internal_timing_breakdown_s"] = timing_breakdown

        ok, _ = watcher.wait_for("FILE_OPENED", PHASE_TIMEOUT_S)
        if not ok:
            raise RuntimeError("perf_baseline never reported FILE_OPENED")

        # Discover the real nvim --embed child now that LiveHarness::with_options has returned
        # (confirmed by READY already having fired, which requires the harness to already exist).
        nvim_pid = None
        try:
            children = psutil.Process(neovibe_pid).children(recursive=True)
            for c in children:
                if "nvim" in c.name():
                    nvim_pid = c.pid
                    break
        except psutil.NoSuchProcess:
            pass
        result["neovibe_pid"] = neovibe_pid
        result["nvim_child_pid"] = nvim_pid
        print(f"neovibe_pid={neovibe_pid} nvim_child_pid={nvim_pid}", flush=True)

        pids = [neovibe_pid] + ([nvim_pid] if nvim_pid else [])
        pids_str = {str(p) for p in pids}

        ok, _ = watcher.wait_for("WARMUP_DONE", PHASE_TIMEOUT_S)
        if not ok:
            raise RuntimeError("perf_baseline never reported WARMUP_DONE")

        # ---------------- IDLE PHASE ----------------
        ok, t_idle_start = watcher.wait_for("IDLE_START", PHASE_TIMEOUT_S)
        if not ok:
            raise RuntimeError("perf_baseline never reported IDLE_START")
        print(f"=== idle sampling for {IDLE_DURATION_S}s ===", flush=True)
        idle_gpu_path = f"{RESULTS_DIR}/idle_gpu.json"
        idle_gpu_thread = threading.Thread(
            target=run_gpu_sampler_blocking, args=(IDLE_DURATION_S + 1, idle_gpu_path))
        idle_gpu_thread.start()
        idle_sampler = ProcSampler(pids, IDLE_SAMPLE_INTERVAL_S)
        idle_sampler.start()

        ok, t_idle_end = watcher.wait_for("IDLE_END", IDLE_DURATION_S + 10)
        if not ok:
            raise RuntimeError("perf_baseline never reported IDLE_END")
        # Stop the idle sampler and arm the workload samplers IMMEDIATELY -- perf_baseline fires
        # TYPING_START on its own clock right after IDLE_END with no gap, so any slower
        # idle-phase bookkeeping (joining the idle GPU subprocess, which deliberately outlives the
        # nominal window by +1s as a safety margin, parsing its JSON, printing) MUST happen after
        # arming the workload samplers, not before. An earlier version of this driver did the slow
        # bookkeeping first and lost ~1s of the typing sub-phase's CPU/GPU samples to it (caught by
        # cross-checking summarize_cpu_mem's wall_seconds against the raw stdout marker log's own
        # timestamps -- see PHASE_REPORT.md).
        idle_sampler.stop()

        gpu_window_s = 13.0  # generous upper bound for typing(~5.5s)+scroll(~3.8s)+barriers
        workload_gpu_path = f"{RESULTS_DIR}/workload_gpu.json"
        workload_gpu_thread = threading.Thread(
            target=run_gpu_sampler_blocking, args=(gpu_window_s, workload_gpu_path))
        workload_sampler = ProcSampler(pids, WORKLOAD_SAMPLE_INTERVAL_S)
        workload_gpu_thread.start()
        workload_sampler.start()

        # ---------------- IDLE bookkeeping (safe to be slow now that the workload samplers are
        # already running) ----------------
        idle_gpu_thread.join(timeout=IDLE_DURATION_S + 5)
        result["idle"] = summarize_cpu_mem(idle_sampler.samples, pids, "idle")
        result["idle"]["wall_seconds_from_markers"] = round(t_idle_end - t_idle_start, 3)
        idle_gpu_objs = parse_concat_json_objects(open(idle_gpu_path).read())
        result["idle"]["gpu"] = summarize_gpu(idle_gpu_objs, pids_str, "idle")
        print(json.dumps(result["idle"], indent=2), flush=True)

        # ---------------- WORKLOAD PHASE (typing then scroll) ----------------
        # TYPING_START should already have fired (it follows IDLE_END with no gap on the Rust
        # side) by the time we get here -- wait_for returns instantly and hands back the marker's
        # actual fire time, which is what we anchor the workload wall-clock window to (NOT a fresh
        # time.perf_counter() call here, which -- per the bug this comment block documents -- can
        # be well after the fact).
        print("=== workload: waiting for TYPING_START ===", flush=True)
        ok, t_workload_start = watcher.wait_for("TYPING_START", PHASE_TIMEOUT_S)
        if not ok:
            raise RuntimeError("perf_baseline never reported TYPING_START")

        ok, t_typing_end = watcher.wait_for("TYPING_END", PHASE_TIMEOUT_S)
        if not ok:
            raise RuntimeError("perf_baseline never reported TYPING_END")
        ok, t_scroll_end = watcher.wait_for("SCROLL_END", PHASE_TIMEOUT_S)
        if not ok:
            raise RuntimeError("perf_baseline never reported SCROLL_END")

        workload_sampler.stop()
        workload_gpu_thread.join(timeout=gpu_window_s + 5)

        typing_wall_s = t_typing_end - t_workload_start
        scroll_wall_s = t_scroll_end - t_typing_end
        total_wall_s = t_scroll_end - t_workload_start

        result["workload"] = {
            "typing_wall_seconds": round(typing_wall_s, 3),
            "scroll_wall_seconds": round(scroll_wall_s, 3),
            "total_wall_seconds": round(total_wall_s, 3),
        }

        cpu = summarize_cpu_mem(workload_sampler.samples, pids, "workload_full")
        split_idx = None
        for i, (t, _) in enumerate(workload_sampler.samples):
            if t - workload_sampler.samples[0][0] >= typing_wall_s:
                split_idx = i
                break
        result["workload"]["cpu_mem_full"] = cpu
        if split_idx and split_idx > 1:
            result["workload"]["cpu_mem_typing"] = summarize_cpu_mem(
                workload_sampler.samples[:split_idx + 1], pids, "typing")
            result["workload"]["cpu_mem_scroll"] = summarize_cpu_mem(
                workload_sampler.samples[split_idx:], pids, "scroll")

        workload_gpu_objs = parse_concat_json_objects(open(workload_gpu_path).read())
        gpu_split_idx = int((typing_wall_s * 1000) / GPU_SAMPLE_INTERVAL_MS)
        result["workload"]["gpu"] = {
            "label": "workload",
            "overall": summarize_gpu(workload_gpu_objs, pids_str, "workload")["overall"],
            "first_part_typing": summarize_gpu(workload_gpu_objs[:gpu_split_idx], pids_str, "typing")["overall"],
            "second_part_scroll": summarize_gpu(workload_gpu_objs[gpu_split_idx:], pids_str, "scroll")["overall"],
        }
        print(json.dumps(result["workload"], indent=2), flush=True)

        ok, _ = watcher.wait_for("SANITY_DONE", PHASE_TIMEOUT_S)
        if not ok:
            print("WARNING: SANITY_DONE never observed", flush=True)

        # ---------------- Frame timing (already instrumented internally) ----------------
        ok, _ = watcher.wait_for("SHUTDOWN_DONE", PHASE_TIMEOUT_S)
        if not ok:
            print("WARNING: SHUTDOWN_DONE never observed within timeout", flush=True)

        frame_stats = None
        if watcher.results_json_line:
            try:
                frame_stats = json.loads(watcher.results_json_line)
            except json.JSONDecodeError:
                pass
        if frame_stats is None and os.path.exists(RESULTS_JSON_FROM_RUST):
            try:
                frame_stats = json.loads(open(RESULTS_JSON_FROM_RUST).read())
            except (json.JSONDecodeError, OSError):
                pass
        result["frame_timing"] = frame_stats

        # ---------------- Sanity ----------------
        sanity = {}
        if os.path.exists(SANITY_FILE):
            lines = open(SANITY_FILE).read().splitlines()
            if len(lines) >= 2:
                sanity["final_line_count"] = int(lines[0])
                sanity["final_cursor_line"] = int(lines[1])
        result["sanity"] = sanity
        print(json.dumps(result["sanity"], indent=2), flush=True)

    finally:
        print("=== cleanup ===", flush=True)
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            pass
        kill_pid_tree(neovibe_pid)
        time.sleep(0.5)
        post = preflight_check()
        result["postflight_pgrep"] = post
        print(post or "(none)", flush=True)

        with open(f"{RESULTS_DIR}/stdout_lines.log", "w") as f:
            for t, line in watcher.dump():
                f.write(f"{t:.4f}\t{line}\n")

    with open(f"{RESULTS_DIR}/summary.json", "w") as f:
        json.dump(result, f, indent=2)
    print("=== wrote results/summary.json ===", flush=True)


if __name__ == "__main__":
    main()
