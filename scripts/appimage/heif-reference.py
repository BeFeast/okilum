#!/usr/bin/env python3
"""Compare the Linux viewer's HEIF decode with an independent pixel reference (#2177).

Viewer path: the exact bytes the viewer hands to libheif (`primary_only`), run
through `heif-convert` under the viewer's `prlimit` bounds.

Reference path: no libheif code. `heif_boxes` (this directory) parses the
container; each coded HEVC item becomes an Annex B stream (hvcC parameter
sets + item NAL units) decoded by FFmpeg's own HEVC decoder (`-f hevc`, not
its HEIF demuxer). Grid composition, `iden`, `clap`, `irot` and `imir` are
applied here in `ipma` order; alpha comes from the `auxl` item's luma,
mapped from its coded range (limited-range 16..235 -> 0..255).

Thresholds were fixed before the first comparison: equal dimensions, mean
absolute RGB delta <= 1.5, at least 99.9% of RGB samples within 8 levels and
alpha within 1 level. YCbCr->RGB rounding and chroma upsampling legitimately
differ between decoders, which is why the colour comparison is not exact.
Every fixture also runs negative controls (each geometric step perturbed);
each must fail the same thresholds, so a pass cannot come from a blind metric.

Usage: heif-reference.py [--fixtures fixtures.json] [--json out.json] [--heif-convert PATH]
Network: downloads each pinned fixture and checks its SHA-256.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import urllib.request

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import heif_boxes as hb  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_FIXTURES = ROOT / 'docs/evidence/composer-heif-parity/fixtures.json'
LIMITS = ['--as=1073741824', '--fsize=268435456', '--cpu=20', '--core=0', '--']
MEAN_LIMIT, NEAR_LEVELS, NEAR_SHARE, ALPHA_LIMIT = 1.5, 8, 0.999, 1

# H.273 matrix_coefficients -> FFmpeg scale filter names.
ALPHA_URNS = {b'urn:mpeg:hevc:2015:auxid:1', b'urn:mpeg:mpegB:cicp:systems:auxiliary:alpha'}
MATRICES = {1: 'bt709', 5: 'bt470bg', 6: 'smpte170m', 9: 'bt2020', 10: 'bt2020'}


def annexb(heif, item_id):
    found = heif.property(item_id, 'hvcC')
    if not found:
        raise hb.HeifError('hvc1 item without hvcC')
    payload, end = found
    data = heif.data
    r = hb.Reader(data, payload + 21, end)
    length_size = (r.uint(1) & 3) + 1
    stream = bytearray()
    for _ in range(r.uint(1)):
        r.uint(1)
        for _ in range(r.uint(2)):
            stream += b'\0\0\0\1' + r.take(r.uint(2))
    sample = heif.item_data(item_id)
    offset = 0
    while offset < len(sample):
        size = int.from_bytes(sample[offset:offset + length_size], 'big')
        offset += length_size
        if size == 0 or offset + size > len(sample):
            raise hb.HeifError('bad NAL length in item data')
        stream += b'\0\0\0\1' + sample[offset:offset + size]
        offset += size
    return bytes(stream)


def nclx(heif, item_id):
    found = heif.property(item_id, 'colr')
    if not found:
        return None
    payload, end = found
    if heif.data[payload:payload + 4] != b'nclx':
        return None
    r = hb.Reader(heif.data, payload + 4, end)
    _primaries, _transfer, matrix = r.uint(2), r.uint(2), r.uint(2)
    return matrix, bool(r.uint(1) >> 7)


def ffmpeg_decode(heif, item_id, gray=False):
    width, height = hb.ispe(heif, item_id)
    scale = 'scale=flags=accurate_rnd+full_chroma_int+neighbor'
    colour = nclx(heif, item_id)
    if colour and not gray:
        matrix, full = colour
        if matrix in MATRICES:
            scale += ':in_color_matrix=' + MATRICES[matrix]
        scale += ':in_range=' + ('full' if full else 'limited')
    if gray:
        # Alpha is the auxiliary image's luma plane; range applied below.
        command = ['ffmpeg', '-v', 'error', '-f', 'hevc', '-i', '-', '-frames:v', '1',
                   '-vf', 'extractplanes=y', '-f', 'rawvideo', '-pix_fmt', 'gray', '-']
    else:
        command = ['ffmpeg', '-v', 'error', '-f', 'hevc', '-i', '-', '-frames:v', '1',
                   '-vf', scale, '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-']
    result = subprocess.run(command, input=annexb(heif, item_id), capture_output=True, timeout=60)
    if result.returncode:
        raise hb.HeifError('ffmpeg: ' + result.stderr.decode(errors='replace').strip())
    channels = 1 if gray else 3
    if len(result.stdout) != width * height * channels:
        raise hb.HeifError(f'ffmpeg output size differs from ispe {width}x{height}')
    image = np.frombuffer(result.stdout, np.uint8).reshape(height, width, channels)
    if gray and coded_range(heif, item_id) == 'tv':
        # A limited-range alpha picture (VUI video_full_range_flag 0) maps
        # 16..235 to transparent..opaque, like any limited-range luma.
        image = np.clip(np.round((image.astype(np.float64) - 16) * 255 / 219), 0, 255).astype(np.uint8)
    return image


def coded_range(heif, item_id):
    """'tv' or 'pc' from the item's own HEVC VUI, as FFmpeg parses it."""
    probe = subprocess.run(['ffprobe', '-v', 'error', '-f', 'hevc', '-show_entries',
                            'stream=color_range', '-of', 'csv=p=0', '-'],
                           input=annexb(heif, item_id), capture_output=True, timeout=60)
    return probe.stdout.decode().strip() or 'unknown'


