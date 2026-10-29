#!/usr/bin/env python3
"""Prepare Area TOML manifests. Dry-run by default; legacy YAML is never changed.

The manager applies this to the real vault only after every channel/OS supports
TOML. Conversion tests use a separate copy of the Area manifests.
"""
from __future__ import annotations

import argparse
import datetime
import json
import math
import os
from pathlib import Path
import stat
import tempfile

from ruamel.yaml import YAML
import tomlkit

SKIP = {".git", ".obsidian", ".trash", ".stversions", "node_modules", ".okilum", ".okilum-index"}


def pointer(parts):
    return "/" + "/".join(str(p).replace("~", "~0").replace("/", "~1") for p in parts)


def normalize(value, path=(), changes=None, active=None):
    """TOML's native conventions: absent nullable keys, string keys, ISO dates.

    Each representational change is reported. Null array elements and cycles
    cannot be translated without changing sequence semantics, so they fail.
    """
    changes = changes if changes is not None else []
    active = active if active is not None else set()
    if isinstance(value, (dict, list)):
        if id(value) in active:
            raise ValueError(f"{pointer(path)}: cyclic YAML alias cannot be represented in TOML")
        active.add(id(value))
        try:
            if isinstance(value, dict):
                result, keys = {}, set()
                for key, item in value.items():
                    if not isinstance(key, (str, int, bool, float, datetime.date)):
                        raise ValueError(f"{pointer(path)}: unsupported mapping key")
                    text = key if isinstance(key, str) else str(key)
                    if text in keys:
                        raise ValueError(f"{pointer(path + (text,))}: mapping keys collide as TOML strings")
                    keys.add(text)
                    if not isinstance(key, str):
                        changes.append({"path": pointer(path + (text,)), "kind": "string_key"})
                    if item is None:
                        changes.append({"path": pointer(path + (text,)), "kind": "omitted_null"})
                        continue
                    result[text] = normalize(item, path + (text,), changes, active)
                return result
            if any(item is None for item in value):
                raise ValueError(f"{pointer(path)}: TOML arrays cannot contain null")
            return [normalize(item, path + (index,), changes, active) for index, item in enumerate(value)]
        finally:
            active.remove(id(value))
    if isinstance(value, (datetime.date, datetime.datetime)):
        changes.append({"path": pointer(path), "kind": "iso_date_string"})
        return value.isoformat()
    if isinstance(value, float) and not math.isfinite(value):
        raise ValueError(f"{pointer(path)}: nonfinite number needs a human decision")
    if isinstance(value, (str, int, float, bool)):
        return value
    raise ValueError(f"{pointer(path)}: unsupported YAML scalar")


def yaml_comments(value):
    """Use YAML comment tokens, never '#' text inside a scalar or block string."""
    result, visited, emitted = [], set(), set()

    def tokens(item):
        if isinstance(item, (list, tuple)):
            for child in item:
                yield from tokens(child)
        elif hasattr(item, "value") and isinstance(item.value, str):
            for line in item.value.splitlines():
                if line.lstrip().startswith("#"):
                    yield line.lstrip()[1:].lstrip(" ")

    def add(items, path):
        for comment in tokens(items):
            pair = (pointer(path), comment)
            if pair not in emitted:
                emitted.add(pair)
                result.append(pair)

    def visit(item, path=()):
        if id(item) in visited:
            return
        visited.add(id(item))
        comments = getattr(item, "ca", None)
        if comments is not None:
            add(comments.comment, path)
            add(comments.end, path)
            for key, comment in comments.items.items():
                add(comment, path + (key,))
        if isinstance(item, dict):
            for key, child in item.items():
                visit(child, path + (key,))
        elif isinstance(item, list):
            for index, child in enumerate(item):
                visit(child, path + (index,))

    visit(value)
    return result


def translate(source):
    yaml = YAML(typ="rt")
    yaml.allow_duplicate_keys = False
    commented = yaml.load(source)
    # The round-trip loader can replace recursive aliases with None. A safe
    # graph load retains their identity so normalization refuses such cycles.
    semantic = YAML(typ="safe", pure=True)
    semantic.allow_duplicate_keys = False
    original = semantic.load(source)
    if not isinstance(original, dict):
        raise ValueError("Area manifest must be a mapping")
    changes = []
    normalized = normalize(original, changes=changes)
    document = tomlkit.document()
    comments = yaml_comments(commented)
    if comments:
        document.add(tomlkit.comment("Original YAML comments, with their field context:"))
        for field, comment in comments:
            document.add(tomlkit.comment(f"{field}: {comment}"))
    for change in changes:
        document.add(tomlkit.comment(f"YAML {change['kind']}: {change['path']}"))
    if comments or changes:
        document.add(tomlkit.nl())
    for key, item in normalized.items():
        document.add(key, tomlkit.item(item))
    output = tomlkit.dumps(document)
    if tomlkit.parse(output).unwrap() != normalized:
        raise ValueError("TOML round-trip changed manifest values")
    return output, changes, len(comments)


def atomic_create(path, data, permissions):
    """Same-directory atomic publication; an existing TOML is never replaced."""
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as file:
            file.write(data)
            if hasattr(os, "fchmod"):
                os.fchmod(file.fileno(), permissions)
            file.flush()
            os.fsync(file.fileno())
        os.link(temporary, path)  # atomic no-replace, unlike exists()+rename()
        if os.name == "posix":
            directory = os.open(path.parent, os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
    finally:
        os.unlink(temporary)


def convert_file(source, apply=False):
    if source.is_symlink() or not stat.S_ISREG(source.stat().st_mode):
        raise ValueError("Source manifest must be a regular file, not a symlink")
    original = source.read_bytes()
    output, changes, comments = translate(original.decode("utf-8"))
    target = source.with_suffix(".toml")
    if target.is_symlink():
        raise ValueError("Target manifest is a symlink")
    if target.exists():
        desired = tomlkit.parse(output).unwrap()
        if tomlkit.parse(target.read_text(encoding="utf-8")).unwrap() != desired:
            raise ValueError("Existing TOML differs; it was not overwritten")
        return {"status": "unchanged", "changes": changes, "comments": comments}
    if apply:
        if source.read_bytes() != original:
            raise ValueError("YAML changed during conversion; no TOML was written")
        atomic_create(target, output.encode("utf-8"), stat.S_IMODE(source.stat().st_mode))
    return {"status": "created" if apply else "would_create", "changes": changes, "comments": comments}


def manifests(root):
    def fail(error):
        raise error  # An unreadable subtree must not look like an empty Area.

    for directory, folders, files in os.walk(root, followlinks=False, onerror=fail):
        folders[:] = [name for name in folders if name not in SKIP and not (Path(directory) / name).is_symlink()]
        if Path(directory).name == "ops":
            for name in ("project.yaml", "board.yaml"):
                if name in files:
                    yield Path(directory) / name


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="Vault or copied Areas tree")
    parser.add_argument("--write", action="store_true", help="Create missing TOML files; manager only after rollout. YAML remains untouched.")
    args = parser.parse_args()
    if not args.root.is_dir() or args.root.is_symlink():
        parser.error("root must be an existing directory, not a symlink")
    rows, failed = [], False
    for source in sorted(manifests(args.root)):
        row = {"source": str(source.relative_to(args.root)), "target": str(source.with_suffix('.toml').relative_to(args.root))}
        try:
            row.update(convert_file(source, args.write))
        except Exception as error:
            row.update(status="error", error=str(error))
            failed = True
        rows.append(row)
    print(json.dumps({"mode": "write" if args.write else "dry-run", "files": rows}, ensure_ascii=False, indent=2))
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
