use okilum_core::{search::SearchDocument, search_snippet::plain_snippet, Searcher};

#[test]
fn long_note_adjacent_alias_outweighs_short_scattered_matches_after_reopen() -> anyhow::Result<()> {
    let line = "- **Cloudflare 22.09:** [[Finance/Areas/Day-to-Day Operations/2026-09-22-cloudflare-billing-review|проверка Billing]] — Dashboard доступен: все 18 invoices Paid";
    let body = format!(
        "# Finance\n\nEarlier billing mention\n{}\n{line}\n",
        "Unrelated finance context. ".repeat(300)
    );
    let mut docs = vec![SearchDocument {
        path: "Finance/_index.md".into(),
        title: "Finance Index".into(),
        text: body,
    }];
    for i in 0..40 {
        docs.push(SearchDocument {
            path: format!("other/{i}.md"),
            title: "Other".into(),
            text: "проверка unrelated separate context Billing".into(),
        });
    }
    let index = tempfile::tempdir()?;
    let fresh = Searcher::build_documents(&docs, index.path())?;
    for searcher in [fresh, Searcher::open(index.path())?] {
        let hits = searcher.search("проверка Billing", 100)?;
        assert_eq!(hits.len(), 41);
        assert_eq!(hits[0].path, "Finance/_index.md");
        let explanation: serde_json::Value = serde_json::from_str(
            &searcher
                .explain("проверка Billing", "Finance/_index.md")?
                .unwrap(),
        )?;
        assert!((explanation["value"].as_f64().unwrap() - f64::from(hits[0].score)).abs() < 0.001);
        let snippet = plain_snippet(&hits[0].snippet_html);
        let highlights: Vec<_> = snippet
            .highlights
            .iter()
            .map(|r| &snippet.text[r.clone()])
            .collect();
        assert!(
            highlights.iter().any(|t| t.contains("проверка")),
            "{snippet:?}"
        );
        assert!(
            highlights.iter().any(|t| t.contains("Billing")),
            "{snippet:?}"
        );
        let scoped = searcher.search_in_paths("проверка Billing", 100, &["other/0.md".into()])?;
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].path, "other/0.md");
    }
    Ok(())
}

#[test]
fn ordinary_punctuation_retains_phrase_preference() -> anyhow::Result<()> {
    for (query, scattered) in [
        ("day-to-day billing", "day-to-day unrelated billing"),
        ("what’s billing", "what’s unrelated billing"),
        ("what billing?", "what unrelated billing"),
    ] {
        let root = tempfile::tempdir()?;
        let mut documents = vec![SearchDocument {
            path: "adjacent.md".into(),
            title: "Long note".into(),
            text: format!("{} {query}", "unrelated context ".repeat(400)),
        }];
        for i in 0..40 {
            documents.push(SearchDocument {
                path: format!("other/{i}.md"),
                title: "Other".into(),
                text: scattered.into(),
            });
        }
        let searcher = Searcher::build_documents(&documents, root.path())?;
        let hits = searcher.search(query, 100)?;
        assert_eq!(hits.len(), 41, "query={query}");
        assert_eq!(hits[0].path, "adjacent.md", "query={query}");
        assert!(searcher
            .search(&format!("path:other {query}"), 100)?
            .iter()
            .all(|h| h.path != "adjacent.md"));
    }
    Ok(())
}
