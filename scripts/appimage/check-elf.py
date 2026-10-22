"""Check x86_64 ELF relative relocations without loading or executing the payload."""
from pathlib import Path
import struct
import sys


def validate(data):
    if data[:6] != b'\x7fELF\x02\x01' or struct.unpack_from('<H', data, 18)[0] != 62:
        raise ValueError('Expected a little-endian ELF64 x86_64 payload')
    phoff = struct.unpack_from('<Q', data, 32)[0]
    phentsize, phnum = struct.unpack_from('<HH', data, 54)
    headers = [struct.unpack_from('<IIQQQQQQ', data, phoff + i * phentsize)
               for i in range(phnum)]
    loads = [h for h in headers if h[0] == 1]
    dynamic = next(h for h in headers if h[0] == 2)
    tags = {}
    for offset in range(dynamic[2], dynamic[2] + dynamic[5], 16):
        tag, value = struct.unpack_from('<qQ', data, offset)
        if tag == 0:
            break
        tags[tag] = value
    # DT_RELA, DT_RELASZ, DT_RELAENT, DT_RELACOUNT. ldd alone does not
    # apply these relocations and accepted the corrupted PR #1471 artifact.
    address, size, entry, count = (tags[t] for t in (7, 8, 9, 0x6ffffff9))
    if entry != 24 or count == 0 or count * entry > size:
        raise ValueError('Invalid relative relocation table size/count')
    segment = next(h for h in loads if h[3] <= address
                   and address + size <= h[3] + h[5])
    start = segment[2] + address - segment[3]
    if start < phoff + phentsize * phnum and start + size > phoff:
        raise ValueError('Program headers overlap the relocation table')
    for i in range(count):
        info = struct.unpack_from('<Q', data, start + i * entry + 8)[0]
        if info != 8:  # symbol zero, R_X86_64_RELATIVE
            raise ValueError(f'Relative relocation {i} has invalid r_info {info:#x}')
    return count


if __name__ == '__main__':
    path = Path(sys.argv[1])
    print(f'{path}: {validate(path.read_bytes())} relative relocations valid; '
          'program headers do not overlap the relocation table')
