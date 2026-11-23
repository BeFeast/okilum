//! Contextual Projects instructions, mounted in the Projects section header.
//! Available before any project exists; reading it writes no vault files.
use crate::contextual_help::Guide;
use crate::prelude::*;

const GUIDE: &str = include_str!("../../../docs/help/projects/setup.md");
const FILES: &[(&str, &[u8])] = &[
    (
        "assets/Manrope.ttf",
        include_bytes!("../../../docs/help/projects/assets/Manrope.ttf"),
    ),
    (
        "assets/OFL.txt",
        include_bytes!("../../../docs/help/projects/assets/OFL.txt"),
    ),
    (
        "index.html",
        include_bytes!("../../../docs/help/projects/index.html"),
    ),
    (
        "captions.vtt",
        include_bytes!("../../../docs/help/projects/captions.vtt"),
    ),
    (
        "assets/projects-tutorial.mp4",
        include_bytes!("../../../docs/help/projects/assets/projects-tutorial.mp4"),
    ),
    (
        "assets/projects-board.png",
        include_bytes!("../../../docs/help/projects/assets/projects-board.png"),
    ),
];

static PROJECTS: Guide = Guide {
    topic: "projects",
    title: "Projects help",
    entry_id: "projects-help",
    content_id: "projects-help-content",
    text_id: "projects-help-written-guide",
    copy_id: "projects-help-copy",
    video_id: "projects-help-video",
    markdown: GUIDE,
    files: FILES,
};

pub fn entry() -> Button {
    crate::contextual_help::entry(&PROJECTS)
}
