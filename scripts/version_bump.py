#!/usr/bin/env python3
"""Per-crate semver bumping driven by source content hashes.

Every workspace member carries its own `version` in its `Cargo.toml` (no
`version.workspace = true`). This script hashes each crate's compilable
content, compares it against the recorded hash in `.build-versions.json`, and
bumps the patch version of every crate whose content changed since the last
recorded build. `scripts/build.sh` runs it immediately before cargo, so a
recompilation that follows a code change always produces a new version number
for the crate that changed — and only for that crate.

Cargo reads `Cargo.toml` before any `build.rs` runs, which is why the bump
happens here rather than inside the build itself: a bump performed during the
build would only take effect on the *next* one.

The `version = "..."` line is excluded from a crate's hash, so bumping a crate
does not itself make the crate look changed on the following run.

Usage:
    scripts/version_bump.py            # bump changed crates, update manifest
    scripts/version_bump.py --check    # report what would bump, change nothing
    scripts/version_bump.py --baseline # (re)record hashes without bumping
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
MANIFEST = REPO / ".build-versions.json"

# Directories and files whose content can change what cargo compiles.
HASHED_DIRS = ("src", "tests", "benches", "examples")
HASHED_FILES = ("build.rs",)

PACKAGE_VERSION_RE = re.compile(
    r'^(?P<prefix>\s*version\s*=\s*)(?P<value>"[^"]*"|\{[^}]*\})\s*$', re.MULTILINE
)
SEMVER_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")


def workspace_members() -> list[Path]:
    """Reads the `members` list out of the workspace `Cargo.toml`."""
    text = (REPO / "Cargo.toml").read_text()
    block = re.search(r"members\s*=\s*\[(.*?)\]", text, re.DOTALL)
    if not block:
        sys.exit("version_bump: no [workspace] members list in Cargo.toml")
    members = re.findall(r'"([^"]+)"', block.group(1))
    return [REPO / m for m in members]


def crate_name(manifest_text: str) -> str:
    m = re.search(r'^\s*name\s*=\s*"([^"]+)"', manifest_text, re.MULTILINE)
    if not m:
        sys.exit("version_bump: a crate manifest has no [package] name")
    return m.group(1)


def content_hash(crate_dir: Path) -> str:
    """Hashes everything cargo compiles for this crate, minus the version line.

    Excluding the version line keeps a bump from registering as a change on the
    next run, which would otherwise bump every crate on every invocation.
    """
    h = hashlib.sha256()

    manifest = (crate_dir / "Cargo.toml").read_text()
    h.update(PACKAGE_VERSION_RE.sub("", manifest, count=1).encode())

    paths: list[Path] = []
    for d in HASHED_DIRS:
        root = crate_dir / d
        if root.is_dir():
            paths.extend(p for p in root.rglob("*") if p.is_file())
    for f in HASHED_FILES:
        p = crate_dir / f
        if p.is_file():
            paths.append(p)

    for p in sorted(paths, key=lambda x: x.relative_to(crate_dir).as_posix()):
        h.update(p.relative_to(crate_dir).as_posix().encode())
        h.update(b"\0")
        h.update(p.read_bytes())
        h.update(b"\0")
    return h.hexdigest()


def current_version(crate_dir: Path, manifest_text: str) -> str:
    m = PACKAGE_VERSION_RE.search(manifest_text)
    if not m:
        sys.exit(f"version_bump: {crate_dir}/Cargo.toml has no version key")
    value = m.group("value")
    if not value.startswith('"'):
        sys.exit(
            f"version_bump: {crate_dir}/Cargo.toml still uses {value}. "
            "Per-crate versioning needs an explicit version = \"x.y.z\"."
        )
    return value.strip('"')


def bump_patch(version: str, crate_dir: Path) -> str:
    m = SEMVER_RE.match(version)
    if not m:
        sys.exit(f"version_bump: {crate_dir} has non-semver version {version!r}")
    major, minor, patch = (int(g) for g in m.groups())
    return f"{major}.{minor}.{patch + 1}"


def write_version(crate_dir: Path, manifest_text: str, new_version: str) -> None:
    updated = PACKAGE_VERSION_RE.sub(
        lambda m: f'{m.group("prefix")}"{new_version}"', manifest_text, count=1
    )
    (crate_dir / "Cargo.toml").write_text(updated)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true", help="report only, change nothing")
    ap.add_argument(
        "--baseline",
        action="store_true",
        help="record current hashes without bumping any version",
    )
    args = ap.parse_args()

    recorded = {}
    if MANIFEST.exists():
        recorded = json.loads(MANIFEST.read_text()).get("crates", {})
    first_run = not recorded

    state: dict[str, dict[str, str]] = {}
    bumped: list[str] = []

    for crate_dir in workspace_members():
        manifest_text = (crate_dir / "Cargo.toml").read_text()
        name = crate_name(manifest_text)
        version = current_version(crate_dir, manifest_text)
        digest = content_hash(crate_dir)

        prior = recorded.get(name)
        changed = prior is not None and prior.get("hash") != digest

        if changed and not (args.check or args.baseline or first_run):
            version = bump_patch(version, crate_dir)
            write_version(crate_dir, manifest_text, version)
            bumped.append(f"{name} {prior['version']} -> {version}")
        elif changed:
            bumped.append(f"{name} {prior['version']} -> (would bump)")

        state[name] = {
            "version": version,
            "hash": digest,
            "path": crate_dir.relative_to(REPO).as_posix(),
        }

    if args.check:
        for line in bumped:
            print(f"  would bump: {line}")
        print(f"version_bump: {len(bumped)} crate(s) changed since the last build")
        return 1 if bumped else 0

    MANIFEST.write_text(
        json.dumps(
            {
                "_comment": "Written by scripts/version_bump.py. Hash covers each "
                "crate's src/tests/benches/examples/build.rs plus its Cargo.toml "
                "with the version line removed.",
                "updated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "crates": dict(sorted(state.items())),
            },
            indent=2,
        )
        + "\n"
    )

    if first_run or args.baseline:
        print(f"version_bump: baseline recorded for {len(state)} crate(s); no bumps")
    elif bumped:
        for line in bumped:
            print(f"  bumped: {line}")
        print(f"version_bump: {len(bumped)} crate(s) bumped")
    else:
        print("version_bump: no crate content changed; versions unchanged")
    return 0


if __name__ == "__main__":
    sys.exit(main())
