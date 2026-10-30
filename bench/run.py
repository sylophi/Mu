#!/usr/bin/env python3
"""Times how soon what Mu's asked for is on screen, and fails if that's slower than its target, or
than the base build when there is one.

    bench/run.py [--rounds N] [--base BINARY] BINARY

Each BINARY is Mu built with `--features bench`, which says what it times in src/bench.rs. A round
runs each build once, taking turns at going first, so a busy moment on the machine slows both.
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path

# How much slower than the base a timing's median may be before it fails: both by this share and by
# this many milliseconds, so the noise in a fast one doesn't fail it. It has to be slower in every
# round too, so one busy moment doesn't.
SLOWER_SHARE = 0.15
SLOWER_MS = 0.5

PLIST = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>io.github.sylophi.mu.bench</string>
<key>CFBundleName</key><string>Mu Bench</string>
<key>CFBundleExecutable</key><string>mu</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSMinimumSystemVersion</key><string>27.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"""


def bundle(binary, app):
    """Wraps `binary` in an app, as macOS brings an app opened with `open` to the front."""
    (app / "Contents/MacOS").mkdir(parents=True)
    shutil.copy(binary, app / "Contents/MacOS/mu")
    (app / "Contents/Info.plist").write_text(PLIST)
    subprocess.run(["codesign", "--force", "--sign", "-", app], check=True, capture_output=True)


def run(app, out):
    log = out.with_suffix(".log")
    env = ["--env", f"MU_BENCH={out}"]
    # Its windows aren't put back as they were, as after a run that failed.
    args = ["--args", "-ApplePersistenceIgnoreState", "YES"]
    subprocess.run(["open", "-n", "-W", *env, "--stdout", log, "--stderr", log, app, *args], check=True, timeout=600)
    if not out.exists():
        sys.exit(f"{app.stem} wrote no timings. Its log:\n{log.read_text()}")
    return json.loads(out.read_text())


def p90(samples):
    return statistics.quantiles(samples, n=10)[-1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary")
    parser.add_argument("--base")
    parser.add_argument("--rounds", type=int, default=3)
    args = parser.parse_args()

    builds = {"head": args.binary} | ({"base": args.base} if args.base else {})
    samples = {}
    with tempfile.TemporaryDirectory() as temp:
        temp = Path(temp)
        apps = {}
        for name, binary in builds.items():
            apps[name] = temp / f"{name}.app"
            bundle(binary, apps[name])
        # Not counted: Quick Look and the disk keep what the first run asks for, so it's the slowest.
        warm_up = run(apps["head"], temp / "warm-up.json")
        targets = {timing["name"]: timing["target"] for timing in warm_up["timings"]}
        for round in range(args.rounds):
            order = list(apps) if round % 2 == 0 else list(reversed(apps))
            for name in order:
                report = run(apps[name], temp / f"{name}-{round}.json")
                print(f"Round {round + 1} of {args.rounds}: {name}", file=sys.stderr)
                for timing in report["timings"]:
                    samples.setdefault((timing["name"], name), []).append(timing["samples"])

    chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout
    failures = []
    lines = [
        f"On an {chip.strip()} with a {warm_up['refresh_rate']:g} Hz display, {args.rounds} rounds:",
        "",
        "| | Target | Median | p90 | Base median | Change |",
        "|---|--:|--:|--:|--:|--:|",
    ]
    for name, target in targets.items():
        head_rounds = samples[name, "head"]
        head_samples = sum(head_rounds, [])
        head = statistics.median(head_samples)
        cells = [name, f"{target:g} ms", f"{head:.1f} ms", f"{p90(head_samples):.1f} ms"]
        if head > target:
            failures.append(f"{name} took {head:.1f} ms, over its target of {target:g} ms")
        if base_rounds := samples.get((name, "base")):
            base = statistics.median(sum(base_rounds, []))
            cells += [f"{base:.1f} ms", f"{(head - base) / base:+.0%}"]
            rounds = zip(head_rounds, base_rounds)
            always = all(statistics.median(this) > statistics.median(that) for this, that in rounds)
            if always and head > base * (1 + SLOWER_SHARE) and head - base > SLOWER_MS:
                failures.append(f"{name} took {head:.1f} ms, up from {base:.1f} ms")
        else:
            cells += ["", ""]
        lines.append("| " + " | ".join(cells) + " |")
    lines += [""] + [f"- {failure}" for failure in failures]

    report = "\n".join(lines) + "\n"
    print(report)
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a") as file:
            file.write(report)
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
