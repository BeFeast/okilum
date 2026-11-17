"""Native viewer checks with a required, non-empty production-path inventory."""
import subprocess
import sys

GROUPS = {
    "audio": ("okilum-shell", None, "reader_audio::tests", [
        "mp3_and_wav_decode_non_silence_and_seek_without_an_output_device",
        "real_sink_controls_pause_seek_volume_end_and_replay",
    ]),
    "media": ("okilum-shell", None, "reader_media::tests", [
        "media_file_urls_share_the_canonical_vault_identity_and_reject_outside_files",
        "video_extensions_do_not_capture_audio_or_images",
        "video_card_only_opens_its_checked_file_after_an_explicit_click",
        "showcase_images_use_the_shipping_decoder_and_apply_exif",
    ]),
    "reader": ("okilum-shell", None, "reader_viewer_tests", [
        "local_viewers_dispatch_inline_and_as_files_and_canvas_navigates_without_writes",
        "cold_start_canvas_opens_its_exact_note_before_another_document",
    ]),
    "canvas": ("okilum-core", "--lib", "canvas::tests", [
        "json_canvas_sample_preserves_card_types_edges_and_negative_geometry",
        "malformed_duplicate_and_unbounded_scenes_fail_but_missing_edges_stay_visible",
        "fit_contains_the_scene_and_zoom_keeps_the_pointer_anchor",
    ]),
    "showcase": ("okilum-core", "viewer_showcase", "", [
        "showcase_virtual_log_selects_both_sources_without_mutating_them",
        "showcase_jpeg_and_bmp_embeds_resolve_to_real_files",
    ]),
    # HEIC/HEIF platform decoders (#2177): macOS sips/ImageIO decodes real
    # pixels; the hosted Windows runner has no Store HEIF/HEVC extensions, so
    # the workflow pins the named missing-codec fallback.
    "heic": ("okilum-shell", None, "heic", [
        "issue2177_non_heif_bytes_never_reach_a_decoder",
        "issue2177_missing_decoder_messages_name_the_component",
        "issue2984_oversized_source_is_rejected_before_decoding",
        "issue2984_heic_and_heif_open_the_image_viewer_and_embed_decoder",
    ] + ([
        "issue2177_windows_wic_decodes_or_names_the_missing_extensions",
        "issue2177_windows_job_child_failure_and_timeout_are_errors",
    ] if sys.platform == "win32" else [
        "issue2984_real_decoder_reads_hevc_without_changing_the_source",
        "issue2984_decoder_failure_and_timeout_are_errors",
    ] + (["issue2984_native_heic_preview_reports_errors_and_refreshes_assets"]
         if sys.platform.startswith("linux") else []) + ([
        "issue2984_multi_image_container_uses_first_still",
        "issue2984_bundled_decoder_wins_over_the_host",
    ] if sys.platform.startswith("linux") else []))),
}

# Known failures excluded from a group on one platform, each with its issue.
SKIPS = {
    # #3064: fails identically on unchanged main (viewers-native run 44037169749):
    # after the file is overwritten, no image/fallback is drawn on macOS.
    ("heic", "darwin"): ["issue2984_native_heic_preview_reports_errors_and_refreshes_assets"],
}


def check(group):
    package, target, prefix, expected = GROUPS[group]
    command = ["cargo", "test", "--locked", "-p", package]
    if target == "--lib":
        command.append(target)
    elif target:
        command += ["--test", target]
    if prefix:
        command.append(prefix)
    skips = [arg for name in SKIPS.get((group, sys.platform), [])
             for arg in ("--skip", name)]
    listed = subprocess.run(command + ["--", "--list"] + skips, check=True,
                            stdout=subprocess.PIPE, text=True).stdout
    print(listed, flush=True)
    inventory = {line.removesuffix(": test").rsplit("::", 1)[-1]
                 for line in listed.splitlines() if line.endswith(": test")}
    if inventory != set(expected):
        raise RuntimeError(f"{group}: expected {expected}, discovered {sorted(inventory)}")
    subprocess.run(command + ["--", "--test-threads=1"] + skips, check=True)


if __name__ == "__main__":
    check(sys.argv[1])
