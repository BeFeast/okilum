#!/usr/bin/env python3
"""Read ETL with WinSDK EVENT_RECORD + RAW_TIMESTAMP; never infer frame time."""
import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import struct
import uuid

U8, U16, U32, U64, I64, PTR = C.c_uint8, C.c_uint16, C.c_uint32, C.c_uint64, C.c_int64, C.c_void_p
THREAD = '3d6fa8d1-fe05-11d0-9dda-00c04fd7ba7c'
PROCESS = '3d6fa8d0-fe05-11d0-9dda-00c04fd7ba7c'
IMAGE = '2cb15d1d-5fc1-11d2-abe1-00a0c911f518'
STACK = 'def2fe46-7bd6-4b80-bd94-f57fe20d0ce3'
MARKER = '97057e99-44cf-4e25-aeb7-139ea0cdb951'
RAW_MODE = 0x10000000 | 0x1000


class Guid(C.Structure):
    _fields_ = [('bytes', U8 * 16)]


class Descriptor(C.Structure):
    _fields_ = [('id', U16), ('version', U8), ('channel', U8), ('level', U8),
               ('opcode', U8), ('task', U16), ('keyword', U64)]


class EventHeader(C.Structure):
    _fields_ = [('size', U16), ('type', U16), ('flags', U16), ('property', U16),
               ('tid', U32), ('pid', U32), ('stamp', I64), ('provider', Guid),
               ('descriptor', Descriptor), ('processor_time', U64), ('activity', Guid)]


class EventRecord(C.Structure):
    _fields_ = [('header', EventHeader), ('buffer_context', U32), ('extended_count', U16),
               ('data_length', U16), ('extended', PTR), ('data', PTR), ('context', PTR)]


class LegacyHeader(C.Structure):
    _fields_ = [('size', U16), ('flags', U16), ('version', U32), ('tid', U32),
               ('pid', U32), ('stamp', I64), ('guid', Guid), ('time_union', U64)]


class LegacyEvent(C.Structure):
    _fields_ = [('header', LegacyHeader), ('instance', U32), ('parent_instance', U32),
               ('parent_guid', Guid), ('data', PTR), ('length', U32), ('context', U32)]


class TimeZone(C.Structure):
    _fields_ = [('bias', C.c_int32), ('standard_name', U16 * 32), ('standard_date', U16 * 8),
               ('standard_bias', C.c_int32), ('daylight_name', U16 * 32),
               ('daylight_date', U16 * 8), ('daylight_bias', C.c_int32)]


class LogHeader(C.Structure):
    _fields_ = [('buffer_size', U32), ('version', U32), ('provider_version', U32),
               ('processors', U32), ('end', I64), ('timer_resolution', U32),
               ('max_size', U32), ('mode', U32), ('buffers_written', U32),
               ('start_buffers', U32), ('pointer_size', U32), ('events_lost', U32),
               ('cpu_mhz', U32), ('logger_name', PTR), ('file_name', PTR),
               ('zone', TimeZone), ('boot', I64), ('frequency', I64), ('start', I64),
               ('clock_type', U32), ('buffers_lost', U32)]


class LogFile(C.Structure):
    _fields_ = [('file_name', PTR), ('logger_name', PTR), ('current_time', I64),
               ('buffers_read', U32), ('process_mode', U32), ('current_event', LegacyEvent),
               ('header', LogHeader), ('buffer_callback', PTR), ('buffer_size', U32),
               ('filled', U32), ('events_lost', U32), ('event_callback', PTR),
               ('is_kernel', U32), ('context', PTR)]


def verify_layouts():
    # WinSDK x64 layouts, independently represented by pinned windows-sys 0.61.2.
    expected = [(Guid, 16), (Descriptor, 16), (EventHeader, 80), (EventRecord, 112),
                (LegacyHeader, 48), (LegacyEvent, 88), (TimeZone, 172),
                (LogHeader, 280), (LogFile, 448)]
    for kind, size in expected:
        assert C.sizeof(kind) == size, (kind.__name__, C.sizeof(kind), size)
    assert LogFile.header.offset == 120 and LogFile.event_callback.offset == 424
    assert LogHeader.frequency.offset == 256 and LogHeader.events_lost.offset == 48
    assert EventRecord.data.offset == 96


