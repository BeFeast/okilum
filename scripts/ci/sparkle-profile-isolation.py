"""Exercise the production entrypoint with real Sparkle on hosted macOS.

An isolated Reader profile must not create a native updater for the shared
bundle identity. The ordinary profile is a positive control. Recompile the same
host with only the isolation guard removed; require its exact runtime failure.
No production bundle, preferences, signing identity or installation is used.
"""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import uuid

ROOT = Path(__file__).resolve().parents[2]
GUARD = '    if (getenv("OKILUM_STATE_DIR") != NULL) return;\n'
FAILURE = 'ISOLATED_PROFILE_STARTED_NATIVE_UPDATER'


def main():
    assert sys.platform == 'darwin'
    assert os.environ.get('GITHUB_SERVER_URL') == 'https://github.com'
    original = (ROOT / 'crates/okilum-shell/src/updater/bridge.m').read_bytes()
    source = original.decode()
    assert source.count(GUARD) == 1, 'The reviewed isolation guard changed'
    framework = ROOT / 'vendor/sparkle'
    evidence = Path(os.environ['RUNNER_TEMP']) / 'sparkle-freshness-evidence'
    evidence.mkdir(exist_ok=True)
    results = []
    with tempfile.TemporaryDirectory(prefix='okilum-profile-isolation-') as temporary:
        work = Path(temporary)
        for variant, bridge in [('fixed', source), ('unguarded', source.replace(GUARD, ''))]:
            copied_bridge = work / f'{variant}-bridge.m'
            copied_bridge.write_text(bridge)
            host = work / f'{variant}-host.m'
            host.write_text(f'''#import {json.dumps(str(copied_bridge))}
#include <stdio.h>
#include <string.h>
int main(int argc, const char *argv[]) {{
    @autoreleasepool {{
        if (argc != 2) return 85;
        BOOL isolated = strcmp(argv[1], "isolated") == 0;
        if (isolated != (getenv("OKILUM_STATE_DIR") != NULL)) return 85;
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        okilum_updater_start();
        if (isolated && okilum_updater_available()) {{
            fprintf(stderr, "{FAILURE}\\n");
            return 86;
        }}
        if (!isolated && !okilum_updater_available()) return 87;
        printf("%s\\n", isolated ? "ISOLATED_PROFILE_HAS_NO_NATIVE_UPDATER"
                                  : "DEFAULT_PROFILE_NATIVE_UPDATER_AVAILABLE");
    }}
    return 0;
}}
''')
            app = work / f'{variant}.app'
            binary = app / 'Contents/MacOS/ProfileIsolation'
            binary.parent.mkdir(parents=True)
            info = {
                'CFBundleIdentifier': 'uk.oklabs.okilum.profile-isolation.' + uuid.uuid4().hex,
                'CFBundleExecutable': binary.name, 'CFBundleName': 'ProfileIsolation',
                'CFBundlePackageType': 'APPL', 'CFBundleVersion': '1',
                'SUFeedURL': 'http://127.0.0.1:9/appcast.xml',
                'SUPublicEDKey': 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=',
                'SUEnableAutomaticChecks': True, 'SUAllowsAutomaticUpdates': False,
                'NSAppTransportSecurity': {'NSAllowsLocalNetworking': True},
            }
            (app / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
            subprocess.run([
                'xcrun', 'clang', '-fobjc-arc', '-fblocks', '-Wall', '-Werror',
                '-Wno-unused-parameter', '-F', str(framework), '-framework', 'Sparkle',
                '-framework', 'AppKit', '-Wl,-rpath,@executable_path/../Frameworks',
                str(host), '-o', str(binary),
            ], check=True, timeout=120)
            subprocess.run(['ditto', str(framework / 'Sparkle.framework'),
                            str(app / 'Contents/Frameworks/Sparkle.framework')], check=True)
            subprocess.run(['codesign', '--force', '--sign', '-', str(app)], check=True)
            for profile in ['default', 'isolated']:
                environment = dict(os.environ)
                environment.pop('DYLD_FRAMEWORK_PATH', None)
                environment.pop('OKILUM_STATE_DIR', None)
                if profile == 'isolated':
                    environment['OKILUM_STATE_DIR'] = str(work / 'reader-state')
                result = subprocess.run([str(binary), profile], env=environment,
                                        capture_output=True, text=True, timeout=30)
                output = result.stdout + result.stderr
                (evidence / f'profile-{variant}-{profile}.log').write_text(output)
                negative = variant == 'unguarded' and profile == 'isolated'
                expected = (FAILURE if negative else
                            'ISOLATED_PROFILE_HAS_NO_NATIVE_UPDATER' if profile == 'isolated'
                            else 'DEFAULT_PROFILE_NATIVE_UPDATER_AVAILABLE')
                assert result.returncode == (86 if negative else 0), output
                assert expected in output, output
                results.append({'variant': variant, 'profile': profile,
                                'exit': result.returncode, 'runtime_marker': expected})
    assert (ROOT / 'crates/okilum-shell/src/updater/bridge.m').read_bytes() == original
    (evidence / 'profile-isolation.json').write_text(json.dumps({
        'source': os.environ['OKILUM_SOURCE_COMMIT'],
        'bridge_sha256': hashlib.sha256(original).hexdigest(),
        'production_source_unchanged': True, 'results': results,
    }, indent=2) + '\n')
    print('PASS native profile isolation; unguarded entrypoint failed the exact runtime control')


if __name__ == '__main__':
    main()
