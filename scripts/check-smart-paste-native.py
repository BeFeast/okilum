#!/usr/bin/env python3
"""Native #1618: Linux smart paste against the clipboard offers real apps make,
through Wayland the way Hyprland runs it (wlroots data-control). Runs its own
headless sway, so nothing reaches a real screen; never on maestro.
Needs: sway, wtype, grim, dbus-launch, gcc, wayland-scanner, pkg-config with
wayland-client, ImageMagick, and wlr-data-control-unstable-v1.xml (found in the
Cargo registry's wayland-protocols-wlr, or set WLR_DATA_CONTROL_XML).
Usage: python3 scripts/check-smart-paste-native.py OKILUM_BINARY WORK_DIR
Each run writes into a new smart-paste-* directory inside WORK_DIR and never
deletes anything it did not create.

scripts/wl_multi_copy.c owns the clipboard with several MIME types at once,
each from a file, and logs every transfer the app asks for. Each case offers
the MIME set of one source app, presses Ctrl+V / Ctrl+Shift+V / Ctrl+Alt+V in
source editing, saves, and checks the saved note bytes, the files added to the
vault, the one-line paste log (types, route, bytes, ms) and which flavors were
transferred (only the routed ones):
- Firefox (UTF-16 HTML with BOM, BOM-less text/x-moz-url-priv): heading,
  table, bold, a relative link resolved against the page, a relative image
  downloaded into the vault; no pixels or Mozilla context flavors read.
- Chromium (<meta charset> prefix, chromium/x-source-url): rich text, link.
- Nautilus copy (gnome-copied-files + uri-list + a picture): uri-list alone is
  read, both files are copied into attachments.
- gnome-copied-files only, `cut`: pasted as a copy, the source stays.
- Dolphin cut (uri-list + application/x-kde-cutselection): a copy.
- A screenshot tool (image/png only): an image attachment.
- VS Code (vscode-editor-data + HTML): a rust fence, the HTML never read.
- Tab-separated text: a table. CRLF text: LF only.
- Ctrl+Shift+V on the Firefox offer: the plain text, nothing else read.
- Ctrl+Alt+V on a URL: a titled link.
- Ctrl+V on the Firefox offer inside a code fence: the plain text, literal.
Pages, images and titles come from a loopback HTTP server.
Positive control: typed text reaches the saved note.
Exit 0: all hold; 1: a check failed; 2: the run proves nothing.
"""
import functools, glob, http.server, json, os, shutil, subprocess, sys, tempfile, threading, time
from pathlib import Path

binary, parent = sys.argv[1], Path(sys.argv[2])
SCRIPTS = Path(__file__).resolve().parent


def build_copier(out):
    xml = os.environ.get('WLR_DATA_CONTROL_XML') or next(iter(sorted(glob.glob(os.path.expanduser(
        '~/.cargo/registry/src/*/wayland-protocols-wlr-*/wlr-protocols/unstable/wlr-data-control-unstable-v1.xml')))), None)
    if not xml:
        return None
    out.mkdir(parents=True)
    subprocess.run(['wayland-scanner', 'client-header', xml, str(out / 'wlr-data-control-unstable-v1-client-protocol.h')], check=True)
    subprocess.run(['wayland-scanner', 'private-code', xml, str(out / 'wlr-data-control-unstable-v1-protocol.c')], check=True)
    flags = subprocess.run(['pkg-config', '--cflags', '--libs', 'wayland-client'], capture_output=True, text=True, check=True).stdout.split()
    subprocess.run(['gcc', '-O1', '-Wall', '-I', str(out), '-o', str(out / 'wl_multi_copy'), str(SCRIPTS / 'wl_multi_copy.c'),
                    str(out / 'wlr-data-control-unstable-v1-protocol.c'), *flags], check=True)
    return out / 'wl_multi_copy'


