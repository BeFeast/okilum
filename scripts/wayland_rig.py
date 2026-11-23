"""Headless Wayland rig for native checks: sway (wlroots, like Hyprland), a
virtual keyboard, and fcitx5 with the requested input methods. Nothing reaches
a real screen. Used by check-ime-native.py and check-external-link-native.py.
Needs: sway, wtype, wlrctl, grim, fcitx5, gdbus, dbus-launch."""
import json, os, shutil, subprocess, tempfile, time
from pathlib import Path


class Rig:
    """Headless sway + a virtual keyboard + fcitx5 with the input methods."""

    def __init__(self, root, width, input_methods=(), path_prefix=None, output_scale=None, floating=False):
        """output_scale: a (fractional) output scale such as 1.3; floating: every
        window floats with client-side decorations (sway's `border csd`, as on
        GNOME, which has no server decorations), so sway follows the client's
        window geometry instead of tiling the window to the output. A
        [width, height] value also sets that logical window size once, smaller
        than the output, so sway never clamps it."""
        self.root = root
        self.run = Path(tempfile.mkdtemp(prefix='okime-'))
        cfg = root / 'home' / '.config'
        (cfg / 'fcitx5').mkdir(parents=True, exist_ok=True)
        conf = f'output HEADLESS-1 resolution {width}x768\ndefault_border none\n'
        if output_scale is not None:
            conf += f'output HEADLESS-1 scale {float(output_scale)}\n'
        if floating:
            size = f', resize set {int(floating[0])} {int(floating[1])}' if isinstance(floating, (list, tuple)) else ''
            conf += f'for_window [app_id=".*"] floating enable, border csd{size}\n'
        (root / 'sway.conf').write_text(conf)
        env = {k: v for k, v in os.environ.items() if k not in ('DISPLAY', 'WAYLAND_DISPLAY')}
        env.update(HOME=str(root / 'home'), XDG_CONFIG_HOME=str(cfg), XDG_RUNTIME_DIR=str(self.run),
                   XDG_DATA_HOME=str(root / 'home' / 'share'), XDG_CACHE_HOME=str(root / 'home' / 'cache'),
                   XDG_STATE_HOME=str(root / 'home' / 'state'), WLR_BACKENDS='headless',
                   WLR_RENDERER='pixman', WLR_LIBINPUT_NO_DEVICES='1', LANG='C.UTF-8')
        if path_prefix:
            env['PATH'] = f'{path_prefix}:{env["PATH"]}'
        for line in subprocess.run(['dbus-launch', '--sh-syntax'], env=env, capture_output=True,
                                   text=True).stdout.splitlines():
            if line.startswith('DBUS_SESSION_BUS_ADDRESS='):
                env['DBUS_SESSION_BUS_ADDRESS'] = line.split('=', 1)[1].strip("';")
            if line.startswith('DBUS_SESSION_BUS_PID='):
                self.dbus_pid = int(line.split('=', 1)[1].strip("';"))
        self.procs = [subprocess.Popen(['sway', '-c', str(root / 'sway.conf')], env=env,
                                       stdout=open(root / 'sway.log', 'w'), stderr=subprocess.STDOUT)]
        for _ in range(40):
            time.sleep(.25)
            socks = [p.name for p in self.run.iterdir() if p.name.startswith('wayland-') and not p.name.endswith('.lock')]
            if socks:
                break
        if not socks:
            self.close()
            raise RuntimeError(f'Headless sway did not start; see {root / "sway.log"}')
        env['WAYLAND_DISPLAY'] = socks[0]
        # swaymsg finds sway only through SWAYSOCK.
        for _ in range(20):
            ipc = [p for p in self.run.iterdir() if p.name.startswith('sway-ipc.')]
            if ipc:
                env['SWAYSOCK'] = str(ipc[0])
                break
            time.sleep(.25)
        self.env = env
        # Headless sway has no keyboard; the input method's grab needs one with a keymap.
        self.procs.append(subprocess.Popen(['wtype', '-s', '3600000', '-k', 'F24'], env=env))
        time.sleep(.5)
        # Acceptance scenarios without IME need only the virtual keyboard.
        if input_methods is None:
            return
        self.procs.append(subprocess.Popen(['fcitx5', '--disable', 'notificationitem', '-r'], env=env,
                                           stdout=open(root / 'fcitx5.log', 'w'), stderr=subprocess.STDOUT))
        time.sleep(4)
        items = ', '.join(f"('{im}', '')" for im in ['keyboard-us', *input_methods])
        subprocess.run(['gdbus', 'call', '--session', '--dest', 'org.fcitx.Fcitx5', '--object-path', '/controller',
                        '--method', 'org.fcitx.Fcitx.Controller1.SetInputMethodGroupInfo', 'Default', 'us',
                        f'[{items}]'], env=env, check=True, capture_output=True)

    def keys(self, *names, mods=()):
        args = []
        for m in mods:
            args += ['-M', m]
        for n in names:
            args += ['-k', n]
        for m in reversed(mods):
            args += ['-m', m]
        subprocess.run(['wtype', *args], env=self.env, check=True)
        time.sleep(.2)

    def text(self, word):
        for ch in word:
            subprocess.run(['wtype', ch], env=self.env, check=True)
            time.sleep(.12)
        time.sleep(.8)

    def im(self, name):
        for _ in range(10):
            if subprocess.run(['fcitx5-remote', '-s', name], env=self.env).returncode == 0:
                break
            time.sleep(1)
        time.sleep(.4)
        return subprocess.run(['fcitx5-remote', '-n'], env=self.env, capture_output=True, text=True).stdout.strip()

    def click(self, x, y, hold=None):
        """A left click at output pixel (x, y), optionally with a modifier held
        (wtype names: logo, ctrl, shift, alt). The headless seat has no pointer
        device, so wlrctl's virtual pointer moves (relative: to the corner first)."""
        wlr = lambda *a: subprocess.run(['wlrctl', 'pointer', *map(str, a)], env=self.env, check=True)
        wlr('move', -10000, -10000)
        wlr('move', x, y)
        time.sleep(.3)
        holder = subprocess.Popen(['wtype', '-M', hold, '-s', '2500', '-m', hold], env=self.env) if hold else None
        time.sleep(.6 if hold else 0)
        wlr('click', 'left')
        if holder:
            holder.wait(timeout=10)
        time.sleep(.6)

    def window(self):
        """The focused (else first) application window as sway sees it, in
        logical output coordinates: `rect` (placement), `window_rect` and
        `geometry` (the client's xdg window geometry as sway applied it)."""
        tree = json.loads(subprocess.run(['swaymsg', '-t', 'get_tree'], env=self.env, check=True,
                                         capture_output=True, text=True).stdout)
        views, stack = [], [tree]
        while stack:
            node = stack.pop()
            if node.get('pid') and node.get('type') in ('con', 'floating_con'):
                views.append(node)
            stack += node.get('nodes', []) + node.get('floating_nodes', [])
        if not views:
            return None
        view = next((v for v in views if v.get('focused')), views[0])
        return {k: view.get(k) for k in ('app_id', 'type', 'rect', 'window_rect', 'geometry', 'deco_rect')}

    def shot(self, path):
        time.sleep(.4)
        subprocess.run(['grim', str(path)], env=self.env, check=True)
        return path

    def close(self):
        for p in reversed(self.procs):
            if p.poll() is None:
                p.terminate()
                try:
                    p.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    p.kill()
                    p.wait()
        if getattr(self, 'dbus_pid', None):
            subprocess.run(['kill', str(self.dbus_pid)])
        shutil.rmtree(self.run, ignore_errors=True)
