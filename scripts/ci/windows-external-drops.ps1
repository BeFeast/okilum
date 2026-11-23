# Native IDataObject/HGLOBAL controls plus the actual Okilum drop router.
$ErrorActionPreference = 'Stop'
python scripts/gpui-pre-windows-setup.py --verify
if ($LASTEXITCODE -ne 0) { throw 'Vendor verification failed' }
$output = cargo test --locked -p gpui-pre-windows --lib external_drop -- --test-threads=1 2>&1
$output | Write-Output
if ($LASTEXITCODE -ne 0) { throw 'Windows external drop tests failed' }
foreach ($test in @('external_drop_real_ole_object_empty_hdrop_and_bounded_reads', 'external_drop_selection_budget_and_single_image', 'external_drop_dib_validates_layout_and_header_budget')) {
    if (!(($output -join "`n") -match "$test ... ok")) { throw "Missing executed control: $test" }
}
$output = cargo test --locked -p okilum-shell --bin okilum windows_dropped_formats_route_through_the_paste_router 2>&1
$output | Write-Output
if ($LASTEXITCODE -ne 0 -or !(($output -join "`n") -match 'windows_dropped_formats_route_through_the_paste_router ... ok')) { throw 'Windows router regression did not pass' }
$output = cargo test --locked -p okilum-shell --bin okilum cf_html_fragment_uses_byte_offsets_and_rejects_invalid_ranges 2>&1
$output | Write-Output
if ($LASTEXITCODE -ne 0 -or !(($output -join "`n") -match 'cf_html_fragment_uses_byte_offsets_and_rejects_invalid_ranges ... ok')) { throw 'CF_HTML regression did not pass' }
python scripts/gpui-pre-windows-setup.py --verify
if ($LASTEXITCODE -ne 0) { throw 'Vendor changed during tests' }