def apply_transforms(heif, item_id, image, perturb=None):
    """perturb = (step_index, kind) shifts one step for a negative control."""
    for index, (kind, value) in enumerate(hb.transforms(heif, item_id)):
        wrong = perturb is not None and perturb[0] == index
        if kind == 'clap':
            left, top, width, height = hb.clap_rect(*value, heif.data, image.shape[1], image.shape[0])
            if wrong:
                left = left + 8 if left + 8 + width <= image.shape[1] else left - 8
            image = image[top:top + height, left:left + width]
        elif kind == 'irot':
            image = np.rot90(image, (4 - value) % 4 if wrong else value)
        elif kind == 'imir':
            # axis 0 exchanges top and bottom, 1 left and right: libheif's
            # reading, and Nokia's C042 description ("vertically mirrored").
            axis = value ^ 1 if wrong else value
            image = image[::-1] if axis == 0 else image[:, ::-1]
    return np.ascontiguousarray(image)


def render(heif, item_id, perturb=None, alpha=False):
    item = heif.items[item_id]
    if item.type == 'hvc1':
        image = ffmpeg_decode(heif, item_id, gray=alpha)
    elif item.type == 'iden':
        (_, sources), = heif.references('dimg', source=item_id)
        image = render(heif, sources[0], alpha=alpha)
    elif item.type == 'grid':
        data = heif.item_data(item_id)
        rows, columns = data[2] + 1, data[3] + 1
        size = 4 if data[1] & 1 else 2
        width = int.from_bytes(data[4:4 + size], 'big')
        height = int.from_bytes(data[4 + size:4 + 2 * size], 'big')
        (_, tiles), = heif.references('dimg', source=item_id)
        if len(tiles) != rows * columns:
            raise hb.HeifError('grid tile count differs from rows x columns')
        if perturb == 'tiles':
            tiles = tiles[1:] + tiles[:1]
        decoded = [render(heif, tile, alpha=alpha) for tile in tiles]
        tile_h, tile_w = decoded[0].shape[:2]
        canvas = np.zeros((tile_h * rows, tile_w * columns, decoded[0].shape[2]), np.uint8)
        for index, tile in enumerate(decoded):
            y, x = divmod(index, columns)
            canvas[y * tile_h:(y + 1) * tile_h, x * tile_w:(x + 1) * tile_w] = tile
        image = canvas[:height, :width]
    else:
        raise hb.HeifError('unsupported item type ' + item.type)
    return apply_transforms(heif, item_id, image, None if perturb == 'tiles' else perturb)