def export(path, output, target_tids):
    assert os.name == 'nt' and C.sizeof(PTR) == 8, 'Windows x64 exporter only'
    verify_layouts()
    api = C.WinDLL('advapi32', use_last_error=True)
    api.OpenTraceW.argtypes = [C.POINTER(LogFile)]
    api.OpenTraceW.restype = U64
    api.ProcessTrace.argtypes = [C.POINTER(U64), U32, PTR, PTR]
    api.ProcessTrace.restype = U32
    api.CloseTrace.argtypes = [U64]
    api.CloseTrace.restype = U32
    record_type = C.WINFUNCTYPE(None, C.POINTER(EventRecord))
    buffer_type = C.WINFUNCTYPE(U32, C.POINTER(LogFile))
    failures, losses, counts = [], [], {}
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open('w', encoding='utf-8') as stream:
        @record_type
        def record(pointer):
            try:
                value = pointer.contents
                header = value.header
                provider = str(uuid.UUID(bytes_le=bytes(header.provider.bytes)))
                if provider not in [THREAD, PROCESS, IMAGE, STACK, MARKER]:
                    return
                payload = C.string_at(value.data, value.data_length)
                op = header.descriptor.opcode
                if provider == THREAD:
                    if op == 36:
                        assert len(payload) >= 16
                    elif op == 50:
                        assert len(payload) >= 4
                    elif op in [1, 2, 3, 4, 72]:
                        assert len(payload) >= 8
                        # Preserve waker lifetimes, including pre-existing
                        # threads represented by DCStart/DCEnd rundown.
                    else:
                        return
                # ReadyThread stackwalk belongs to the WAKER, not the target
                # UI thread. Retain all stacks AND scheduler origins, so an
                # event-QPC/CPU collision cannot attach an unrelated stack.
                counts[provider] = counts.get(provider, 0) + 1
                stream.write(json.dumps({'qpc': header.stamp, 'provider': provider, 'opcode': op,
                                        'version': header.descriptor.version, 'pid': header.pid,
                                        'tid': header.tid, 'cpu': value.buffer_context & 0xffff,
                                        'flags': header.flags, 'payload_hex': payload.hex()}) + '\n')
            except Exception as error:
                failures.append(repr(error))

        @buffer_type
        def buffer(pointer):
            losses.append(pointer.contents.events_lost)
            return 1

        name = C.create_unicode_buffer(str(path.resolve()))
        logfile = LogFile()
        logfile.file_name = C.cast(name, PTR)
        logfile.process_mode = RAW_MODE
        logfile.event_callback = C.cast(record, PTR)
        logfile.buffer_callback = C.cast(buffer, PTR)
        handle = api.OpenTraceW(C.byref(logfile))
        assert handle != 0xffffffffffffffff, ('OpenTraceW', C.get_last_error())
        header = logfile.header
        # Copy these before CloseTrace releases any backing string storage.
        meta = {'raw_timestamp_mode': RAW_MODE, 'frequency': header.frequency,
                'requested_primary_tids': sorted(target_tids),
                'clock_type': header.clock_type, 'pointer_size': header.pointer_size,
                'events_lost': header.events_lost, 'buffers_lost': header.buffers_lost,
                'trace_start_filetime': header.start, 'trace_end_filetime': header.end}
        try:
            handles = (U64 * 1)(handle)
            status = api.ProcessTrace(handles, 1, None, None)
            meta.update({'process_trace_status': status, 'events_lost_after': logfile.events_lost,
                         'buffer_loss_samples': losses, 'callback_errors': failures, 'counts': counts})
        finally:
            meta['close_trace_status'] = api.CloseTrace(handle)
    meta['input_sha256'] = hashlib.sha256(path.read_bytes()).hexdigest()
    meta['output_sha256'] = hashlib.sha256(output.read_bytes()).hexdigest()
    output.with_suffix('.meta.json').write_text(json.dumps(meta, indent=2))
    assert meta['process_trace_status'] == 0 and meta['close_trace_status'] == 0, meta
    assert meta['clock_type'] == 1 and meta['frequency'] > 0, meta
    assert meta['events_lost'] == meta['buffers_lost'] == meta['events_lost_after'] == 0, meta
    assert losses and all(value == 0 for value in losses) and not failures, meta


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('etl', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--cases', type=Path, required=True)
    args = parser.parse_args()
    cases = json.loads(args.cases.read_text())
    export(args.etl, args.output, {case['primary_tid'] for case in cases})
