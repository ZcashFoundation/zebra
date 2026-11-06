#!/usr/bin/env python3
"""Build one dependency train: run `cargo update`, keep only that train's crates, write the PR body.

Train A moves every crate except the consensus-sensitive set; Train B moves only that set.
See book/src/dev/dependency-trains.md and https://github.com/ZcashFoundation/zebra/issues/9630.

Never commits or pushes: the workflow's create-pull-request step does that.
"""

import argparse
import datetime
import fnmatch
import json
import os
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

USER_AGENT = "zebra-dependency-train (github.com/ZcashFoundation/zebra)"
CRATES_IO = "https://crates.io/api/v1/crates"
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
BOOK_PAGE = "https://zebra.zfnd.org/dev/dependency-trains.html"
ISSUE_URL = "https://github.com/ZcashFoundation/zebra/issues/9630"
ROOT = Path(__file__).resolve().parents[2]
COLLAPSE_ROWS = 40

TRAINS = {
    "a": ("Train A", "everything except the consensus-sensitive crates"),
    "b": ("Train B", "only the consensus-sensitive crates"),
}


def log(message):
    print(message, file=sys.stderr, flush=True)


def warn(message):
    prefix = "::warning::" if os.environ.get("GITHUB_ACTIONS") else "warning: "
    log(f"{prefix}{message}")


def cargo(*args):
    """Runs a cargo command in the repository root and returns the CompletedProcess."""
    command = ["cargo", *args]
    log("$ " + " ".join(command))
    return subprocess.run(command, cwd=ROOT, capture_output=True, text=True)


def cargo_ok(*args):
    result = cargo(*args)
    if result.returncode != 0:
        warn(f"`cargo {' '.join(args)}` failed: {cargo_error(result)}")
    return result.returncode == 0


def cargo_error(result):
    lines = result.stderr.strip().splitlines()
    return lines[-1] if lines else "no output"


def precise_all(targets):
    """Moves (name, from_version, to_version) crates with `--precise` until no more progress is made.

    One move can drag or block another, so every pending move is retried after each pass;
    a crate already at its target is skipped. Returns the moves that never applied, with cargo's error.
    """
    pending = {(name, to_version): from_version for name, from_version, to_version in targets}
    errors = {}
    while pending:
        lock = read_lock()
        progressed = False
        for (name, to_version), from_version in list(pending.items()):
            versions = lock.get(name, {})
            if to_version in versions:
                del pending[(name, to_version)]
                progressed = True
                continue
            if from_version in versions:
                spec = f"{name}@{from_version}"
            elif len(versions) == 1:
                spec = f"{name}@{next(iter(versions))}"
            else:
                spec = name
            result = cargo("update", "-p", spec, "--precise", to_version)
            if result.returncode == 0:
                del pending[(name, to_version)]
                progressed = True
            else:
                errors[(name, to_version)] = cargo_error(result)
        if not progressed:
            break
    return [(name, from_version, to_version, errors.get((name, to_version), "unknown"))
            for (name, to_version), from_version in pending.items()]


# Version handling: enough of semver to order versions and detect breaking lines.


def parse_version(text):
    """Returns a sortable tuple; pre-releases order before their release."""
    core, _, build = text.partition("+")
    core, _, pre = core.partition("-")
    numbers = [int(part) for part in core.split(".")]
    while len(numbers) < 3:
        numbers.append(0)
    pre_parts = tuple(
        (0, int(part)) if part.isdigit() else (1, part) for part in pre.split(".") if part
    )
    return (*numbers[:3], 0 if pre_parts else 1, pre_parts)


def compat_key(text):
    """The semver-compatible range a version belongs to, with cargo's 0.x rules."""
    major, minor, patch, *_ = parse_version(text)
    if major > 0:
        return (major,)
    if minor > 0:
        return (0, minor)
    return (0, 0, patch)


def requirement_base(requirement):
    """The version a requirement like `^1.2`, `0.4.40` or `=1.0.0` starts from."""
    first = requirement.split(",")[0].strip().lstrip("^=~>< ")
    if first == "*" or not first:
        return None
    return first.replace(".*", "")


# Lockfile handling.


