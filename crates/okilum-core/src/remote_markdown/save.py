# Fixed transient program embedded by Okilum; invoked only by explicit Save/Retry.
import base64
import ctypes
import errno
import hashlib
import json
import os
import signal
import stat
import sys
import uuid

LIMIT = 2 * 1024 * 1024
UNSUPPORTED = "This server does not support safe saving; your changes are kept locally"


def revision(data):
    return {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}


def stamp(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def snapshot(directory, name):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_size > LIMIT:
            raise ValueError("The remote source is not a supported single-link regular file")
        data = bytearray()
        while len(data) <= LIMIT:
            chunk = os.read(fd, min(32768, LIMIT + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
        if len(data) > LIMIT or stamp(before) != stamp(os.fstat(fd)):
            raise ValueError("The remote source changed while reading; retry before saving")
        data = bytes(data)
        return data, before
    finally:
        os.close(fd)


def shown(data):
    try:
        return {"text": data.decode("utf-8"), "revision": revision(data)}
    except UnicodeError:
        return None


def main():
    raw = sys.stdin.buffer.read(4 * LIMIT + 16385)
    if len(raw) > 4 * LIMIT + 16384:
        raise ValueError("Remote save request exceeds its limit")
    if not sys.platform.startswith("linux"):
        return {"status": "unsupported", "message": UNSUPPORTED}
    import fcntl
    libc = ctypes.CDLL(None, use_errno=True)
    rename = getattr(libc, "renameat2", None)
    if rename is None:
        return {"status": "unsupported", "message": UNSUPPORTED}
    rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    rename.restype = ctypes.c_int

    request = json.loads(raw)
    if set(request) != {"version", "path_b64", "text_b64", "base", "attempt"} or request["version"] != 1:
        raise ValueError("Unsupported remote save request")
    path = base64.b64decode(request["path_b64"], validate=True)
    text = base64.b64decode(request["text_b64"], validate=True)
    text.decode("utf-8")
    base = request["base"]
    if (not path.startswith(b"/") or b"\0" in path or len(path) > 4096 or len(text) > LIMIT
            or set(base) != {"sha256", "bytes"} or len(base["sha256"]) != 64
            or any(c not in "0123456789abcdef" for c in base["sha256"])
            or not isinstance(base["bytes"], int) or not 0 <= base["bytes"] <= LIMIT
            or str(uuid.UUID(request["attempt"])) != request["attempt"]):
        raise ValueError("Invalid remote save identity or content")
    parent, name = os.path.split(path)
    if name in (b"", b".", b".."):
        raise ValueError("Choose a remote file")
    directory = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    history = None
    staged = None
    owned = None
    exchanged = False
    recovery = []
    transaction = None
    lock = None
    record = None

    def exchange(left, right):
        if rename(directory, left, directory, right, 2) != 0:
            code = ctypes.get_errno()
            raise OSError(code, os.strerror(code))
        os.fsync(directory)

    def unlink_owned(leaf, identity):
        try:
            info = os.stat(leaf, dir_fd=directory, follow_symlinks=False)
            if (info.st_dev, info.st_ino) == identity:
                os.unlink(leaf, dir_fd=directory)
        except FileNotFoundError:
            pass

    def archive_displaced(leaf, role):
        # Preserve the actual displaced inode too: a writer holding an old fd
        # may still change it after our snapshot. Never check-then-unlink it.
        destination = (request["attempt"] + "-" + role + "-" + uuid.uuid4().hex + ".md").encode()
        if rename(directory, leaf, history, destination, 1) != 0:
            code = ctypes.get_errno()
            if code == errno.ENOENT:
                return
            raise OSError(code, os.strerror(code))
        os.fsync(history)
        os.fsync(directory)
        recovery.append(base64.b64encode(os.path.join(parent, b".okilum-remote-history", destination)).decode())

    def retain(leaf, role):
        # A physical, fsynced recovery copy avoids adding hard links to the
        # canonical inode when a conflict exchange restores it.
        destination = (request["attempt"] + "-" + role + "-" + uuid.uuid4().hex + ".md").encode()
        source = os.open(leaf, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
        output = None
        try:
            before = os.fstat(source)
            if not stat.S_ISREG(before.st_mode) or before.st_size > LIMIT:
                raise ValueError("The displaced preimage cannot be safely archived")
            output = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=history)
            total = 0
            while True:
                chunk = os.read(source, min(32768, LIMIT + 1 - total))
                if not chunk:
                    break
                total += len(chunk)
                if total > LIMIT:
                    raise ValueError("The displaced preimage grew during recovery")
                remaining = memoryview(chunk)
                while remaining:
                    n = os.write(output, remaining)
                    if n <= 0:
                        raise OSError("Recovery write did not advance")
                    remaining = remaining[n:]
            if stamp(before) != stamp(os.fstat(source)):
                raise ValueError("The displaced preimage changed during recovery; staging was kept")
            os.fsync(output)
        finally:
            os.close(source)
            if output is not None:
                os.close(output)
        os.fsync(history)
        recovery.append(base64.b64encode(os.path.join(parent, b".okilum-remote-history", destination)).decode())
        return list(stamp(before))

    def checkpoint(phase, raced=None):
        record["phase"] = phase
        record["recovery"] = recovery
        record["raced"] = shown(raced) if raced is not None else None
        temporary = (".receipt-" + uuid.uuid4().hex).encode()
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=history)
        try:
            data = memoryview(json.dumps(record).encode())
            while data:
                n = os.write(fd, data)
                if n <= 0:
                    raise OSError("Receipt write did not advance")
                data = data[n:]
            os.fsync(fd)
        finally:
            os.close(fd)
        os.replace(temporary, transaction, src_dir_fd=history, dst_dir_fd=history)
        os.fsync(history)

    def conflict(data, message, raced=None, rollback_race=False):
        return {"status": "conflict", "remote": shown(data) if data is not None else None,
                "raced": shown(raced) if raced is not None else None, "recovery": recovery,
                "rollback_race": rollback_race, "message": message}

    try:
        wanted = revision(text)
        try:
            os.mkdir(b".okilum-remote-history", 0o700, dir_fd=directory)
        except FileExistsError:
            pass
        history = os.open(b".okilum-remote-history", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
        hi = os.fstat(history)
        if hi.st_uid != os.geteuid() or stat.S_IMODE(hi.st_mode) & 0o077:
            raise ValueError("Remote recovery directory is not private; changes are kept locally")
        lock = os.open(hashlib.sha256(name).hexdigest().encode() + b".lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=history)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError("Another safe-save attempt is still finishing. Retry explicitly after it completes")
        current, info = snapshot(directory, name)
        transaction = (request["attempt"] + ".json").encode()
        try:
            receipt_fd = os.open(transaction, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=history)
        except FileNotFoundError:
            receipt_fd = None
        if receipt_fd is not None:
            with os.fdopen(receipt_fd, "rb") as receipt_file:
                data = receipt_file.read(LIMIT * 6 + 65537)
            if len(data) > LIMIT * 6 + 65536:
                raise ValueError("The previous save receipt exceeds its bound")
            record = json.loads(data)
            if record["path_b64"] != request["path_b64"] or record["base"] != base or record["wanted"] != wanted:
                raise ValueError("The previous save attempt belongs to another draft")
            recovery = record.get("recovery", [])
            staged = base64.b64decode(record["stage"], validate=True)
            if not staged.startswith(b".okilum-remote-save-") or b"/" in staged or b"\x00" in staged:
                raise ValueError("Invalid previous save staging identity")
            owned = tuple(record["owned"])
            if record["phase"] in ("verified", "committed"):
                if revision(current) == wanted:
                    if record["phase"] == "verified":
                        # Preimage was already archived before this durable phase.
                        archive_displaced(staged, "preimage-inode")
                        staged = None
                        checkpoint("committed")
                    return {"status": "saved", "revision": wanted, "already_present": True, "recovery": recovery}
                return conflict(current, "The file changed after the previous save. Compare the new remote version before saving.")
            if record["phase"] == "conflict":
                return {"status": "conflict", "remote": shown(current), "raced": record.get("raced"),
                        "recovery": recovery, "rollback_race": record.get("raced") is not None,
                        "message": "The previous save found a conflict. Compare the remote and retained versions before saving."}
            try:
                stage_info = os.stat(staged, dir_fd=directory, follow_symlinks=False)
            except FileNotFoundError:
                stage_info = None
            if (info.st_dev, info.st_ino) == owned and stage_info is not None:
                # Disconnect after exchange but before verification: finish the
                # original guard, never acknowledge only by matching new bytes.
                exchanged = True
            elif stage_info is None or (stage_info.st_dev, stage_info.st_ino) == owned:
                if stage_info is not None:
                    unlink_owned(staged, owned)
                staged = None
                if revision(current) != base:
                    checkpoint("conflict")
                    return conflict(current, "The remote file changed before the previous attempt completed.")
                record = None
            else:
                # An unrelated writer superseded a partially published attempt.
                retain(staged, "interrupted-preimage")
                archive_displaced(staged, "interrupted-inode")
                staged = None
                checkpoint("conflict")
                return conflict(current, "The file changed during an interrupted save; its preimage was retained.")
        if not exchanged:
            if revision(current) == wanted:
                return {"status": "saved", "revision": wanted, "already_present": True, "recovery": recovery}
            if revision(current) != base:
                return conflict(current, "The remote file changed. Compare it before choosing which version to save.")

        if not exchanged:
            # Capability probe touches private files only, before any source swap.
            probes = []
            try:
                for _ in range(2):
                    leaf = (".okilum-save-probe-" + uuid.uuid4().hex).encode()
                    fd = os.open(leaf, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
                    item = os.fstat(fd)
                    os.close(fd)
                    probes.append((leaf, (item.st_dev, item.st_ino)))
                exchange(probes[0][0], probes[1][0])
                probes[0], probes[1] = (probes[0][0], probes[1][1]), (probes[1][0], probes[0][1])
            except OSError as error:
                if error.errno in (errno.ENOSYS, errno.EINVAL, errno.ENOTSUP, errno.EOPNOTSUPP):
                    return {"status": "unsupported", "message": UNSUPPORTED}
                raise
            finally:
                for leaf, identity in probes:
                    unlink_owned(leaf, identity)

            # test:before_staging
            staged = (".okilum-remote-save-" + uuid.uuid4().hex).encode()
            fd = os.open(staged, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
            try:
                created = os.fstat(fd)
                owned = (created.st_dev, created.st_ino)
                os.fchown(fd, info.st_uid, info.st_gid)
                os.fchmod(fd, stat.S_IMODE(info.st_mode))
                # Preserve ACL/security metadata or fail before touching the source.
                source_fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
                try:
                    for key in os.listxattr(source_fd):
                        os.setxattr(fd, key, os.getxattr(source_fd, key))
                finally:
                    os.close(source_fd)
                remaining = memoryview(text)
                while remaining:
                    written = os.write(fd, remaining)
                    if written <= 0:
                        raise OSError("Remote staging write did not advance")
                    remaining = remaining[written:]
                os.fsync(fd)
            finally:
                os.close(fd)
            os.fsync(directory)
            record = {"path_b64": request["path_b64"], "base": base, "wanted": wanted,
                      "stage": base64.b64encode(staged).decode(), "owned": list(owned),
                      "attributes": [info.st_uid, info.st_gid, stat.S_IMODE(info.st_mode)]}
            checkpoint("prepared")
            # test:before_exchange
            exchange(staged, name)
            exchanged = True
            # test:after_exchange
        try:
            displaced, displaced_info = snapshot(directory, staged)
            matches = (revision(displaced) == base and
                       [displaced_info.st_uid, displaced_info.st_gid, stat.S_IMODE(displaced_info.st_mode)] == record["attributes"])
        except (OSError, ValueError):
            displaced = None
            matches = False
        if not matches:
            # Preserve the first displaced inode before the reverse exchange.
            retain(staged, "conflict-preimage")
            # test:before_rollback
            exchange(staged, name)
            # The second exchange can itself displace an unrelated writer.
            after = os.stat(staged, dir_fd=directory, follow_symlinks=False)
            raced = None
            try:
                returned, _ = snapshot(directory, staged)
            except (OSError, ValueError):
                returned = None
            rollback_race = (after.st_dev, after.st_ino) != owned or returned is None or revision(returned) != wanted
            if rollback_race:
                raced = returned
                retain(staged, "rollback-race")
                archive_displaced(staged, "rollback-inode")
            else:
                archive_displaced(staged, "reversed-proposal")
            staged = None
            checkpoint("conflict", raced)
            return conflict(displaced,
                "The remote file changed during save. The exchange was reversed; your local draft is kept."
                if not rollback_race else
                "Another writer changed the file during conflict recovery. Both remote versions were preserved; compare them before saving again.",
                raced, rollback_race)

        # Confirm the installed source is still this attempt before acknowledgement.
        record["preimage_stamp"] = retain(staged, "preimage")
        checkpoint("verified")
        archive_displaced(staged, "preimage-inode")
        staged = None
        os.fsync(directory)
        # test:after_commit
        installed, installed_info = snapshot(directory, name)
        if (installed_info.st_dev, installed_info.st_ino) != owned or revision(installed) != wanted:
            return conflict(installed, "The remote file changed again after publication. Your draft and the preimage are kept.")
        checkpoint("committed")
        return {"status": "saved", "revision": wanted, "already_present": False, "recovery": recovery}
    finally:
        # A failed/uncertain exchange keeps the displaced name recoverable.
        if staged is not None and not exchanged and owned is not None:
            unlink_owned(staged, owned)
        if lock is not None:
            os.close(lock)
        if history is not None:
            os.close(history)
        os.close(directory)


def timed_out(*_):
    raise TimeoutError("Remote save deadline; changes are kept locally")


signal.signal(signal.SIGALRM, timed_out)
signal.alarm(25)
sys.stdout.write('{"protocol":"okilum-safe-save-v1"}\n')
sys.stdout.flush()
try:
    answer = main()
except Exception as error:
    answer = {"status": "error", "message": str(error) + "; your changes are kept locally. Retry checks the remote version first."}
try:
    sys.stdout.write(json.dumps(answer, ensure_ascii=True))
    sys.stdout.flush()
except BrokenPipeError:
    pass