class Rig:
    """Headless sway and a virtual keyboard, nothing else (no input method)."""

    def __init__(self, root):
        self.run = Path(tempfile.mkdtemp(prefix='okpaste-'))  # 0700, owned by this run
        home = root / 'home'
        (home / '.config' / 'okilum').mkdir(parents=True)
        (home / '.config' / 'okilum' / 'appearance.json').write_text('{"appearance":"light"}')
        (root / 'sway.conf').write_text('output HEADLESS-1 resolution 1366x900\ndefault_border none\n')
        env = {k: v for k, v in os.environ.items() if k not in ('DISPLAY', 'WAYLAND_DISPLAY')}
        env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'), XDG_RUNTIME_DIR=str(self.run),
                   XDG_DATA_HOME=str(home / 'share'), XDG_CACHE_HOME=str(home / 'cache'),
                   XDG_STATE_HOME=str(home / 'state'), WLR_BACKENDS='headless', WLR_RENDERER='pixman',
                   WLR_LIBINPUT_NO_DEVICES='1', LANG='C.UTF-8', OKILUM_STATE_DIR=str(root / 'state'))
        self.dbus_pid = None
        for line in subprocess.run(['dbus-launch', '--sh-syntax'], env=env, capture_output=True, text=True).stdout.splitlines():
            if line.startswith('DBUS_SESSION_BUS_ADDRESS='):
                env['DBUS_SESSION_BUS_ADDRESS'] = line.split('=', 1)[1].strip("';")
            if line.startswith('DBUS_SESSION_BUS_PID='):
                self.dbus_pid = int(line.split('=', 1)[1].strip("';"))
        self.procs = [subprocess.Popen(['sway', '-c', str(root / 'sway.conf')], env=env,
                                       stdout=open(root / 'sway.log', 'w'), stderr=subprocess.STDOUT)]
        socks = []
        for _ in range(40):
            time.sleep(.25)
            socks = [p.name for p in self.run.iterdir() if p.name.startswith('wayland-') and not p.name.endswith('.lock')]
            if socks:
                break
        if not socks:
            raise RuntimeError('sway did not start')
        env['WAYLAND_DISPLAY'] = socks[0]
        self.env = env
        # Headless sway has no keyboard; keep one virtual keyboard with a keymap.
        self.procs.append(subprocess.Popen(['wtype', '-s', '3600000', '-k', 'F24'], env=env))
        time.sleep(.5)

    def keys(self, *names, mods=()):
        args = []
        for m in mods:
            args += ['-M', m]
        for n in names:
            args += ['-k', n]
        for m in reversed(mods):
            args += ['-m', m]
        subprocess.run(['wtype', *args], env=self.env, check=True)
        time.sleep(.25)

    def shot(self, path):
        time.sleep(.4)
        subprocess.run(['grim', str(path)], env=self.env, check=True)

    def close(self):
        for p in reversed(self.procs):
            p.terminate()
        if self.dbus_pid:
            subprocess.run(['kill', str(self.dbus_pid)])
        shutil.rmtree(self.run, ignore_errors=True)


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def serve(root):
    handler = functools.partial(QuietHandler, directory=str(root))
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def fixtures(fx, src, site, base):
    fx.mkdir(parents=True); src.mkdir(); (site / 'docs').mkdir(parents=True)
    subprocess.run(['convert', '-size', '64x48', 'gradient:red-blue', str(src / 'photo.png')], check=True)
    subprocess.run(['convert', '-size', '40x30', 'xc:green', str(fx / 'shot.png')], check=True)
    subprocess.run(['convert', '-size', '32x32', 'xc:orange', str(site / 'docs' / 'logo.png')], check=True)
    (site / 'title.html').write_text('<html><head><title>Okilum title page</title></head><body>t</body></html>')
    (src / 'notes.txt').write_text('plain file from the file manager\n')
    html = ('<h2>Firefox heading</h2><p>See <a href="../reports/">the reports</a> and <b>bold</b> text.</p>'
            '<table><tr><th>Col A</th><th>Col B</th></tr><tr><td>1</td><td>2</td></tr></table>'
            '<p><img src="logo.png" alt="logo"></p>')
    files = {
        'ff.html': b'\xff\xfe' + html.encode('utf-16-le'),  # Firefox: UTF-16LE with a BOM
        'ff.url': f'{base}/docs/page.html'.encode('utf-16-le'),  # x-moz-url-priv: no BOM
        'ff.txt': FF_PLAIN.encode(),
        'ff.ctx': '<html><body></body></html>'.encode('utf-16-le'),
        'ff.info': '0,0'.encode('utf-16-le'),
        'cr.html': ("<meta charset='utf-8'><h3>Chromium café</h3><p>Relative <a href=\"/reports/\">link</a> “quoted”</p>"
                    '<ul><li>one</li><li>two</li></ul>').encode(),
        'cr.url': f'{base}/docs/other.html'.encode(),
        'cr.txt': 'Chromium café\nRelative link “quoted”\none\ntwo\n'.encode(),
        'gn-copy': f'copy\n{(src / "photo.png").as_uri()}\n{(src / "notes.txt").as_uri()}'.encode(),
        'gn-cut': f'cut\n{(src / "notes.txt").as_uri()}'.encode(),
        'uris': f'# comment line\r\n{(src / "photo.png").as_uri()}\r\n{(src / "notes.txt").as_uri()}\r\n'.encode(),
        'paths': f'{src / "photo.png"}\n{src / "notes.txt"}'.encode(),
        'kde-cut': b'1',
        'cut-uri': f'{(src / "notes.txt").as_uri()}\r\n'.encode(),
        'vs.txt': VS_TEXT.encode(),
        'vs.json': json.dumps({'version': 1, 'isFromEmptySelection': False, 'multicursorText': None, 'mode': 'rust'}).encode(),
        'vs.html': b'<meta charset="utf-8"><div><div><span>fn main() {</span></div></div>',
        'tsv': b'Name\tQty\nApple\t3\nPear\t5\n',
        'crlf': b'line one\r\nline two\r\n',
        'url': f'{base}/title.html'.encode(),
    }
    for name, data in files.items():
        (fx / name).write_bytes(data)


