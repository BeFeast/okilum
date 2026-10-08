//! Shared prepared-link navigation; destination routing for note panes lives here.
use super::*;

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
                .then(tessera_core::document_links::prepared::LinkState::unknown)
        })
    });
    if let Some(path) = prepared
        .as_ref()
        .filter(|s| s.status == tessera_core::document_links::prepared::LinkStatus::MissingFile)
        .and_then(|s| s.action_url.as_deref())
        .and_then(|u| u.strip_prefix("tessera://missing-file/"))
    {
        reader_toast::missing_file(tessera_core::document_links::decode(path), window, cx);
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
        .filter(|state| state.status == tessera_core::document_links::prepared::LinkStatus::Unknown)
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
    if url.starts_with("tessera://outside-file/") {
        let _ = entity.update(cx, |this, cx| this.outside_file_menu(url, window, cx));
    } else if let Some(rest) = url.strip_prefix("tessera://attachment/") {
        let _ = entity.update(cx, |this, cx| {
            this.preview_file(&tessera_core::document_links::decode(rest), window, cx)
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
            url.strip_prefix("tessera://ambiguous-markdown/")
                .map(|r| (r, false))
        })
    {
        let target = tessera_core::document_links::decode(rest);
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                let resolved = tessera_core::document_links::resolve(
                    &target,
                    wiki,
                    &this.vault,
                    &this.current_rel,
                );
                this.link_notice =
                    Some("This document link is ambiguous. Choose its destination.".into());
                this.link_choices = resolved
                    .candidates
                    .into_iter()
                    .map(|path| (path, resolved.heading.clone()))
                    .collect();
                cx.notify();
            });
        }
    } else if let Some(reason) = url.strip_prefix("tessera://unsupported/") {
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                this.link_notice = Some(tessera_core::document_links::decode(reason).into());
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
                        tessera_core::document_links::decode(
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