def alpha_item(heif, item_id):
    for source, targets in heif.references('auxl', target=item_id):
        found = heif.property(source, 'auxC')
        urn = heif.data[found[0] + 4:found[1]].split(b'\0')[0] if found else b''
        if urn in ALPHA_URNS:
            return source
    return None


def reference(heif, perturb=None):
    image = render(heif, heif.primary, perturb)
    source = alpha_item(heif, heif.primary)
    if source is not None:
        alpha = render(heif, source, alpha=True)
        alpha = apply_transforms(heif, heif.primary, alpha, perturb if isinstance(perturb, tuple) else None)
        image = np.concatenate([image, alpha], axis=2)
    return image


def png_pixels(path):
    command = ['ffmpeg', '-v', 'error', '-i', str(path), '-f', 'rawvideo', '-pix_fmt', 'rgba', '-']
    probe = subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-show_entries',
                            'stream=width,height,pix_fmt', '-of', 'json', str(path)],
                           capture_output=True, check=True)
    stream = json.loads(probe.stdout)['streams'][0]
    raw = subprocess.run(command, capture_output=True, check=True).stdout
    image = np.frombuffer(raw, np.uint8).reshape(stream['height'], stream['width'], 4)
    return image if 'a' in stream['pix_fmt'] else image[:, :, :3]


def compare(viewer, expected):
    if viewer.shape[:2] != expected.shape[:2]:
        return {'verdict': 'FAIL', 'reason': f'dimensions {viewer.shape[1]}x{viewer.shape[0]} '
                f'vs reference {expected.shape[1]}x{expected.shape[0]}'}
    delta = np.abs(viewer[:, :, :3].astype(np.int16) - expected[:, :, :3].astype(np.int16))
    result = {
        'rgb_mean_absolute_delta': round(float(delta.mean()), 4),
        'rgb_max_absolute_delta': int(delta.max()),
        'rgb_within_8_share': round(float((delta <= NEAR_LEVELS).mean()), 6),
    }
    ok = result['rgb_mean_absolute_delta'] <= MEAN_LIMIT and result['rgb_within_8_share'] >= NEAR_SHARE
    has_alpha = expected.shape[2] == 4
    if has_alpha != (viewer.shape[2] == 4):
        result['reason'] = 'alpha channel present in only one image'
        ok = False
    elif has_alpha:
        alpha = np.abs(viewer[:, :, 3].astype(np.int16) - expected[:, :, 3].astype(np.int16))
        result['alpha_max_absolute_delta'] = int(alpha.max())
        ok = ok and result['alpha_max_absolute_delta'] <= ALPHA_LIMIT
    result['verdict'] = 'PASS' if ok else 'FAIL'
    return result


def perturbations(heif):
    steps = list(hb.transforms(heif, heif.primary))
    # irot by 180 degrees is its own inverse, so skip that control.
    controls = [(index, kind) for index, (kind, value) in enumerate(steps)
                if not (kind == 'irot' and value == 2)]
    if heif.items[heif.primary].type == 'grid':
        controls.append('tiles')
    return controls


def viewer_decode(source, converter, work):
    snapshot = work / 'source.heic'
    snapshot.write_bytes(hb.primary_only(source))
    result = subprocess.run(['prlimit', *LIMITS, converter, str(snapshot), 'preview.png'],
                            cwd=work, capture_output=True, timeout=30)
    if result.returncode:
        raise RuntimeError('viewer decode failed: ' + result.stderr.decode(errors='replace'))
    outputs = sorted(work.glob('preview*.png'))
    if [path.name for path in outputs] != ['preview.png']:
        raise RuntimeError('viewer decode wrote ' + ', '.join(p.name for p in outputs))
    return png_pixels(outputs[0])


def fetch(entry, cache):
    path = cache / entry['name']
    if not path.exists():
        with urllib.request.urlopen(entry['url'], timeout=60) as response:
            path.write_bytes(response.read())
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != entry['sha256']:
        raise RuntimeError(entry['name'] + ': SHA-256 differs from fixtures.json')
    return data