FF_PLAIN = 'Firefox heading\nSee the reports and bold text.\nCol A\tCol B\n1\t2\n'
VS_TEXT = 'fn main() {\n    println!("hi");\n}\n'
PLAIN = 'text/plain;charset=utf-8'
FIREFOX = ['text/html=ff.html', 'text/_moz_htmlcontext=ff.ctx', 'text/_moz_htmlinfo=ff.info',
           'text/x-moz-url-priv=ff.url', f'{PLAIN}=ff.txt', 'UTF8_STRING=ff.txt', 'TEXT=ff.txt',
           'STRING=ff.txt', 'text/plain=ff.txt']
NOTE = '# Paste 1618\n\n```text\nINSIDE\n```\n\nEND\n'


def cases(base):
    """(name, offer, modifiers, where, checks). Checks get the case result; each
    returns a list of failure strings."""
    def has(*parts):
        return lambda r: [f'missing {p!r}' for p in parts if p not in r['inserted']]

    def lacks(*parts):
        return lambda r: [f'unexpected {p!r}' for p in parts if p in r['inserted']]

    def read(*mimes):
        return lambda r: [] if r['read'] == list(mimes) else [f'read {r["read"]} != {list(mimes)}']

    def route(name, mode='Smart'):
        return lambda r: [] if f'mode={mode} route={name} ' in r['log'] else [f'not mode={mode} route={name}']

    def new(*dirs):
        return lambda r: ([] if len(r['new_files']) == len(dirs) and all(f.startswith(d) for f, d in zip(r['new_files'], dirs))
                          else [f'new files {r["new_files"]} != {list(dirs)}'])

    def follow(text):
        return lambda r: [] if text in r['follow'] else [f'no follow-up {text!r}']

    return [
        ('firefox-into-fence', FIREFOX, ('ctrl',), 'fence',
         [route('CodeBlockText'), read(PLAIN), new(),
          lambda r: [] if r['note'].startswith('# Paste 1618\n\n```text\nINSIDE\n' + FF_PLAIN) else ['fence text not literal']]),
        ('firefox-rich', FIREFOX, ('ctrl',), 'end',
         [route('RichText'), read(PLAIN, 'text/html', 'text/x-moz-url-priv'), new('@Assets/Images/'),
          has('## Firefox heading', f'[the reports]({base}/reports/)', '**bold**', '| Col A | Col B |', '![['),
          follow('images downloaded=1 failed=0')]),
        ('chromium-rich', ['text/html=cr.html', 'chromium/x-source-url=cr.url', f'{PLAIN}=cr.txt',
                           'UTF8_STRING=cr.txt', 'text/plain=cr.txt'], ('ctrl',), 'end',
         [route('RichText'), read(PLAIN, 'text/html', 'chromium/x-source-url'), new(),
          has('### Chromium café', f'[link]({base}/reports/)', '“quoted”', '- one'), lacks('<meta', 'charset')]),
        ('nautilus-copy', ['x-special/gnome-copied-files=gn-copy', 'text/uri-list=uris', f'{PLAIN}=paths',
                           'UTF8_STRING=paths', 'image/png=shot.png'], ('ctrl',), 'end',
         [route('Files'), read('text/uri-list'), new('@Assets/Files/notes', '@Assets/Images/photo'),
          has('[[notes.txt]]', 'photo'), follow('copied 2 of 2 files')]),
        ('gnome-cut-only', ['x-special/gnome-copied-files=gn-cut', f'{PLAIN}=paths'], ('ctrl',), 'end',
         [route('Files'), read('x-special/gnome-copied-files'), new('@Assets/Files/notes'), follow('copied 1 of 1 files')]),
        ('dolphin-cut', ['text/uri-list=cut-uri', 'application/x-kde-cutselection=kde-cut', f'{PLAIN}=paths'], ('ctrl',), 'end',
         [route('Files'), read('text/uri-list'), new('@Assets/Files/notes'), follow('copied 1 of 1 files')]),
        ('screenshot-png', ['image/png=shot.png'], ('ctrl',), 'end',
         [route('Image'), read('image/png'), new('@Assets/Images/'), has('![[@Assets/Images/')]),
        ('vscode', [f'{PLAIN}=vs.txt', 'UTF8_STRING=vs.txt', 'text/plain=vs.txt', 'vscode-editor-data=vs.json',
                    'text/html=vs.html'], ('ctrl',), 'end',
         [route('IdeCode'), read(PLAIN, 'vscode-editor-data'), new(), has('```rust\n' + VS_TEXT + '```')]),
        ('tsv-table', [f'{PLAIN}=tsv', 'UTF8_STRING=tsv'], ('ctrl',), 'end',
         [route('Table'), read(PLAIN), has('| Name  | Qty |', '| Apple |   3 |')]),
        ('crlf-text', [f'{PLAIN}=crlf', 'UTF8_STRING=crlf', 'text/plain=crlf'], ('ctrl',), 'end',
         [route('Text'), read(PLAIN), has('line one\nline two\n'), lambda r: ['CR saved'] if '\r' in r['note'] else []]),
        ('plain-ctrl-shift-v', FIREFOX, ('ctrl', 'shift'), 'end',
         [route('PlainText', 'Plain'), read(PLAIN), new(), has(FF_PLAIN), lacks('**', '##')]),
        ('titled-ctrl-alt-v', [f'{PLAIN}=url', 'UTF8_STRING=url'], ('ctrl', 'alt'), 'end',
         [route('TitledLink', 'TitledLink'), read(PLAIN), has(f'[Okilum title page]({base}/title.html)')]),
    ]


