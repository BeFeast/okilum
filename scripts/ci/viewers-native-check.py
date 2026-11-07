"""Native viewer checks with a required, non-empty production-path inventory."""
import subprocess
import sys

GROUPS = {
    "audio": ("okilum-shell", None, "reader_audio::tests", [
        "mp3_and_wav_decode_non_silence_and_seek_without_an_output_device",
        "real_sink_controls_pause_seek_volume_end_and_replay",
    ]),
    "media": ("okilum-shell", None, "reader_media::tests", [
        "video_extensions_do_not_capture_audio_or_images",
        "video_card_only_opens_its_checked_file_after_an_explicit_click",
        "showcase_images_use_the_shipping_decoder_and_apply_exif",
    ]),
    "reader": ("okilum-shell", None, "reader_viewer_tests", [
        "local_viewers_dispatch_inline_and_as_files_and_canvas_navigates_without_writes",
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
    listed = subprocess.run(command + ["--", "--list"], check=True,
                            stdout=subprocess.PIPE, text=True).stdout
    print(listed, flush=True)
    inventory = {line.removesuffix(": test").rsplit("::", 1)[-1]
                 for line in listed.splitlines() if line.endswith(": test")}
    if inventory != set(expected):
        raise RuntimeError(f"{group}: expected {expected}, discovered {sorted(inventory)}")
    subprocess.run(command + ["--", "--test-threads=1"], check=True)


if __name__ == "__main__":
    check(sys.argv[1])
