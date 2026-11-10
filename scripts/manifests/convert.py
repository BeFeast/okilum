#!/usr/bin/env python3
"""Prepare Area TOML manifests. Dry-run by default; YAML removal needs a backup.

Use a quiescent copy of the Area manifests. Real-vault conversion is a separate
explicit rollout after every channel/OS and external reader supports TOML.
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
# The whole service set from okilum_t3::service_layout, not just the two files
# converted here: an orphan sidecar in the other directory is also a conflict.
SERVICE_FILES = {
    "project.toml", "project.yaml", "board.toml", "board.yaml",
    "board-local.toml", "board-local.yaml", "board-context.toml",
    "board-delivery.toml", "thread-ids.md",
}


def service_directory(context):
    """Resolve one existing layout without relocating or combining metadata."""
    inventories = {}
    for name in (".okilum", "ops"):
        directory = context / name
        try:
            metadata = directory.lstat()
        except FileNotFoundError:
            inventories[name] = set()
            continue
        if not stat.S_ISDIR(metadata.st_mode):
            raise ValueError(f"{directory}: service directory must not be a symlink or file")
        found = set()
        with os.scandir(directory) as entries:
            for entry in entries:
                if entry.name == "service-migration.json" and name == ".okilum":
                    raise ValueError(f"{directory}: service migration needs explicit recovery first")
                if ".sync-conflict-" in entry.name and entry.name.startswith(("project", "board", "thread-ids")):
                    raise ValueError(f"{directory / entry.name}: conflicting service file needs reconciliation")
                if entry.name in SERVICE_FILES:
                    if not entry.is_file(follow_symlinks=False):
                        raise ValueError(f"{directory / entry.name}: service file must be regular, not a symlink")
                    found.add(entry.name)
        inventories[name] = found
    if inventories[".okilum"] and inventories["ops"]:
        raise ValueError(f"{context}: conflicting service layouts; reconcile .okilum and ops first")
    selected = "ops" if inventories["ops"] else ".okilum"
    files = inventories[selected]
    if files and not files.intersection({"project.toml", "project.yaml"}):
        raise ValueError(f"{context / selected}: orphan service files; restore the project manifest first")
    return context / selected, files


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


def atomic_create(path, data, permissions, timestamps=None):
    """Same-directory atomic publication; an existing TOML is never replaced."""
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as file:
            file.write(data)
            if hasattr(os, "fchmod"):
                os.fchmod(file.fileno(), permissions)
            file.flush()
            if timestamps is not None:
                os.utime(temporary, ns=timestamps)
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


def remove_yaml(source, original, target, backup):
    """Retire a captured directory entry only after a durable byte-exact backup.

    Quarantine by rename before checking bytes: a replacement writer cannot make
    us unlink a different entry at the original path. On refusal restore without
    replacing a concurrent creator; if occupied, retain the recovery file.
    """
    metadata = source.stat(follow_symlinks=False)
    if source.is_symlink() or source.read_bytes() != original:
        raise ValueError("YAML changed before removal")
    if target.is_symlink() or not target.is_file():
        raise ValueError("TOML must remain a regular file before removal")
    target_bytes = target.read_bytes()
    if tomlkit.parse(target_bytes.decode()).unwrap() != tomlkit.parse(translate(original.decode())[0]).unwrap():
        raise ValueError("TOML differs before YAML removal")
    # A shared filesystem alias (macOS /var -> /private/var) is already part
    # of the selected source tree. Refuse aliases introduced only by the backup.
    source_parents = set(source.parents)
    if any(path.is_symlink() and path not in source_parents for path in [backup, *backup.parents]):
        raise ValueError("Backup path must not contain symlinks")
    backup.parent.mkdir(parents=True, exist_ok=True)
    if backup.exists():
        if not backup.is_file() or backup.read_bytes() != original:
            raise ValueError("Existing backup differs; YAML was not removed")
    else:
        atomic_create(backup, original, stat.S_IMODE(metadata.st_mode),
                      (metadata.st_atime_ns, metadata.st_mtime_ns))
    fd, temporary = tempfile.mkstemp(prefix=f".{source.name}.recovery-", dir=source.parent)
    os.close(fd)
    recovery = Path(temporary)
    moved = False
    try:
        os.replace(source, recovery)
        moved = True
        captured = recovery.stat(follow_symlinks=False)
        identity = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns)
        if (not stat.S_ISREG(captured.st_mode) or identity(captured) != identity(metadata)
                or recovery.read_bytes() != original):
            raise ValueError("YAML changed during removal")
        if target.is_symlink() or target.read_bytes() != target_bytes:
            raise ValueError("TOML changed during YAML removal")
        if backup.is_symlink() or backup.read_bytes() != original:
            raise ValueError("Backup changed during YAML removal")
        if os.path.lexists(source):
            raise ValueError("YAML was recreated during removal")
    except Exception:
        if moved:
            try:
                os.link(recovery, source, follow_symlinks=False)
            except OSError as error:
                raise ValueError(f"YAML retained for recovery at {recovery}; cannot restore {source}: {error}") from error
        recovery.unlink()
        raise
    recovery.unlink()


def convert_file(source, apply=False, backup=None):
    if backup is not None and not apply:
        raise ValueError("YAML removal requires --write")
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
        status = "unchanged"
    elif apply:
        if source.read_bytes() != original:
            raise ValueError("YAML changed during conversion; no TOML was written")
        atomic_create(target, output.encode("utf-8"), stat.S_IMODE(source.stat().st_mode))
        status = "created"
    else:
        status = "would_create"
    result = {"status": status, "changes": changes, "comments": comments}
    if backup is not None:
        remove_yaml(source, original, target, backup)
        result.update(yaml="removed", backup=str(backup))
    return result


def manifests(root):
    def fail(error):
        raise error  # An unreadable subtree must not look like an empty Area.

    for directory, folders, files in os.walk(root, followlinks=False, onerror=fail):
        selected, service_files = service_directory(Path(directory))
        # Inspect service directories above, but never walk their other files
        # as though they were independent owning contexts.
        folders[:] = [name for name in folders if name not in SKIP and name != "ops"
                      and not (Path(directory) / name).is_symlink()]
        for name in ("project.yaml", "board.yaml"):
            if name in service_files:
                yield selected / name


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="Vault or copied Areas tree")
    parser.add_argument("--write", action="store_true", help="Create missing TOML files; only after rollout. YAML remains unless --remove-yaml is explicit.")
    parser.add_argument("--remove-yaml", action="store_true", help="Remove equivalent YAML only with --write and --backup-dir.")
    parser.add_argument("--backup-dir", type=Path, help="Separate backup directory outside the selected tree; original relative paths are retained.")
    args = parser.parse_args()
    if not args.root.is_dir() or args.root.is_symlink():
        parser.error("root must be an existing directory, not a symlink")
    if args.remove_yaml and (not args.write or args.backup_dir is None):
        parser.error("--remove-yaml requires --write and --backup-dir")
    if args.backup_dir is not None:
        if not args.remove_yaml:
            parser.error("--backup-dir requires --remove-yaml")
        if args.backup_dir.resolve().is_relative_to(args.root.resolve()):
            parser.error("backup directory must be outside the selected tree")
    rows, failed = [], False
    try:
        # Complete discovery before any write: a later conflicting Area must
        # not leave the earlier Areas half converted.
        sources = sorted(manifests(args.root))
    except (OSError, ValueError) as error:
        print(json.dumps({"mode": "write" if args.write else "dry-run",
                          "files": [], "error": str(error)}, ensure_ascii=False, indent=2))
        return 1
    for source in sources:
        row = {"source": str(source.relative_to(args.root)), "target": str(source.with_suffix('.toml').relative_to(args.root))}
        try:
            selected, _ = service_directory(source.parent.parent)
            if selected != source.parent:
                raise ValueError("Service location changed; rerun discovery on a quiescent copy")
            backup = args.backup_dir / source.relative_to(args.root) if args.remove_yaml else None
            row.update(convert_file(source, args.write, backup))
        except Exception as error:
            row.update(status="error", error=str(error))
            failed = True
        rows.append(row)
    print(json.dumps({"mode": "write" if args.write else "dry-run", "files": rows}, ensure_ascii=False, indent=2))
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
