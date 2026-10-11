"""Headless Wayland rig for native checks: sway (wlroots, like Hyprland), a
virtual keyboard, and fcitx5 with the requested input methods. Nothing reaches
a real screen. Used by check-ime-native.py and check-external-link-native.py.
Needs: sway, wtype, grim, fcitx5, gdbus, dbus-launch."""
import os, shutil, subprocess, time
from pathlib import Path


class Rig:
    """Headless sway + a virtual keyboard + fcitx5 with the input methods."""

    def __init__(self, root, width, input_methods=(), path_prefix=None):
        self.root = root
        self.run = Path(f'/tmp/okime-{os.getpid()}-{width}')
        shutil.rmtree(self.run, ignore_errors=True)
        self.run.mkdir(mode=0o700)
        cfg = root / 'home' / '.config'
        (cfg / 'fcitx5').mkdir(parents=True, exist_ok=True)
        (root / 'sway.conf').write_text(f'output HEADLESS-1 resolution {width}x768\ndefault_border none\n')
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
        env['WAYLAND_DISPLAY'] = socks[0]
        self.env = env
        # Headless sway has no keyboard; the input method's grab needs one with a keymap.
        self.procs.append(subprocess.Popen(['wtype', '-s', '3600000', '-k', 'F24'], env=env))
        time.sleep(.5)
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
        (wtype names: logo, ctrl, shift, alt). sway moves the seat's cursor."""
        swaymsg = lambda *a: subprocess.run(['swaymsg', 'seat', '-', 'cursor', *map(str, a)], env=self.env,
                                            capture_output=True, check=True)
        swaymsg('set', x, y)
        time.sleep(.3)
        holder = subprocess.Popen(['wtype', '-M', hold, '-s', '2500', '-m', hold], env=self.env) if hold else None
        time.sleep(.6 if hold else 0)
        swaymsg('press', 'button1')
        time.sleep(.08)
        swaymsg('release', 'button1')
        if holder:
            holder.wait(timeout=10)
        time.sleep(.6)

    def shot(self, path):
        time.sleep(.4)
        subprocess.run(['grim', str(path)], env=self.env, check=True)
        return path

    def close(self):
        for p in reversed(self.procs):
            p.terminate()
        if getattr(self, 'dbus_pid', None):
            subprocess.run(['kill', str(self.dbus_pid)])
        shutil.rmtree(self.run, ignore_errors=True)