def run(arguments):
    entries = json.loads(Path(arguments.fixtures).read_text())
    version = subprocess.run([arguments.heif_convert, '--version'], capture_output=True, text=True)
    if version.returncode:  # libheif 1.12 has no --version; its help names it
        version = subprocess.run([arguments.heif_convert, '-h'], capture_output=True, text=True)
        version.stdout = '\n'.join(line for line in (version.stdout + version.stderr).splitlines()
                                   if 'version' in line.lower()) or 'unknown'
        version.stderr = ''
    ffmpeg = subprocess.run(['ffmpeg', '-version'], capture_output=True, text=True)
    report = {
        'viewer': {'heif_convert': arguments.heif_convert,
                   'version': (version.stdout + version.stderr).strip().splitlines()[0]},
        'reference': {'decoder': ffmpeg.stdout.splitlines()[0],
                      'path': 'heif_boxes.py container parse + Annex B per item + FFmpeg hevc decoder'},
        'thresholds': {'rgb_mean_absolute_delta_max': MEAN_LIMIT, 'rgb_within_8_share_min': NEAR_SHARE,
                       'alpha_max_absolute_delta_max': ALPHA_LIMIT},
        'fixtures': [],
    }
    failed = False
    with tempfile.TemporaryDirectory() as temporary:
        cache = Path(arguments.cache) if arguments.cache else Path(temporary) / 'cache'
        cache.mkdir(parents=True, exist_ok=True)
        for entry in entries:
            source = fetch(entry, cache)
            heif = hb.Heif(source)
            work = Path(tempfile.mkdtemp(dir=temporary))
            row = {'fixture': entry['name'], 'sha256': entry['sha256'],
                   'primary_item': heif.primary, 'primary_type': heif.items[heif.primary].type,
                   'transforms': [kind for kind, _ in hb.transforms(heif, heif.primary)]}
            alpha = alpha_item(heif, heif.primary)
            if alpha is not None:
                row['alpha_coded_range'] = coded_range(heif, alpha)
            try:
                viewer = viewer_decode(source, arguments.heif_convert, work)
            except (RuntimeError, subprocess.TimeoutExpired) as error:
                # A visible viewer error, not a crash: still a failed decode.
                row['result'] = {'verdict': 'FAIL', 'reason': str(error).strip()[:300]}
                report['fixtures'].append(row)
                failed = True
                print(f"FAIL {entry['name']} viewer: {row['result']['reason']}")
                continue
            row.update({'width': int(viewer.shape[1]), 'height': int(viewer.shape[0]),
                        'alpha': viewer.shape[2] == 4})
            row['result'] = compare(viewer, reference(heif))
            controls = []
            for perturb in perturbations(heif):
                label = perturb if isinstance(perturb, str) else f'{perturb[1]}#{perturb[0]}'
                outcome = compare(viewer, reference(heif, perturb))
                controls.append({'control': label, **outcome})
                failed |= outcome['verdict'] != 'FAIL'
            # Whole-image control: the metric must reject a mirrored decode.
            outcome = compare(viewer[:, ::-1], reference(heif))
            controls.append({'control': 'mirrored-viewer', **outcome})
            failed |= outcome['verdict'] != 'FAIL'
            row['negative_controls'] = controls
            failed |= row['result']['verdict'] != 'PASS'
            report['fixtures'].append(row)
            print(f"{row['result']['verdict']} {entry['name']} {row['width']}x{row['height']} "
                  f"mean={row['result'].get('rgb_mean_absolute_delta')} "
                  f"near8={row['result'].get('rgb_within_8_share')} "
                  f"alpha={row['result'].get('alpha_max_absolute_delta', '-')} "
                  f"controls={'/'.join(c['verdict'] for c in controls)}")
    report['verdict'] = 'FAIL' if failed else 'PASS'
    if arguments.json:
        Path(arguments.json).write_text(json.dumps(report, indent=2) + '\n')
    print('HEIF_REFERENCE_' + report['verdict'])
    return 1 if failed else 0


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--fixtures', default=str(DEFAULT_FIXTURES))
    parser.add_argument('--json')
    parser.add_argument('--cache', help='directory for downloaded fixtures (kept)')
    parser.add_argument('--heif-convert', default=os.environ.get('HEIF_CONVERT', 'heif-convert'))
    sys.exit(run(parser.parse_args()))
