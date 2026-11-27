//! Contextual Board instructions; available before and after Board creation.
use crate::contextual_help::Guide;
use crate::prelude::*;

const GUIDE: &str = include_str!("../../../../../docs/help/board/setup.md");
const FILES: &[(&str, &[u8])] = &[
    (
        "assets/Manrope.ttf",
        include_bytes!("../../../../../docs/help/board/assets/Manrope.ttf"),
    ),
    (
        "assets/OFL.txt",
        include_bytes!("../../../../../docs/help/board/assets/OFL.txt"),
    ),
    (
        "index.html",
        include_bytes!("../../../../../docs/help/board/index.html"),
    ),
    (
        "captions.vtt",
        include_bytes!("../../../../../docs/help/board/captions.vtt"),
    ),
    (
        "assets/board-tutorial.mp4",
        include_bytes!("../../../../../docs/help/board/assets/board-tutorial.mp4"),
    ),
    (
        "assets/board-saved.png",
        include_bytes!("../../../../../docs/help/board/assets/board-saved.png"),
    ),
];

static BOARD: Guide = Guide {
    topic: "board",
    title: "Board help",
    entry_id: "board-help",
    content_id: "board-help-content",
    text_id: "board-help-written-guide",
    copy_id: "board-help-copy",
    video_id: "board-help-video",
    markdown: GUIDE,
    files: FILES,
};

pub(super) fn entry() -> Button {
    crate::contextual_help::entry(&BOARD)
}
