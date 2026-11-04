#!/bin/bash
set -Eeuo pipefail
controls="${RUNNER_TEMP:?}/manager-input-controls"
mkdir -p "$controls"
git rev-parse HEAD | tee "$controls/source.txt"
# Use the hosted lane's exact native toolchain/cache/vendor/Sparkle setup and
# non-empty test runner, stopping before the unrelated lifecycle/full suites.
setup=$(mktemp)
python3 - "$setup" <<'PY'
from pathlib import Path
import sys
s=Path('scripts/ci/check-macos.sh').read_text()
Path(sys.argv[1]).write_text(s.split('# Kernel lifecycle failures')[0])
PY
source "$setup"
rm "$setup"
run_tests okilum-shell --bins issue2521_ 2>&1 | tee "$controls/positive.log"
run_tests okilum-shell --bins project::composer::tests::send_mode_disclosure_stays_with_send_and_enter_retains_each_explicit_mode --exact 2>&1 | tee "$controls/ime-positive.log"
[[ "$macos_rust_failed" = 0 ]]
python3 - <<'PY'
from pathlib import Path
p=Path('crates/okilum-shell/src/note_paste.rs')
s=p.read_text()
for label in ['Undo','Redo','Cut','Copy','Select All']:
    line=f'            gpui::MenuItem::action("{label}", gpui_component::input::'
    found=[l for l in s.splitlines(keepends=True) if l.startswith(line)]
    assert len(found)==1,(label,found)
    s=s.replace(found[0],'')
p.write_text(s)
p=Path('crates/okilum-shell/src/project/attachments.rs');s=p.read_text()
assert s.count('input.replace(text, window, cx)')==2
s=s.replace('input.replace(text, window, cx)','input.insert(text, window, cx)')
needle='if image::guess_format(&bytes).ok() == Some(image::ImageFormat::Tiff) {'
assert needle in s
s=s.replace(needle,'if false { // Negative control: old unsupported-TIFF policy.')
p.write_text(s)
p=Path('crates/okilum-shell/src/platform/exact_macos_clipboard.rs');s=p.read_text()
needle='offer::read_chat_offer(&MacBoard(board))'
assert s.count(needle)==1
p.write_text(s.replace(needle,'offer::read_offer(&MacBoard(board))'))
p=Path('crates/okilum-shell/src/project/composer.rs');s=p.read_text()
needle='                && !this.input.read(cx).ime_settling(cx)\n'
assert s.count(needle)==1
p.write_text(s.replace(needle,''))
PY
git diff > "$controls/reverted-behavior.diff"
# A compiler error cannot count as a successful negative control.
cargo "${cargo_args[@]}" --workspace --bins --no-run
set +e
cargo "${cargo_args[@]}" --workspace --bins issue2521_ -- --test-threads=1 > "$controls/negative.log" 2>&1
negative_status=$?
cargo "${cargo_args[@]}" --workspace --bins project::composer::tests::send_mode_disclosure_stays_with_send_and_enter_retains_each_explicit_mode -- --exact --test-threads=1 > "$controls/ime-negative.log" 2>&1
ime_status=$?
set -e
cat "$controls/negative.log" "$controls/ime-negative.log"
[[ "$negative_status" != 0 && "$ime_status" != 0 ]]
python3 - "$controls" <<'PY'
from pathlib import Path
import sys
p=Path(sys.argv[1]); log=(p/'negative.log').read_text()
for test in [
 'issue2521_native_text_paste_replaces_selection_and_undo_restores_it',
 'issue2521_edit_menu_and_command_shortcuts_follow_composer_focus',
 'issue2521_edit_menu_copy_uses_focused_feed_selection',
 'issue2521_chat_file_flavors_preserve_missing_paths_without_ui_filesystem_probes',
 'issue2521_chat_plain_cached_path_stays_text',
 'issue2521_tiff_attachment_converts_losslessly_with_preview_and_output_budgets',
 'issue2521_tiff_paste_upload_send_reaches_provider_with_lossless_png',
]:
    lines=[l for l in log.splitlines() if test+' ... FAILED' in l]
    assert len(lines)==1,(test,lines)
    print('NEGATIVE CONTROL CONFIRMED:',test)
ime=(p/'ime-negative.log').read_text()
assert 'send_mode_disclosure_stays_with_send_and_enter_retains_each_explicit_mode ... FAILED' in ime
assert 'marked-text Enter cannot send' in ime
assert 'signal: 6' not in log+ime
print('NEGATIVE CONTROL CONFIRMED: marked-text Enter guard')
PY
