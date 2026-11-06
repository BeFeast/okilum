#!/usr/bin/env python3
"""Require compiled failures without Rules scrolling and similarity fixes (#1590)."""
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
CORE = 'agent_rules::tests::rules_1590_similar_lines_and_headings_keep_peer_coordinates_and_exact_flags'
SHELL = 'project::agent_rules::tests::rules_1590_mounted_layers_scroll_and_open_at_last_layer'


def run(package, name, *args):
    command = ['cargo', 'test', '--locked', '-p', package]
    if package == 'okilum-shell':
        command += ['--bin', 'okilum']
    result = subprocess.run(command + [name, '--', *args], cwd=ROOT,
                            capture_output=True, text=True, timeout=1200)
    print(result.stdout, end='', flush=True)
    print(result.stderr, end='', flush=True)
    return result


def main():
    core = ROOT / 'crates/okilum-core/src/agent_rules.rs'
    project = ROOT / 'crates/okilum-shell/src/project.rs'
    originals = {path: path.read_bytes() for path in (core, project)}
    try:
        text = originals[core].decode()
        assert text.count('    mark_similar(stack);\n') == 1
        core.write_bytes(text.replace('    mark_similar(stack);\n', '').encode())
        text = originals[project].decode()
        start = text.index('    fn render_resource_section(')
        end = text.index('            ProjectSection::Skills => {', start)
        section = text[start:end]
        assert section.count('v_flex()') == section.count('.size_full()') == 1
        text = text[:start] + section.replace('v_flex()', 'div()').replace('.size_full()', '') + text[end:]
        constraint = '                        view.flex().flex_col().min_w_0().overflow_hidden()'
        assert text.count(constraint) == 1
        project.write_bytes(text.replace(constraint, '                        view').encode())
        for package, name in [('okilum-core', CORE), ('okilum-shell', SHELL)]:
            inventory = run(package, name, '--exact', '--list')
            assert inventory.returncode == 0, 'negative control must compile'
            assert f'{name}: test' in inventory.stdout, 'negative control must be discovered'
            result = run(package, name, '--exact', '--nocapture', '--test-threads=1')
            assert result.returncode != 0, 'disabled Rules fix unexpectedly passed'
            assert re.search(r'test result: FAILED\. 0 passed; 1 failed;', result.stdout), (
                'expected one runtime assertion failure, not compiler/infra failure')
            print(f'RULES_NEGATIVE_PASS {name}', flush=True)
    finally:
        for path, original in originals.items():
            path.write_bytes(original)
            assert path.read_bytes() == original
    for package in ['okilum-core', 'okilum-shell']:
        assert run(package, 'rules_1590_', '--nocapture', '--test-threads=1').returncode == 0
    print('RULES_RESTORED_PASS', flush=True)


if __name__ == '__main__':
    main()
