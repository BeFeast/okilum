"""Observe priority inheritance only; never change priority or budget clocks."""
import ctypes
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import time

kernel = ctypes.WinDLL('kernel32', use_last_error=True)
kernel.GetCurrentProcess.restype = ctypes.c_void_p
kernel.GetPriorityClass.argtypes = [ctypes.c_void_p]
kernel.GetPriorityClass.restype = ctypes.c_uint32
class ThreadEntry(ctypes.Structure):
    _fields_ = [('size', ctypes.c_uint32), ('usage', ctypes.c_uint32), ('id', ctypes.c_uint32), ('pid', ctypes.c_uint32), ('base', ctypes.c_int32), ('delta', ctypes.c_int32), ('flags', ctypes.c_uint32)]
kernel.CreateToolhelp32Snapshot.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
kernel.CreateToolhelp32Snapshot.restype = ctypes.c_void_p
kernel.Thread32First.argtypes = [ctypes.c_void_p, ctypes.POINTER(ThreadEntry)]
kernel.Thread32Next.argtypes = [ctypes.c_void_p, ctypes.POINTER(ThreadEntry)]
kernel.CloseHandle.argtypes = [ctypes.c_void_p]

def thread_priorities(pid):
    snapshot = kernel.CreateToolhelp32Snapshot(4, 0)
    assert snapshot not in [0, ctypes.c_void_p(-1).value]
    entry = ThreadEntry(size=ctypes.sizeof(ThreadEntry))
    results = []
    try:
        more = kernel.Thread32First(snapshot, ctypes.byref(entry))
        while more:
            if entry.pid == pid:
                results.append({'id': entry.id, 'base_priority': entry.base})
            more = kernel.Thread32Next(snapshot, ctypes.byref(entry))
    finally:
        kernel.CloseHandle(snapshot)
    return results

normal = 0x20
print('PARENT_PRIORITY', json.dumps({'class': kernel.GetPriorityClass(kernel.GetCurrentProcess()), 'normal_class': normal}), flush=True)
spec = importlib.util.spec_from_file_location('native', Path(__file__).with_name('code-toolbar-native-app.py'))
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)
original_run = subprocess.run

def observe(args, **kwargs):
    assert len(args) == 1 and str(args[0]).endswith('okilum.exe')
    timeout = kwargs.pop('timeout')
    with subprocess.Popen(args, **kwargs) as process:
        priority = kernel.GetPriorityClass(int(process._handle))
        assert priority != 0
        print('CHILD_PRIORITY', json.dumps({'pid': process.pid, 'class': priority, 'normal_class': normal}), flush=True)
        meta = Path('diagnostics/scheduling/priority.json')
        meta.write_text(json.dumps({'pid': process.pid, 'class': priority, 'parent_class': kernel.GetPriorityClass(kernel.GetCurrentProcess()), 'normal_class': normal, 'threads': thread_priorities(process.pid)}, indent=2))
        process.wait(timeout=timeout)
        return subprocess.CompletedProcess(args, process.returncode)

native.subprocess.run = observe
sys.argv = ['code-toolbar-native-app.py', str(Path('inputs/toolbar/okilum.exe').resolve())]
native.main()
