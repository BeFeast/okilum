#!/usr/bin/env python3
"""Minimal PE header reader for the Windows packaging checks (#1013).

The sync supervisor is started at login by Task Scheduler and must be a GUI-subsystem
program, or every login would flash a console window. `cargo` does not say which
subsystem it linked, so the build asserts it on the produced file:
    python3 scripts/windows/pe.py gui <file.exe>
"""
import struct
import sys

MACHINE_AMD64 = 0x8664
SUBSYSTEM_GUI = 2
SUBSYSTEM_CONSOLE = 3
PE32_PLUS = 0x20B


def read_header(data: bytes) -> tuple[int, int]:
    """(machine, subsystem) of a PE32+ image; ValueError if it is anything else."""
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise ValueError("not a PE file")
    (lfanew,) = struct.unpack_from("<I", data, 0x3C)
    optional = lfanew + 24
    if lfanew < 0x40 or optional + 70 > len(data) or data[lfanew : lfanew + 4] != b"PE\0\0":
        raise ValueError("not a PE file")
    (machine,) = struct.unpack_from("<H", data, lfanew + 4)
    (magic,) = struct.unpack_from("<H", data, optional)
    if magic != PE32_PLUS:
        raise ValueError("not a 64-bit PE image")
    (subsystem,) = struct.unpack_from("<H", data, optional + 68)
    return machine, subsystem


def require_gui_x64(path: str) -> None:
    with open(path, "rb") as handle:
        machine, subsystem = read_header(handle.read(4096))
    if machine != MACHINE_AMD64:
        raise ValueError(f"{path}: machine 0x{machine:x} is not x86-64")
    if subsystem != SUBSYSTEM_GUI:
        raise ValueError(f"{path}: subsystem {subsystem} is not Windows GUI ({SUBSYSTEM_GUI})")


def main(argv: list[str]) -> int:
    if len(argv) != 3 or argv[1] != "gui":
        print("usage: pe.py gui <file.exe>", file=sys.stderr)
        return 2
    try:
        require_gui_x64(argv[2])
    except (OSError, ValueError) as error:
        print(f"pe.py: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
