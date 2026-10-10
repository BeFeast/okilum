//! Shared prepared-link navigation; destination routing for note panes lives here.
use super::*;

/// The chooser's question: files are files, not documents (#315).
pub(super) fn ambiguous_notice(candidates: &[String]) -> &'static str {
    if !candidates.is_empty()
        && candidates
            .iter()
            .all(|path| !path.to_lowercase().ends_with(".md"))
    {
        "This link matches several files. Choose one."
    } else {
        "This document link is ambiguous. Choose its destination."
    }
}

/// Link handling shared by the reader's TextView and every nested one (a
/// callout body): wikilinks open notes, ambiguous ones go to search,
/// unresolved ones are inert, http(s) leaves the app.
pub(super) fn handle_link(
    entity: &WeakEntity<Reader>,
    url: &str,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(entity) = entity.upgrade() {
        let landed = entity.update(cx, |this, cx| {
            let landing = reader_obsidian::footnote_landing(url, &this.note_source)?;
            match landing {
                Ok(ix) => this.scroll_to_block(ix, cx),
                Err(reason) => this.link_notice = Some(reason.into()),
            }
            cx.notify();
            Some(())
        });
        if landed.is_some() {
            return;
        }
    }
    let prepared = entity.upgrade().and_then(|entity| {
        let reader = entity.read(cx);
        reader.prepared_links.get(url).cloned().or_else(|| {
            reader
                .link_identities
                .iter()
                .any(|link| link.url == url)
                .then(okilum_core::document_links::prepared::LinkState::unknown)
        })
    });
    if let Some(path) = prepared
        .as_ref()
        .filter(|s| s.status == okilum_core::document_links::prepared::LinkStatus::MissingFile)
        .and_then(|s| s.action_url.as_deref())
        .and_then(|u| u.strip_prefix("okilum://missing-file/"))
    {
        reader_toast::missing_file(okilum_core::document_links::decode(path), window, cx);
        return;
    }
    if prepared
        .as_ref()
        .is_some_and(|state| state.status.is_missing())
    {
        return;
    }
    if let Some(state) = prepared
        .as_ref()
        .filter(|state| state.status == okilum_core::document_links::prepared::LinkStatus::Unknown)
    {
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                this.link_notice = Some(state.reason.clone().into());
                cx.notify();
            });
        }
        return;
    }
    let url = prepared
        .as_ref()
        .and_then(|state| state.action_url.as_deref())
        .unwrap_or(url);
    if url.starts_with("okilum://outside-file/") {
        let _ = entity.update(cx, |this, cx| this.outside_file_menu(url, window, cx));
    } else if let Some(rest) = url.strip_prefix("okilum://attachment/") {
        let _ = entity.update(cx, |this, cx| {
            this.preview_file(&okilum_core::document_links::decode(rest), window, cx)
        });
    } else if let Some(rest) = url.strip_prefix(WIKI_SCHEME) {
        // `[[note#Heading]]` carries the heading past the rewrite (#49). An
        // empty path is `[[#Heading]]` in a file outside the vault: the note
        // on screen.
        let (rel, heading) = split_open_url(rest);
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                let rel = if rel.is_empty() {
                    this.current_rel.clone()
                } else {
                    rel
                };
                this.open_note_at(&rel, None, heading.as_deref(), window, cx);
            });
        }
    } else if let Some((rest, wiki)) = url
        .strip_prefix(AMBIGUOUS_SCHEME)
        .map(|r| (r, true))
        .or_else(|| {
            url.strip_prefix("okilum://ambiguous-markdown/")
                .map(|r| (r, false))
        })
    {
        let target = okilum_core::document_links::decode(rest);
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                let resolved = okilum_core::document_links::resolve(
                    &target,
                    wiki,
                    &this.vault,
                    &this.current_rel,
                );
                this.link_notice = Some(ambiguous_notice(&resolved.candidates).into());
                this.link_choices = resolved
                    .candidates
                    .into_iter()
                    .map(|path| (path, resolved.heading.clone()))
                    .collect();
                cx.notify();
            });
        }
    } else if let Some(reason) = url.strip_prefix("okilum://unsupported/") {
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                this.link_notice = Some(okilum_core::document_links::decode(reason).into());
                this.link_choices.clear();
                cx.notify();
            });
        }
    } else if url.starts_with(UNRESOLVED_SCHEME) {
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                this.link_notice = Some(
                    format!(
                        "No document matches this link: {}",
                        okilum_core::document_links::decode(
                            url.trim_start_matches(UNRESOLVED_SCHEME)
                        )
                    )
                    .into(),
                );
                cx.notify();
            });
        }
    } else if prepared_links::external_tooltip(url).is_some() {
        cx.open_url(url);
    } else if let Some(entity) = entity.upgrade() {
        entity.update(cx, |this, cx| {
            this.link_notice = Some("This link action is not supported.".into());
            cx.notify();
        });
    }
}

#[cfg(test)]
mod ambiguous_notice_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn ambiguous_files_are_called_files() {
        let files = ["a/shared.csv".to_owned(), "b/shared.csv".to_owned()];
        assert_eq!(
            ambiguous_notice(&files),
            "This link matches several files. Choose one."
        );
        // Positive control: notes keep the document wording.
        let notes = ["a/n.md".to_owned(), "b/N.MD".to_owned()];
        assert_eq!(
            ambiguous_notice(&notes),
            "This document link is ambiguous. Choose its destination."
        );
        assert_eq!(
            ambiguous_notice(&[]),
            "This document link is ambiguous. Choose its destination."
        );
    }
}
