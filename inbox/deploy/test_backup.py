"""Backup must include committed WAL data; failure must preserve the prior snapshot."""
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    source = root / 'live.db'
    backup = root / 'backup'
    connection = sqlite3.connect(source)
    connection.execute('PRAGMA journal_mode=WAL')
    connection.execute('PRAGMA wal_autocheckpoint=0')
    for name in ('captures', 'capture_operations', 'auth_owner', 'discussion_turns', 'publications'):
        connection.execute(f'CREATE TABLE {name}(id TEXT)')
    connection.execute("INSERT INTO captures VALUES ('committed-original')")
    connection.commit()
    assert Path(str(source) + '-wal').stat().st_size > 0
    command = [sys.executable, str(Path(__file__).with_name('backup.py')), '--source', str(source), '--destination', str(backup)]
    subprocess.run(command, check=True)
    restored = sqlite3.connect(backup / 'inbox-latest.db')
    assert restored.execute('SELECT id FROM captures').fetchall() == [('committed-original',)]
    assert restored.execute('PRAGMA integrity_check').fetchone() == ('ok',)
    restored.close()
    previous = (backup / 'inbox-latest.db').read_bytes()
    # Unsupported/corrupt source never replaces the last good completed backup.
    broken = root / 'broken.db'
    broken.write_bytes(b'not sqlite')
    rejected = subprocess.run([*command[:3], str(broken), *command[4:]], capture_output=True)
    assert rejected.returncode != 0
    assert (backup / 'inbox-latest.db').read_bytes() == previous
    connection.close()
print('WAL backup/restore and failure preservation passed')