def read_lock():
    """Maps crate name to {version: source} for every package in Cargo.lock."""
    with open(ROOT / "Cargo.lock", "rb") as file:
        lock = tomllib.load(file)
    packages = {}
    for package in lock.get("package", []):
        packages.setdefault(package["name"], {})[package["version"]] = package.get("source")
    return packages


def diff_locks(old, new):
    """Lists (name, old_version, new_version) moves; None on one side means added or removed."""
    changes = []
    for name in sorted(set(old) | set(new)):
        removed = sorted(set(old.get(name, {})) - set(new.get(name, {})), key=parse_version)
        added = sorted(set(new.get(name, {})) - set(old.get(name, {})), key=parse_version)
        # Pair versions in the same compatible range first, then whatever is left in order.
        for old_version in list(removed):
            match = next((v for v in added if compat_key(v) == compat_key(old_version)), None)
            if match is not None:
                changes.append((name, old_version, match))
                removed.remove(old_version)
                added.remove(match)
        for old_version, new_version in zip(list(removed), list(added)):
            changes.append((name, old_version, new_version))
            removed.remove(old_version)
            added.remove(new_version)
        changes.extend((name, version, None) for version in removed)
        changes.extend((name, None, version) for version in added)
    return changes


def is_registry(lock, name, version):
    return lock.get(name, {}).get(version) == REGISTRY


def matches_any(name, patterns):
    return any(fnmatch.fnmatchcase(name, pattern) for pattern in patterns)


# crates.io lookups, cached and throttled to about one request per second.


class CratesIo:
    def __init__(self):
        self.cache = {}
        self.last_request = 0.0

    def get(self, path):
        if path in self.cache:
            return self.cache[path]
        wait = 1.0 - (time.monotonic() - self.last_request)
        if wait > 0:
            time.sleep(wait)
        request = urllib.request.Request(f"{CRATES_IO}/{path}", headers={"User-Agent": USER_AGENT})
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                data = json.load(response)
        except (urllib.error.URLError, json.JSONDecodeError, TimeoutError) as error:
            warn(f"crates.io lookup failed for {path}: {error}")
            data = None
        self.last_request = time.monotonic()
        self.cache[path] = data
        return data

    def published_at(self, name, version):
        data = self.get(f"{name}/{version}")
        if not data or "version" not in data:
            return None
        return datetime.datetime.fromisoformat(data["version"]["created_at"].replace("Z", "+00:00"))

    def max_stable_version(self, name):
        data = self.get(name)
        if not data or "crate" not in data:
            return None
        return data["crate"].get("max_stable_version")


# The train steps.


def apply_cooling_off(original, api, cooling_days, held):
    """Reverts bumps to versions published less than `cooling_days` ago. Returns the final diff."""
    now = datetime.datetime.now(datetime.timezone.utc)
    cutoff = now - datetime.timedelta(days=cooling_days)
    tried = set()
    # A revert can move other crates, so re-diff until nothing new needs holding.
    for _ in range(5):
        current = read_lock()
        fresh = []
        for name, old_version, new_version in diff_locks(original, current):
            if old_version is None or new_version is None or (name, new_version) in tried:
                continue
            if not is_registry(current, name, new_version):
                continue
            tried.add((name, new_version))
            published = api.published_at(name, new_version)
            if published is None or published < cutoff:
                continue
            age = (now - published).days
            log(f"{name} {new_version} was published {age} day(s) ago, holding for cooling-off")
            fresh.append((name, new_version, old_version, age))
        if not fresh:
            break
        leftovers = precise_all((name, new, old) for name, new, old, _ in fresh)
        for name, new_version, old_version, error in leftovers:
            warn(f"could not hold {name} {new_version} back to {old_version}, shipping it: {error}")
        stuck = {(name, new_version) for name, new_version, _, _ in leftovers}
        held.extend(
            (name, old_version, new_version, age)
            for name, new_version, old_version, age in fresh
            if (name, new_version) not in stuck
        )
    return diff_locks(original, read_lock())


