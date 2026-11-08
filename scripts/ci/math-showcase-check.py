#!/usr/bin/env python3
"""Compiled old-parser controls for #2741; failed setup never counts as a verdict."""
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def run(package, target, name, *flags):
    command = ['cargo', 'test', '--locked', '-p', package, *target, name,
               '--', '--exact', *flags]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=1200)
    print(result.stdout, end='', flush=True)
    print(result.stderr, end='', flush=True)
    return result


def main():
    path = ROOT / 'crates/okilum-core/src/tex.rs'
    original = path.read_bytes()
    for label, before, old, core, shell in [
        ('SYMBOLS', b'symbol(&name).or_else(|| symbols::symbol(&name))', b'symbol(&name)',
         'math_2741_showcase_symbols_are_native_glyphs_with_their_tex_classes',
         'source_presentation::tests::math_2741_showcase_inline_symbols_reveal_exact_source_and_preserve_unsupported_input'),
        ('TAGS', b'"tag" => {', b'"tag" if false => {',
         'math_2741_exact_screenshot_tags_belong_to_display_and_preserve_the_equation',
         'reader_obsidian::tests::math_2741_read_original_display_tags_paint_beside_the_equation'),
    ]:
        assert original.count(before) == 1
        cases = [('okilum-core', ['--test', 'tex_showcase'], core),
                 ('okilum-shell', ['--bin', 'okilum'], shell)]
        try:
            path.write_bytes(original.replace(before, old, 1))
            for package, target, name in cases:
                listed = run(package, target, name, '--list')
                assert listed.returncode == 0 and listed.stdout.count(': test\n') == 1
                failed = run(package, target, name, '--nocapture', '--test-threads=1')
                assert failed.returncode != 0 and 'test result: FAILED. 0 passed; 1 failed;' in failed.stdout
            print(f'MATH_2741_{label}_OLD_RUNTIME_FAILURES_PASS', flush=True)
        finally:
            path.write_bytes(original)
        for package, target, name in cases:
            assert run(package, target, name, '--nocapture', '--test-threads=1').returncode == 0
        print(f'MATH_2741_{label}_RESTORED_PASS', flush=True)


if __name__ == '__main__':
    main()
