"""Minimal, bounded ISO-BMFF/HEIF item parser (no third-party dependencies).

It reads exactly what the viewer and the independent pixel reference need:
the primary item (`pitm`), item types and flags (`iinf`/`infe`), item data
locations (`iloc`, construction methods 0 and 1), references (`iref`) and
item properties (`ipco`/`ipma`). It never decodes HEVC.

`primary_only()` mirrors the viewer's Rust `primary_only` (#2177): libheif's
`heif-convert` writes every non-hidden top-level image in item order, so the
viewer would otherwise show the first one, not the primary.
"""
from fractions import Fraction
import struct


class HeifError(ValueError):
    pass


def boxes(data, start=0, end=None):
    """Yield (type, payload_start, box_end, header_start) for each child box."""
    end = len(data) if end is None else end
    offset = start
    while offset < end:
        if end - offset < 8:
            raise HeifError('truncated box header')
        size, kind = struct.unpack_from('>I4s', data, offset)
        header = 8
        if size == 1:
            if end - offset < 16:
                raise HeifError('truncated large box header')
            size = struct.unpack_from('>Q', data, offset + 8)[0]
            header = 16
        elif size == 0:
            size = end - offset
        if size < header or offset + size > end:
            raise HeifError('box exceeds its parent: ' + kind.decode('latin-1'))
        yield kind.decode('latin-1'), offset + header, offset + size, offset
        offset += size


def find(data, kind, start=0, end=None):
    for found, payload, box_end, _ in boxes(data, start, end):
        if found == kind:
            return payload, box_end
    return None


class Reader:
    def __init__(self, data, offset, end):
        self.data, self.offset, self.end = data, offset, end

    def take(self, count):
        if count < 0 or self.offset + count > self.end:
            raise HeifError('truncated box payload')
        value = self.data[self.offset:self.offset + count]
        self.offset += count
        return value

    def uint(self, size):
        return int.from_bytes(self.take(size), 'big') if size else 0

    def sint(self, size):
        return int.from_bytes(self.take(size), 'big', signed=True)

    def full(self):
        version = self.uint(1)
        flags = self.uint(3)
        return version, flags


class Item:
    def __init__(self, item_id, kind, hidden, infe_flags_offset):
        self.id, self.type, self.hidden = item_id, kind, hidden
        self.flags_offset = infe_flags_offset
        self.extents, self.construction = [], 0
        self.properties = []  # (essential, property) in ipma order


class Heif:
    """Parsed top-level `meta` of a HEIF file."""

    def __init__(self, data):
        data = bytes(data)
        self.data = data
        if len(data) < 16 or data[4:8] != b'ftyp':
            raise HeifError('not an ISO-BMFF file')
        meta = find(data, 'meta')
        if meta is None:
            raise HeifError('no top-level meta box')
        start, end = meta[0] + 4, meta[1]  # meta is a FullBox
        self.primary = None
        self.items = {}
        self.order = []
        self.refs = []  # (type, from, [to])
        self.idat = None
        properties, associations, locations = [], [], {}
        for kind, payload, box_end, _ in boxes(data, start, end):
            if kind == 'pitm':
                r = Reader(data, payload, box_end)
                version, _ = r.full()
                self.primary = r.uint(2 if version == 0 else 4)
            elif kind == 'iinf':
                self._iinf(payload, box_end)
            elif kind == 'iloc':
                locations = self._iloc(payload, box_end)
            elif kind == 'iref':
                r = Reader(data, payload, box_end)
                version, _ = r.full()
                width = 2 if version == 0 else 4
                for ref, ref_payload, ref_end, _ in boxes(data, r.offset, box_end):
                    rr = Reader(data, ref_payload, ref_end)
                    source = rr.uint(width)
                    targets = [rr.uint(width) for _ in range(rr.uint(2))]
                    self.refs.append((ref, source, targets))
            elif kind == 'iprp':
                ipco = find(data, 'ipco', payload, box_end)
                if ipco:
                    properties = [(k, p, e) for k, p, e, _ in boxes(data, *ipco)]
                for k, p, e, _ in boxes(data, payload, box_end):
                    if k == 'ipma':
                        associations.append((p, e))
            elif kind == 'idat':
                self.idat = (payload, box_end)
        if self.primary is None:
            raise HeifError('no primary item')
        for item_id, (method, extents) in locations.items():
            if item_id in self.items:
                self.items[item_id].construction = method
                self.items[item_id].extents = extents
        for payload, box_end in associations:
            r = Reader(data, payload, box_end)
            version, flags = r.full()
            for _ in range(r.uint(4)):
                item_id = r.uint(2 if version < 1 else 4)
                for _ in range(r.uint(1)):
                    value = r.uint(2 if flags & 1 else 1)
                    bits = 15 if flags & 1 else 7
                    index = value & ((1 << bits) - 1)
                    essential = bool(value >> bits)
                    if index == 0:
                        continue
                    if index > len(properties):
                        raise HeifError('ipma names a missing property')
                    if item_id in self.items:
                        self.items[item_id].properties.append((essential, properties[index - 1]))
        if self.primary not in self.items:
            raise HeifError('primary item is not described in iinf')

    def _iinf(self, payload, end):
        r = Reader(self.data, payload, end)
        version, _ = r.full()
        r.uint(2 if version == 0 else 4)
        for kind, infe, infe_end, header in boxes(self.data, r.offset, end):
            if kind != 'infe':
                continue
            ir = Reader(self.data, infe, infe_end)
            infe_version, flags = ir.full()
            if infe_version < 2:
                continue  # pre-HEIF item entries carry no item type
            item_id = ir.uint(2 if infe_version == 2 else 4)
            ir.uint(2)
            item_type = ir.take(4).decode('latin-1')
            # Least significant flags byte: version(1) + flags(3) after header.
            item = Item(item_id, item_type, bool(flags & 1), infe + 3)
            self.items[item_id] = item
            self.order.append(item_id)

    def _iloc(self, payload, end):
        r = Reader(self.data, payload, end)
        version, _ = r.full()
        sizes = r.uint(1)
        offset_size, length_size = sizes >> 4, sizes & 15
        sizes = r.uint(1)
        base_size, index_size = sizes >> 4, (sizes & 15 if version in (1, 2) else 0)
        locations = {}
        for _ in range(r.uint(2 if version < 2 else 4)):
            item_id = r.uint(2 if version < 2 else 4)
            method = r.uint(2) & 15 if version in (1, 2) else 0
            r.uint(2)  # data_reference_index
            base = r.uint(base_size)
            extents = []
            for _ in range(r.uint(2)):
                r.uint(index_size)
                extents.append((base + r.uint(offset_size), r.uint(length_size)))
            locations[item_id] = (method, extents)
        return locations

    def item_data(self, item_id, limit=64 * 1024 * 1024):
        item = self.items[item_id]
        if item.construction == 0:
            start, end = 0, len(self.data)
        elif item.construction == 1 and self.idat:
            start, end = self.idat
        else:
            raise HeifError('unsupported iloc construction method')
        out = bytearray()
        for offset, length in item.extents:
            if length == 0:
                length = end - start - offset
            if offset < 0 or start + offset + length > end or len(out) + length > limit:
                raise HeifError('item extent outside the file')
            out += self.data[start + offset:start + offset + length]
        return bytes(out)

    def references(self, kind, source=None, target=None):
        for ref, ref_from, ref_to in self.refs:
            if ref == kind and (source is None or ref_from == source) and (
                    target is None or target in ref_to):
                yield ref_from, ref_to

    def property(self, item_id, kind):
        for _, (found, payload, end) in self.items[item_id].properties:
            if found == kind:
                return payload, end
        return None


