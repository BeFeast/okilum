#!/usr/bin/env python3
"""Online SQLite backup for the pre-PBS hook; never copy a live DB without WAL."""
import argparse
import os
import sqlite3
import tempfile
from pathlib import Path
os.umask(0o077)
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path('/data/inbox.db'))
parser.add_argument('--destination', type=Path, default=Path('/backups'))
args = parser.parse_args()
source, target = args.source, args.destination
if not source.is_file():
    raise SystemExit('Inbox database is absent; backup refused')
target.mkdir(mode=0o700, exist_ok=True)
fd, temporary = tempfile.mkstemp(prefix='.inbox-', suffix='.db', dir=target)
os.close(fd)
try:
    with sqlite3.connect(source.resolve().as_uri() + '?mode=ro', uri=True) as live:
        with sqlite3.connect(temporary) as copy:
            live.backup(copy)
            if copy.execute('PRAGMA integrity_check').fetchall() != [('ok',)]:
                raise RuntimeError('Inbox backup integrity check failed')
    with sqlite3.connect(Path(temporary).resolve().as_uri() + '?mode=ro', uri=True) as copy:
        for table in ('captures', 'capture_operations', 'auth_owner', 'discussion_turns', 'publications'):
            copy.execute(f'SELECT count(*) FROM {table}').fetchone()
    with open(temporary, 'rb') as backup:
        os.fsync(backup.fileno())
    os.replace(temporary, target / 'inbox-latest.db')
    directory = os.open(target, os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
finally:
    Path(temporary).unlink(missing_ok=True)
print('Inbox consistent backup complete')
