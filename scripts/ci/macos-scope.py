#!/usr/bin/env python3
"""Conservative native-check selection; input is git diff --name-only -z."""
import sys


def needs_macos(paths):
    # Keep every shell change, filesystem fixture, dependency, vendor patch and
    # unknown input native-checked. Pure web/docs and other-OS packaging do not
    # compile into the Mac Reader or exercise APFS/FSEvents/clipboard behavior.
    def portable_only(path):
        if path.endswith('.md') and (path.startswith('docs/') or '/' not in path):
            return True
        if path.startswith(('web/', 'inbox/', 'scripts/arch/', 'scripts/windows/')):
            return True
        # QA fixtures, release publication, rebrand tooling and design mockups are
        # Python/HTML outside the Reader; the Linux gate runs their tests.
        if path.startswith(('scripts/qa/', 'scripts/releases/', 'scripts/rebrand/', 'design/')):
            return True
        if path.startswith('docs/') and path.endswith(('.svg', '.png', '.jpg', '.webp')):
            return True
        return path in {
            '.forgejo/workflows/linux-release.yml',
            '.forgejo/workflows/windows-diagnostic.yml',
            'scripts/build-windows-ci.sh', 'scripts/windows-rustc.py',
            'scripts/windows-rc.py', 'scripts/windows-icon.py',
        }
    return any(not portable_only(path) for path in paths)


if __name__ == '__main__':
    paths = sys.stdin.buffer.read().decode('utf-8', errors='surrogateescape').split('\0')
    print('true' if needs_macos([path for path in paths if path]) else 'false')