def build_train_a(original, changes, consensus):
    """Reverts every consensus crate the full update moved."""
    targets = [
        (name, new_version, old_version)
        for name, old_version, new_version in changes
        if matches_any(name, consensus) and old_version and new_version
    ]
    for name, new_version, old_version, error in precise_all(targets):
        warn(f"could not keep consensus crate {name} at {old_version}: {error}")
    stuck = [
        change
        for change in diff_locks(original, read_lock())
        if matches_any(change[0], consensus)
    ]
    if stuck:
        names = ", ".join(f"{name} {old} -> {new}" for name, old, new in stuck)
        log(f"::error::Train A cannot leave these consensus crates alone: {names}")
        log("Find the crate dragging them with `cargo tree -i <crate>@<version>` and pin it in .github/dependency-trains.toml.")
        sys.exit(1)


def build_train_b(original_bytes, original, changes, consensus):
    """Starts over from the original lock and applies only the consensus moves."""
    (ROOT / "Cargo.lock").write_bytes(original_bytes)
    targets = [
        (name, old_version, new_version)
        for name, old_version, new_version in changes
        if matches_any(name, consensus) and old_version and new_version
    ]
    for name, old_version, new_version, error in precise_all(targets):
        warn(f"could not move consensus crate {name} to {new_version}: {error}")


def apply_pins(pinned, lock):
    """Pins crates from `[pinned]` back to an exact version. Returns the pins that applied."""
    targets = []
    for name, version in pinned.items():
        if name not in lock:
            warn(f"pinned crate {name} is not in Cargo.lock, skipping")
            continue
        targets.append((name, next(iter(lock[name])), version))
    failed = precise_all(targets)
    for name, _, version, error in failed:
        warn(f"could not pin {name} to {version}: {error}")
    stuck = {(name, version) for name, _, version, _ in failed}
    return [(name, version) for name, _, version in targets if (name, version) not in stuck]


def held_back_majors(api, lock):
    """Direct workspace dependencies whose newest stable release is behind a breaking line."""
    with open(ROOT / "Cargo.toml", "rb") as file:
        manifest = tomllib.load(file)
    members = {Path(member).name for member in manifest["workspace"].get("members", [])}
    rows = []
    seen = set()
    for key, spec in manifest["workspace"].get("dependencies", {}).items():
        if isinstance(spec, dict):
            if "path" in spec or "git" in spec:
                continue
            name, requirement = spec.get("package", key), spec.get("version")
        else:
            name, requirement = key, spec
        base = requirement_base(requirement or "")
        if name in members or name in seen or base is None or name not in lock:
            continue
        seen.add(name)
        locked = next(
            (v for v in lock[name] if compat_key(v) == compat_key(base)),
            max(lock[name], key=parse_version),
        )
        newest = api.max_stable_version(name)
        if newest is None:
            continue
        if compat_key(newest) != compat_key(locked) and parse_version(newest) > parse_version(locked):
            rows.append((name, locked, newest))
    return rows


# Output.


def fragment_text(train, moved_count):
    title = TRAINS[train][0]
    now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000000000+00:00")
    return (
        "project: zebrad\n"
        "kind: Changed\n"
        f"body: Updated dependencies via dependency {title} ({moved_count} crates).\n"
        f"time: {now}\n"
    )


def table(headers, rows):
    lines = ["| " + " | ".join(headers) + " |", "| " + " | ".join("---" for _ in headers) + " |"]
    lines.extend("| " + " | ".join(row) + " |" for row in rows)
    return "\n".join(lines)


def collapsible(summary, body, count):
    if count <= COLLAPSE_ROWS:
        return body
    return f"<details>\n<summary>{summary}</summary>\n\n{body}\n\n</details>"