def primary_only(data):
    """Return a copy whose non-primary `infe` entries carry the hidden flag.

    Only the least significant flags byte of each `infe` changes; no box size
    or offset moves. Hidden items still serve as grid tiles, `iden` sources,
    alpha planes and thumbnails, so the primary decodes unchanged while
    libheif's converter no longer writes the other top-level images.
    """
    try:
        heif = Heif(data)
    except (HeifError, struct.error, IndexError):
        return bytes(data)  # unreadable: left for libheif to judge, as in Rust
    out = bytearray(data)
    for item in heif.items.values():
        if item.id != heif.primary:
            out[item.flags_offset] |= 1
    return bytes(out)


# --- transformative properties (ISO/IEC 23008-12 6.5.9-6.5.12) -----------

def clap_rect(payload, end, data, width, height):
    """Return (left, top, crop_width, crop_height) like libheif's rounding."""
    r = Reader(data, payload, end)
    w_n, w_d, h_n, h_d = r.uint(4), r.uint(4), r.uint(4), r.uint(4)
    x_n, x_d, y_n, y_d = r.sint(4), r.uint(4), r.sint(4), r.uint(4)
    if not (w_d and h_d and x_d and y_d):
        raise HeifError('clap has a zero denominator')
    crop_w, crop_h = Fraction(w_n, w_d), Fraction(h_n, h_d)
    # The clean aperture is centred on the image centre plus the offset.
    left = Fraction(width - 1, 2) + Fraction(x_n, x_d) - (crop_w - 1) / 2
    top = Fraction(height - 1, 2) + Fraction(y_n, y_d) - (crop_h - 1) / 2

    def rounded(value):
        return int(value + Fraction(1, 2)) if value >= 0 else -int(-value + Fraction(1, 2))

    left, top = rounded(left), rounded(top)
    right = rounded(left + crop_w - 1)
    bottom = rounded(top + crop_h - 1)
    if left < 0 or top < 0 or right >= width or bottom >= height or right < left or bottom < top:
        raise HeifError('clap outside the image')
    return left, top, right - left + 1, bottom - top + 1


def transforms(heif, item_id):
    """Yield ('clap', rect-fn) / ('irot', quarter_turns_ccw) / ('imir', axis) in ipma order."""
    for _, (kind, payload, end) in heif.items[item_id].properties:
        if kind == 'clap':
            yield 'clap', (payload, end)
        elif kind == 'irot':
            yield 'irot', heif.data[payload] & 3
        elif kind == 'imir':
            yield 'imir', heif.data[payload] & 1


def ispe(heif, item_id):
    found = heif.property(item_id, 'ispe')
    if not found:
        raise HeifError('item has no ispe')
    r = Reader(heif.data, *found)
    r.full()
    return r.uint(4), r.uint(4)
