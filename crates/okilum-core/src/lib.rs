//! okilum-core — the half of Okilum that has nothing to do with a window.
//!
//! Vault scan, wikilink resolution and backlinks, Markdown rendering (HTML and
//! a block IR), and search. The desktop shell uses it in process; `okilum-cored`
//! exposes the same thing over a versioned protocol, so a second client never
//! needs a private API.

pub mod analyzer;
pub mod archive;
pub mod callout;
#[cfg(all(unix, feature = "brain"))]
pub mod decision_reuse;
pub mod deep_link;
pub mod delimited;
pub mod document_links;
pub mod excalidraw;
#[cfg(all(unix, feature = "brain"))]
pub mod export;
pub mod facets;
#[cfg(any(unix, windows))]
pub mod file_editor;
#[cfg(all(unix, feature = "brain"))]
pub mod goal_criteria;
pub mod ir;
pub mod link_candidates;
pub mod link_registration;
#[cfg(any(unix, windows))]
pub mod link_rewrite;
pub mod log;
pub mod maestro_observation;
#[cfg(any(unix, windows))]
pub mod note_files;
#[cfg(unix)]
pub mod note_move;
#[cfg(windows)]
#[path = "note_move_windows.rs"]
pub mod note_move;
#[cfg(any(unix, windows))]
pub mod note_templates;
pub mod note_title;
pub mod obsidian;
pub mod projects;
pub mod properties;
pub mod prose;
pub mod quick_open;
pub mod reminder_append;
pub mod reminder_context;
pub mod reminder_dates;
pub mod reminder_schedule;
pub mod reminder_task;
pub mod render;
pub mod search;
pub mod search_snippet;
#[cfg(all(unix, feature = "brain"))]
pub mod source;
pub mod source_classifier;
#[cfg(any(unix, windows))]
pub mod source_history;
pub mod source_projection;
#[cfg(any(unix, windows))]
mod source_state;
pub mod task_edit;
pub mod tasks;
pub mod typed_view;
pub mod vault;
pub mod watch;

pub use facets::Facets;
pub use ir::{Block, Doc, Inline};
pub use render::render_html;
pub use search::{SearchHit, Searcher};
pub use vault::{Backlink, Note, Resolution, Vault};
pub use watch::{Changes, VaultWatcher};

pub mod json_view;
pub mod recycle_bin;
#[cfg(windows)]
pub mod windows_files;