def main():
    global work
    parent.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix='smart-paste-', dir=parent)).resolve()
    print(f'work directory: {work}')
    copier = build_copier(work / 'copier')
    if not copier:
        print('no wlr-data-control-unstable-v1.xml (set WLR_DATA_CONTROL_XML)')
        return 2
    fx, src, vault = work / 'fx', work / 'files', work / 'vault'
    server = serve(work / 'site')
    base = f'http://127.0.0.1:{server.server_address[1]}'
    fixtures(fx, src, work / 'site', base)
    vault.mkdir()
    note = vault / 'paste.md'
    note.write_text(NOTE)
    tree = lambda: sorted(str(p.relative_to(vault)) for p in vault.rglob('*') if p.is_file() and not p.name.startswith('.'))
    rig = Rig(work)
    app_log = work / 'app.log'
    app = None
    results, failed = [], []
    try:
        app = subprocess.Popen([binary, '--vault', str(vault), '--index-dir', str(work / 'index'), '--note', 'paste.md'],
                               env=rig.env, stdout=open(app_log, 'w'), stderr=subprocess.STDOUT)
        time.sleep(9)
        rig.keys('e', mods=('ctrl',))  # source editing
        time.sleep(1.5)
        rig.shot(work / '00-open.png')

        def save():
            time.sleep(.8)
            rig.keys('s', mods=('ctrl',))
            time.sleep(1.5)
            with note.open('r', encoding='utf-8', newline='') as f:
                return f.read()

        rig.keys('End', mods=('ctrl',))
        subprocess.run(['wtype', 'typed-control'], env=rig.env, check=True)
        if 'END\ntyped-control' not in save():
            print('positive control: typed text did not reach the saved note')
            return 2
        for i, (name, offer, mods, where, checks) in enumerate(cases(base), 1):
            if app.poll() is not None:
                print(f'the app exited ({app.returncode}) before {name}')
                return 2
            files_before = tree()
            log_mark = len(app_log.read_text(errors='replace').splitlines())
            transfers = work / f'copy-{name}.log'
            owner = subprocess.Popen([str(copier), *offer], cwd=fx, env=rig.env,
                                     stdout=subprocess.DEVNULL, stderr=open(transfers, 'w'))
            time.sleep(1)
            if where == 'fence':
                rig.keys('Home', mods=('ctrl',))
                for _ in range(3):
                    rig.keys('Down')
                rig.keys('End')
                rig.keys('Return')
            else:
                # A fresh paragraph: Return twice leaves a list, then a marker line.
                rig.keys('End', mods=('ctrl',))
                rig.keys('Return'); rig.keys('Return')
                subprocess.run(['wtype', f'case-{name}'], env=rig.env, check=True)
                rig.keys('Return'); rig.keys('Return')
            rig.keys('v', mods=mods)
            time.sleep(5)
            after = save()
            rig.shot(work / f'{i:02d}-{name}.png')
            owner.terminate()
            lines = app_log.read_text(errors='replace').splitlines()[log_mark:]
            paste = [l for l in lines if 'Paste: mode=' in l]
            sent = [l.split()[1] for l in transfers.read_text().splitlines() if l.startswith('send ')]
            r = dict(case=name, keys='+'.join(mods) + '+v', offered=[o.rsplit('=', 1)[0] for o in offer], read=sent,
                     inserted=after.split(f'case-{name}\n', 1)[-1] if where == 'end' else after,
                     note=after, new_files=sorted(set(tree()) - set(files_before)),
                     log=paste[0] if paste else '', follow='\n'.join(l for l in lines if 'Paste:' in l))
            fails = [] if len(paste) == 1 else [f'{len(paste)} paste log lines']
            if paste and not all(k in paste[0] for k in ('types=', 'route=', 'bytes=', 'ms=', 'offered=', 'read=')):
                fails.append('log line lacks types/route/bytes/ms')
            for check in checks:
                fails += check(r)
            r['failed'] = fails
            failed += [f'{name}: {f}' for f in fails]
            results.append(r)
        if not ((src / 'photo.png').exists() and (src / 'notes.txt').exists()):
            failed.append('a copied or cut source file was removed')
    finally:
        if app:
            app.terminate()
            try:
                app.wait(timeout=20)
            except subprocess.TimeoutExpired:
                app.kill()
        rig.close()
        server.shutdown()
        (work / 'results.json').write_text(json.dumps(results, indent=1, ensure_ascii=False))
    for r in results:
        print(('FAIL ' if r['failed'] else 'ok   ') + f"{r['case']:<20} {r['keys']:<13} read={r['read']}")
        for f in r['failed']:
            print(f'     {f}')
    print(f'{len(results) - len([r for r in results if r["failed"]])}/{len(results)} cases hold; details in {work / "results.json"}')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