def body_text(train, changes, held, pins, majors, config_path):
    title, scope = TRAINS[train]
    moved = [(n, o, nv) for n, o, nv in changes if o and nv]
    added = [(n, nv) for n, o, nv in changes if o is None]
    removed = [(n, o) for n, o, nv in changes if nv is None]

    parts = [
        f"## Dependency {title}: {scope}",
        "",
        f"This PR is the monthly `cargo update` for {scope}, generated by the dependency train "
        f"([#9630]({ISSUE_URL}), [playbook]({BOOK_PAGE})). "
        "It only carries semver-compatible bumps; breaking upgrades stay manual.",
        "",
        f"### Moved crates ({len(moved)})",
        "",
    ]
    if moved:
        rows = [(f"`{n}`", f"{o} → {nv}") for n, o, nv in moved]
        parts.append(collapsible(f"{len(moved)} crates", table(["Crate", "Version"], rows), len(moved)))
    else:
        parts.append("_None._")
    if added or removed:
        rows = [(f"`{n}`", f"added {v}") for n, v in added] + [(f"`{n}`", f"removed {v}") for n, v in removed]
        parts += ["", f"### Added or removed transitive crates ({len(rows)})", ""]
        parts.append(collapsible(f"{len(rows)} crates", table(["Crate", "Change"], rows), len(rows)))

    parts += ["", "### Held for cooling-off", ""]
    if held:
        parts.append("Published less than a week ago, so they wait for the next train:")
        parts.append("")
        parts.append(table(["Crate", "Kept", "Newest", "Age"], [(f"`{n}`", o, nv, f"{a}d") for n, o, nv, a in held]))
    else:
        parts.append("_None._")

    parts += ["", f"### Pinned back (`{config_path}`)", ""]
    if pins:
        parts.append(table(["Crate", "Pinned to"], [(f"`{n}`", v) for n, v in pins]))
    else:
        parts.append("_None._")

    parts += ["", f"### Held behind a breaking line ({len(majors)})", ""]
    if majors:
        parts.append("Direct dependencies whose newest stable release needs a manual, breaking upgrade:")
        parts.append("")
        rows = [(f"`{n}`", locked, newest) for n, locked, newest in majors]
        parts.append(collapsible(f"{len(majors)} crates", table(["Crate", "Locked", "Newest"], rows), len(majors)))
    else:
        parts.append("_None._")

    parts += [
        "",
        "---",
        "",
        "**Merge rule:** Train A is merged monthly by the conductor; Train B by the ZODL-lane owner. "
        "After 45 days any maintainer may merge.",
        "",
        "This PR is regenerated on the first of each month, or on demand from the Actions tab; "
        "approve it and the cron leaves it alone.",
        "",
    ]
    return "\n".join(parts)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("train", choices=sorted(TRAINS))
    parser.add_argument("--dry-run", action="store_true", help="print the body and fragment instead of writing them")
    parser.add_argument("--cooling-days", type=int, default=7, help="hold versions younger than this (default: 7)")
    parser.add_argument("--config", default=".github/dependency-trains.toml")
    parser.add_argument("--body-out", default="train-body.md", help="where to write the PR body")
    args = parser.parse_args()

    with open(ROOT / args.config, "rb") as file:
        config = tomllib.load(file)
    consensus = config.get("consensus", [])
    pinned = config.get("pinned", {})
    api = CratesIo()

    original_bytes = (ROOT / "Cargo.lock").read_bytes()
    original = read_lock()

    log("Running the full cargo update")
    if not cargo_ok("update"):
        sys.exit(1)
    held = []
    changes = apply_cooling_off(original, api, args.cooling_days, held)
    log(f"Full update moves {len(changes)} crates after cooling-off, {len(held)} held")

    if args.train == "a":
        build_train_a(original, changes, consensus)
    else:
        build_train_b(original_bytes, original, changes, consensus)
        # Cargo may pick fresh transitive versions again, so the cooling-off pass runs once more.
        apply_cooling_off(original, api, args.cooling_days, held)

    pins = apply_pins(pinned, read_lock())
    final_lock = read_lock()
    changes = diff_locks(original, final_lock)
    moved_count = sum(1 for _, old, new in changes if old and new)
    log(f"{TRAINS[args.train][0]} moves {moved_count} crates")

    log("Checking direct dependencies against crates.io for the majors dashboard")
    majors = held_back_majors(api, final_lock)

    body = body_text(args.train, changes, held, pins, majors, args.config)
    fragment = fragment_text(args.train, moved_count)
    fragment_path = ROOT / ".changes" / "unreleased" / f"zebrad-Changed-dependency-train-{args.train}.yaml"

    if args.dry_run:
        print(body)
        print(f"--- {fragment_path.relative_to(ROOT)}")
        print(fragment, end="")
        return
    if not changes:
        log("Cargo.lock is unchanged, nothing to ship")
        return
    fragment_path.write_text(fragment)
    (ROOT / args.body_out).write_text(body)
    log(f"Wrote {fragment_path.relative_to(ROOT)} and {args.body_out}")


if __name__ == "__main__":
    main()
