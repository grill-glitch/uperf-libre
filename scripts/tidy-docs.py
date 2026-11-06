#!/usr/bin/env python3
"""Move the per-milestone documents under docs/records/ and fix every reference.

Why this exists as a script: the moves are mechanical but they touch ~60 files of
references (code comments, shell scripts, READMEs, the skills' links), and a missed
one leaves a dangling path that only shows up when someone follows it. Running it
twice is a no-op.
"""
import re
import shutil
import subprocess
import pathlib

root = pathlib.Path(__file__).resolve().parents[1]
docs = root / "docs"
records = docs / "records"
records.mkdir(exist_ok=True)

# --------------------------------------------------------------- the moves
moved = {}          # old repo-relative path -> new repo-relative path

# The old names are still known after the move (that is the point of the rewrite), so the
# mapping is built from the destination tree, not from what this run happens to move.
RECORD_NAMES = [
    "cpufreq-probe-alioth.txt", "m0-evidence.md", "m1-device-evidence.md",
    "m1-evidence.md", "m1-static-reverse.md", "m10-log-cap.md", "m11-10-32bit.md",
    "m11-11-ebpf-uprobe-frame-leg.md", "m11-6-thermal-pl1.md", "m11-7-ctl-socket.md",
    "m11-8-recording.md", "m2-evidence.md", "m3-evidence.md", "m4-evidence.md",
    "m5-cpu-governor.md", "m6-sched-evidence.md", "m6b-evidence.md", "m6d-evidence.md",
    "m7-evidence.md", "m8-sfanalysis-reverse.md", "m9-boot-run-alioth.txt",
    "m9-device-run-alioth.txt", "m9-watchdog.md", "webui.md",
]
for name in RECORD_NAMES:
    moved[f"docs/{name}"] = f"docs/records/{name}"
moved["docs/audit/unsafe-audit-report.md"] = "docs/audit/unsafe-audit-report.md"
moved["docs/history/wayfinder-fog.md"] = "docs/history/wayfinder-fog.md"
moved["docs/history/milestone-log.md"] = "docs/history/milestone-log.md"


def move(src: pathlib.Path, dst: pathlib.Path):
    if not src.exists():
        return
    if src.is_dir() and not any(src.iterdir()):
        # An empty directory is not tracked by git and cannot be moved; drop it.
        src.rmdir()
        print(f"  {src.relative_to(root).as_posix()}/ was empty, removed")
        return
    if dst.exists():
        print(f"  skip (already there): {dst.relative_to(root).as_posix()}")
        return
    dst.parent.mkdir(parents=True, exist_ok=True)
    ok = subprocess.run(
        ["git", "-C", str(root), "mv", str(src), str(dst)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    ).returncode == 0
    if not ok:
        # Untracked files cannot `git mv`; a plain rename is enough (a later `git add`
        # picks the new path up).
        src.rename(dst)
    print(f"  {src.relative_to(root).as_posix()} -> {dst.relative_to(root).as_posix()}")


records.mkdir(exist_ok=True)
for old, new in list(moved.items()):
    move(root / old, root / new)

# --------------------------------------------------------------- reference rewrite
# Longest first so `docs/records/m1-evidence.md` is not clipped by a shorter prefix.
pairs = sorted(moved.items(), key=lambda kv: -len(kv[0]))
scan_suffixes = {".md", ".rs", ".sh", ".toml", ".js", ".py", ".cpp", ".h", ".json", ".txt", ".html"}
skip_dirs = {"build", "target", "node_modules", ".git", "webroot"}
changed = 0

for path in root.rglob("*"):
    if not path.is_file() or path.suffix not in scan_suffixes:
        continue
    if any(part in skip_dirs for part in path.relative_to(root).parts):
        continue
    try:
        text = path.read_text()
    except (UnicodeDecodeError, OSError):
        continue
    new = text
    for old, dst in pairs:
        # `docs/x` anywhere, and the bare root-level names for the two moved root files
        new = new.replace(old, dst)
        if old.startswith("docs/") and "docs/" in old:
            new = new.replace(old.split("/", 1)[1], dst.split("/", 1)[1]) if False else new
        if old in ("docs/audit/unsafe-audit-report.md", "docs/history/wayfinder-fog.md"):
            new = new.replace(f"`{old}`", f"`{dst}`")
    if new != text:
        path.write_text(new)
        changed += 1
print(f"  references rewritten in {changed} file(s)")

# The two files that were always named without the `docs/` prefix in prose.
for path in (root / "AGENT.md", root / "README.md", root / "README.zh-CN.md"):
    if not path.exists():
        continue
    t = path.read_text()
    n = (t.replace("UNCLEAR", "UNCLEAR")
          .replace("docs/history/milestone-log.md", "docs/history/milestone-log.md"))
    if n != t:
        path.write_text(n)
        print(f"  prose fixed in {path.name}")
