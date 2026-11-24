#!/usr/bin/env python3
"""Validate or rewrite the tuishou release version for stable and dev tags.

Stable tags must match the version in Cargo.toml. Dev tags only rewrite the
checked-out Cargo.toml and Cargo.lock, so the source commit and its history stay
unchanged.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PACKAGE = "tuishou"
VERSION_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")
STABLE_TAG_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")
DEV_TAG_RE = re.compile(
    r"^v(\d+)\.(\d+)\.(\d+)-dev\.(\d+)\.g([0-9a-fA-F]+)$"
)
SHORT_HASH_RE = re.compile(r"^[0-9a-f]{7,40}$")


class VersionError(Exception):
    """Invalid tag or inconsistent checkout."""


def read_version(root: Path) -> str:
    path = root / "Cargo.toml"
    text = path.read_text(encoding="utf-8")
    if "[package]" not in text:
        raise VersionError(f"no [package] section in {path}")
    package = text.split("[package]", 1)[1].split("\n[", 1)[0]
    match = re.search(r'^version\s*=\s*"([^"]+)"', package, re.MULTILINE)
    if not match:
        raise VersionError(f"no package version in {path}")
    return match.group(1)


def read_lock_version(root: Path) -> str:
    path = root / "Cargo.lock"
    blocks = path.read_text(encoding="utf-8").split("[[package]]\n")
    for block in blocks[1:]:
        if block.startswith(f'name = "{PACKAGE}"\n'):
            match = re.search(r'^version = "([^"]+)"$', block, re.MULTILINE)
            if match:
                return match.group(1)
    raise VersionError(f"no {PACKAGE} entry in {path}")


def write_version(root: Path, version: str) -> None:
    toml_path = root / "Cargo.toml"
    text = toml_path.read_text(encoding="utf-8")
    head, rest = text.split("[package]", 1)
    package, tail = rest.split("\n[", 1)
    package, count = re.subn(
        r'(?m)^version = "[^"]+"$', f'version = "{version}"', package, count=1
    )
    if count != 1:
        raise VersionError(f"no package version to rewrite in {toml_path}")
    toml_path.write_text(head + "[package]" + package + "\n[" + tail, encoding="utf-8")

    lock_path = root / "Cargo.lock"
    blocks = lock_path.read_text(encoding="utf-8").split("[[package]]\n")
    for index, block in enumerate(blocks[1:], 1):
        if block.startswith(f'name = "{PACKAGE}"\n'):
            block, count = re.subn(
                r'(?m)^version = "[^"]+"$',
                f'version = "{version}"',
                block,
                count=1,
            )
            if count != 1:
                raise VersionError(f"no {PACKAGE} version to rewrite in {lock_path}")
            blocks[index] = block
            lock_path.write_text("[[package]]\n".join(blocks), encoding="utf-8")
            return
    raise VersionError(f"no {PACKAGE} entry in {lock_path}")


def parse_tag(tag: str) -> tuple[int, int, int, int, str]:
    """Return (major, minor, patch, commit count, hash); empty hash means stable."""
    stable = STABLE_TAG_RE.match(tag)
    if stable:
        return int(stable.group(1)), int(stable.group(2)), int(stable.group(3)), 0, ""
    dev = DEV_TAG_RE.match(tag)
    if dev:
        return (
            int(dev.group(1)),
            int(dev.group(2)),
            int(dev.group(3)),
            int(dev.group(4)),
            dev.group(5).lower(),
        )
    raise VersionError(f"invalid release tag: {tag}")


def is_dev(tag: str) -> bool:
    return DEV_TAG_RE.match(tag) is not None


def sort_key(tag: str) -> tuple[int, int, int, int, int, str]:
    major, minor, patch, commits, commit_hash = parse_tag(tag)
    # A dev build of the next patch sorts above the base stable tag and below
    # the stable tag with the same core version.
    stage = 0 if is_dev(tag) else 1
    return major, minor, patch, stage, commits, commit_hash


def check_stable(root: Path, tag: str) -> None:
    if is_dev(tag):
        raise VersionError(f"stable release cannot use a dev tag: {tag}")
    parse_tag(tag)
    version = read_version(root)
    if version != tag[1:]:
        raise VersionError(f"tag {tag} does not match Cargo.toml version {version}")
    lock_version = read_lock_version(root)
    if lock_version != version:
        raise VersionError(
            f"Cargo.lock version {lock_version} does not match Cargo.toml version {version}"
        )


def set_version(root: Path, tag: str) -> None:
    version = read_version(root)
    if read_lock_version(root) != version:
        raise VersionError("Cargo.lock version does not match Cargo.toml version")
    if not is_dev(tag):
        check_stable(root, tag)
        return

    core = VERSION_RE.match(version)
    if not core:
        raise VersionError(f"Cargo.toml version is not a stable release: {version}")
    major, minor, patch, commits, commit_hash = parse_tag(tag)
    expected = (int(core.group(1)), int(core.group(2)), int(core.group(3)) + 1)
    if (major, minor, patch) != expected:
        raise VersionError(
            f"dev tag {tag} does not follow base version v{version} (expected v{expected[0]}.{expected[1]}.{expected[2]})"
        )
    if commits == 0:
        raise VersionError(f"dev tag needs a commit count: {tag}")
    if not SHORT_HASH_RE.match(commit_hash):
        raise VersionError(f"dev tag has an invalid short hash: {tag}")
    write_version(root, tag[1:])


def dev_tag(root: Path, commits: int, short_hash: str) -> str:
    version = read_version(root)
    core = VERSION_RE.match(version)
    if not core:
        raise VersionError(f"Cargo.toml version is not a stable release: {version}")
    if commits <= 0:
        raise VersionError("commit count must be positive")
    short_hash = short_hash.lower()
    if not SHORT_HASH_RE.match(short_hash):
        raise VersionError(f"invalid short hash: {short_hash}")
    return f"v{core.group(1)}.{core.group(2)}.{int(core.group(3)) + 1}-dev.{commits}.g{short_hash}"


def prune_list(tags: list[str], keep: int) -> list[str]:
    dev_tags = [tag for tag in tags if is_dev(tag)]
    dev_tags.sort(key=sort_key)
    return dev_tags[:-keep] if keep > 0 else dev_tags


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="checkout root")
    subparsers = parser.add_subparsers(dest="command", required=True)

    check = subparsers.add_parser("check", help="refuse a tag that is not the stable version")
    check.add_argument("tag")

    set_parser = subparsers.add_parser("set", help="check a stable tag or rewrite a dev checkout")
    set_parser.add_argument("tag")

    dev = subparsers.add_parser("dev-tag", help="build the next dev tag from Cargo.toml")
    dev.add_argument("--commits", type=int, required=True)
    dev.add_argument("--hash", required=True, dest="short_hash")

    key = subparsers.add_parser("sort-key", help="print a comparable release key")
    key.add_argument("tag")

    prune = subparsers.add_parser("prune-list", help="print dev tags outside the newest KEEP")
    prune.add_argument("--keep", type=int, default=10)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        if args.command == "check":
            check_stable(args.root, args.tag)
        elif args.command == "set":
            set_version(args.root, args.tag)
        elif args.command == "dev-tag":
            print(dev_tag(args.root, args.commits, args.short_hash))
        elif args.command == "sort-key":
            print(json.dumps(sort_key(args.tag)))
        elif args.command == "prune-list":
            tags = [line.strip() for line in sys.stdin if line.strip()]
            for tag in prune_list(tags, args.keep):
                print(tag)
    except (VersionError, OSError) as error:
        print(f"set-release-version: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
