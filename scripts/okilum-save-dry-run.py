#!/usr/bin/env python3
"""Print a metadata-only inventory of Okilum recovery names. Never delete files."""
import argparse
import datetime
import json
import os
from pathlib import Path
import re
import stat
import sys
import uuid

PREFIX = ".okilum-save-"


def group(name):
    if not name.startswith(PREFIX):
        return None
    suffix = name[len(PREFIX):]
    if suffix.endswith(".previous"):
        identifier = suffix[:-len(".previous")]
        try:
            if str(uuid.UUID(identifier)) == identifier:
                return "windows_uuid_previous"
        except ValueError:
            pass
    if re.fullmatch(r"[A-Za-z0-9]{6}", suffix):
        return "legacy_six_character_tempfile"
    return "other_okilum_save"


def inventory(root):
    result = {
        "mode": "dry-run; metadata only; no file contents read; no deletions",
        "root": str(root),
        "groups": {name: [] for name in (
            "windows_uuid_previous", "legacy_six_character_tempfile", "other_okilum_save"
        )},
        "errors": [],
        "skipped_reparse_directories": [],
    }
    pending = [root]
    while pending:
        directory = pending.pop()
        try:
            with os.scandir(directory) as scan:
                entries = sorted(scan, key=lambda entry: entry.name)
        except OSError as error:
            result["errors"].append({"path": str(directory), "error": str(error)})
            continue
        for entry in entries:
            path = Path(entry.path)
            relative = path.relative_to(root).as_posix()
            try:
                metadata = entry.stat(follow_symlinks=False)
            except OSError as error:
                result["errors"].append({"path": relative, "error": str(error)})
                continue
            attributes = getattr(metadata, "st_file_attributes", 0)
            reparse = bool(attributes & 0x400) or stat.S_ISLNK(metadata.st_mode)
            category = group(entry.name)
            if category:
                result["groups"][category].append({
                    "path": relative,
                    "bytes": metadata.st_size,
                    "modified_utc": datetime.datetime.fromtimestamp(
                        metadata.st_mtime, datetime.timezone.utc
                    ).isoformat(),
                    "kind": "reparse_or_symlink" if reparse else (
                        "file" if stat.S_ISREG(metadata.st_mode) else "directory_or_other"
                    ),
                    "links": metadata.st_nlink,
                    "attributes": attributes,
                })
            if stat.S_ISDIR(metadata.st_mode):
                if reparse:
                    result["skipped_reparse_directories"].append(relative)
                elif category is None:
                    pending.append(path)
    for entries in result["groups"].values():
        entries.sort(key=lambda item: item["path"])
    result["counts"] = {name: len(entries) for name, entries in result["groups"].items()}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("vault_root", type=Path)
    args = parser.parse_args()
    root = Path(os.path.abspath(args.vault_root))
    try:
        metadata = root.lstat()
    except OSError as error:
        parser.error(str(error))
    if not stat.S_ISDIR(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400:
        parser.error("Use a real vault directory, not a symlink or reparse root")
    report = inventory(root)
    sys.stdout.reconfigure(encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 1 if report["errors"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
