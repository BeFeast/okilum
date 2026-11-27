//! Pure T3-compatible prompt recall. History derives from the active thread's
//! user messages; only the current browsing position is ephemeral UI state.
//! The UI must gate calls on visual caret edges, IME and attachment state, and
//! clear `Position` on thread/scope changes. `None` means normal caret movement.

pub const ATTACHMENT_ONLY_BOOTSTRAP_PROMPT: &str = "[User attached one or more files without additional text. Respond using the conversation context and the attached files.]";
const PLAN_IMPLEMENTATION_PROMPT_PREFIX: &str = "PLEASE IMPLEMENT THIS PLAN:\n";
const CLAUDE_ULTRATHINK_PREFIX: &str = "Ultrathink:\n";

#[derive(Clone, Copy, Debug)]
pub struct Message<'a> {
    pub id: &'a str,
    pub role: &'a str,
    pub text: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub prompt: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub entry_id: String,
    pub recalled: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Backward,
    Forward,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub position: Option<Position>,
    pub prompt: String,
}

fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn trim(value: &str) -> &str {
    value.trim_matches(js_whitespace)
}

/// Equivalent to the global non-greedy `<review_comment\b[^>]*>...` pattern.
/// Only its trailing run is stripped; blocks typed earlier are preserved.
fn strip_trailing_review_comments(prompt: &str) -> &str {
    const OPEN: &str = "<review_comment";
    const CLOSE: &str = "</review_comment>";
    let mut matches = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = prompt[cursor..].find(OPEN) {
        let start = cursor + offset;
        let after_name = start + OPEN.len();
        // JavaScript \b uses ASCII word characters, unlike Unicode regex \b.
        if prompt[after_name..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            cursor = after_name;
            continue;
        }
        let Some(header_end) = prompt[after_name..].find('>') else {
            break;
        };
        let body_start = after_name + header_end + 1;
        let Some(close) = prompt[body_start..].find(CLOSE) else {
            break;
        };
        let end = body_start + close + CLOSE.len();
        matches.push((start, end));
        cursor = end;
    }
    let mut cut = prompt.len();
    for (start, end) in matches.into_iter().rev() {
        if !trim(&prompt[end..cut]).is_empty() {
            break;
        }
        cut = start;
    }
    if cut == prompt.len() {
        prompt
    } else {
        prompt[..cut].trim_end_matches(js_whitespace)
    }
}

fn js_dot(c: char) -> bool {
    !matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn context_headers(body: &str) -> Vec<&str> {
    body.split('\n')
        // JavaScript `$` also matches just before a final line terminator.
        .map(|line| {
            line.strip_suffix('\r')
                .or_else(|| line.strip_suffix('\u{2028}'))
                .or_else(|| line.strip_suffix('\u{2029}'))
                .unwrap_or(line)
        })
        .filter_map(|line| line.strip_prefix("- ")?.strip_suffix(':'))
        .filter(|header| !header.is_empty() && header.chars().all(js_dot))
        .collect()
}

/// T3's element/terminal extractor matches the first opener that can reach the
/// trailing close; a block is only removed when it has at least one `- header:`.
fn trailing_context<'a>(prompt: &'a str, tag: &str) -> Option<(&'a str, Vec<&'a str>)> {
    let trimmed = prompt.trim_end_matches(js_whitespace);
    let close = format!("\n</{tag}>");
    let before_close = trimmed.strip_suffix(&close)?;
    let open = format!("<{tag}>\n");
    let start = before_close.find(&open)?;
    let body = &before_close[start + open.len()..];
    let headers = context_headers(body);
    if headers.is_empty() {
        return None;
    }
    Some((before_close[..start].trim_end_matches('\n'), headers))
}

/// Unlike element/terminal blocks, any syntactically complete preview block
/// counts as an annotation. Its body must not contain another preview opener.
fn trailing_preview_annotation(prompt: &str) -> Option<&str> {
    const OPEN: &str = "<preview_annotation>\n";
    let trimmed = prompt.trim_end_matches(js_whitespace);
    let before_close = trimmed.strip_suffix("\n</preview_annotation>")?;
    let start = before_close.rfind(OPEN)?;
    if before_close[start + OPEN.len()..].contains("<preview_annotation>") {
        return None;
    }
    Some(before_close[..start].trim_end_matches('\n'))
}

fn terminal_label(header: &str) -> Option<String> {
    for separator in [" lines ", " line "] {
        let Some((name, range)) = header.rsplit_once(separator) else {
            continue;
        };
        if name.is_empty() || !name.chars().all(js_dot) {
            continue;
        }
        let mut parts = range.split('-');
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit());
        if !digits(parts.next()?)
            || parts.next().is_some_and(|part| !digits(part))
            || parts.next().is_some()
        {
            continue;
        }
        let mut normalized = String::new();
        let mut whitespace = false;
        for c in trim(name).to_lowercase().chars() {
            if js_whitespace(c) {
                whitespace = true;
            } else {
                if whitespace {
                    normalized.push('-');
                }
                whitespace = false;
                normalized.push(c);
            }
        }
        return Some(format!("@{normalized}:{range}"));
    }
    None
}

fn strip_inline_terminal_labels(mut prompt: String, headers: &[&str]) -> String {
    for header in headers {
        let Some(label) = terminal_label(header) else {
            continue;
        };
        let Some(index) = prompt.match_indices(&label).find_map(|(index, _)| {
            let end = index + label.len();
            // Whole label: 4 cannot consume the start of 40 or 4-12.
            (!prompt
                .as_bytes()
                .get(end)
                .is_some_and(|c| c.is_ascii_digit() || *c == b'-'))
            .then_some(index)
        }) else {
            continue;
        };
        let mut end = index + label.len();
        let mut start = index;
        if prompt.as_bytes().get(end) == Some(&b' ') {
            end += 1;
        } else if start > 0 && prompt.as_bytes()[start - 1] == b' ' {
            start -= 1;
        }
        prompt.replace_range(start..end, "");
    }
    prompt
}

/// Reduce a sent message to the user's typed prompt without stale appended
/// browser/terminal/review context. Generated plan and image-only sends vanish.
pub fn recallable_prompt(message_text: &str) -> String {
    let initial = trim(message_text);
    let mut prompt = initial
        .strip_prefix(CLAUDE_ULTRATHINK_PREFIX)
        .unwrap_or(initial)
        .to_owned();
    while !prompt.is_empty() {
        let without_comments = strip_trailing_review_comments(&prompt);
        if without_comments.len() != prompt.len() {
            prompt = without_comments.to_owned();
            continue;
        }
        if let Some(without_preview) = trailing_preview_annotation(&prompt) {
            prompt = without_preview.to_owned();
            continue;
        }
        if let Some((without_elements, _)) = trailing_context(&prompt, "element_context") {
            prompt = without_elements.to_owned();
            continue;
        }
        if let Some((without_terminal, headers)) = trailing_context(&prompt, "terminal_context") {
            prompt = strip_inline_terminal_labels(without_terminal.to_owned(), &headers);
            continue;
        }
        break;
    }
    let trimmed = trim(&prompt);
    if trimmed == ATTACHMENT_ONLY_BOOTSTRAP_PROMPT
        || trimmed.starts_with(PLAN_IMPLEMENTATION_PROMPT_PREFIX)
    {
        String::new()
    } else {
        trimmed.to_owned()
    }
}

/// The input is chronological (oldest first). Consecutive matching user
/// prompts collapse onto the newest message ID, even across non-user rows.
pub fn build_entries<'a>(messages: impl IntoIterator<Item = Message<'a>>) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for message in messages {
        if message.role != "user" {
            continue;
        }
        let prompt = recallable_prompt(message.text);
        if prompt.is_empty() {
            continue;
        }
        let entry = Entry {
            id: message.id.to_owned(),
            prompt,
        };
        if entries
            .last()
            .is_some_and(|previous| previous.prompt == entry.prompt)
        {
            *entries.last_mut().expect("previous entry exists") = entry;
        } else {
            entries.push(entry);
        }
    }
    entries
}

/// Resolve by ID after acknowledgement/history changes; when duplicate
/// collapse retires that ID, use the newest surviving identical prompt.
pub fn step(
    direction: Direction,
    entries: &[Entry],
    position: Option<&Position>,
    current_prompt: &str,
) -> Option<Step> {
    let active = position
        .filter(|p| p.recalled == current_prompt)
        .and_then(|p| {
            entries
                .iter()
                .position(|entry| entry.id == p.entry_id)
                .or_else(|| entries.iter().rposition(|entry| entry.prompt == p.recalled))
        });
    let index = match direction {
        Direction::Backward => {
            if active.is_none() && !current_prompt.is_empty() {
                return None;
            }
            match active {
                Some(index) => index.checked_sub(1)?,
                None => entries.len().checked_sub(1)?,
            }
        }
        Direction::Forward => active? + 1,
    };
    let Some(entry) = entries.get(index) else {
        return Some(Step {
            position: None,
            prompt: String::new(),
        });
    };
    Some(Step {
        position: Some(Position {
            entry_id: entry.id.clone(),
            recalled: entry.prompt.clone(),
        }),
        prompt: entry.prompt.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(prompt: &str, header: &str) -> String {
        format!("{prompt}\n\n<terminal_context>\n- {header}:\n  12 | git status\n  13 | On branch main\n</terminal_context>")
    }

    fn entries() -> Vec<Entry> {
        build_entries([
            Message {
                id: "m1",
                role: "user",
                text: "first",
            },
            Message {
                id: "a1",
                role: "assistant",
                text: "reply",
            },
            Message {
                id: "m2",
                role: "user",
                text: "second",
            },
            Message {
                id: "m3",
                role: "user",
                text: "third",
            },
        ])
    }

    fn position(id: &str, prompt: &str) -> Position {
        Position {
            entry_id: id.into(),
            recalled: prompt.into(),
        }
    }

    #[test]
    fn strips_send_time_blocks_prefix_and_preview_in_repeated_passes() {
        let sent = format!("Ultrathink:\n{}\n\n<element_context>\n- button.submit:\n  page: Example\n  html:\n    <button>Save</button>\n</element_context>\n\n<preview_annotation>\nPreview annotation:\nId: preview-1\nPage: Example\nComment: Adjust spacing\n<element_context>\n- button.submit:\n  page: Example\n</element_context>\n</preview_annotation>\n\n<review_comment file=\"src/app.ts\">Keep configurable.</review_comment>", terminal("Investigate this", "Terminal 1 lines 12-13"));
        assert_eq!(recallable_prompt(&sent), "Investigate this");
    }

    #[test]
    fn removes_only_one_whole_inline_terminal_label_per_context() {
        assert_eq!(
            recallable_prompt(&terminal(
                "Look at @terminal-1:12-13 please",
                "Terminal 1 lines 12-13"
            )),
            "Look at please"
        );
        assert_eq!(
            recallable_prompt(&terminal(
                "@terminal-1:4 typed twice: @terminal-1:4\n    indented  code",
                "Terminal 1 line 4"
            )),
            "typed twice: @terminal-1:4\n    indented  code"
        );
        assert_eq!(
            recallable_prompt(&terminal(
                "see @terminal-1:40 and @terminal-1:4-12 then @terminal-1:4",
                "Terminal 1 line 4"
            )),
            "see @terminal-1:40 and @terminal-1:4-12 then"
        );
        let sent = "A @build-log:7 B @terminal-1:8 C\n\n<terminal_context>\n- Build Log line 7:\n  7 | build output\n\n- Terminal 1 line 8:\n  8 | pwd\n</terminal_context>";
        assert_eq!(recallable_prompt(sent), "A B C");
    }

    #[test]
    fn preserves_typed_tags_labels_and_indentation_when_not_trailing_context() {
        let review = "<review_comment file=\"src/app.ts\">Keep configurable.</review_comment>";
        let middle = format!("Before\n\n{review}\n\nAfter\n    indented  code");
        assert_eq!(recallable_prompt(&middle), middle);
        assert_eq!(recallable_prompt(&format!("{middle}\n\n{review}")), middle);
        let typed =
            "Use @terminal-1:4\n\n<element_context>\nnot a generated entry\n</element_context>";
        assert_eq!(recallable_prompt(typed), typed);
        let typed_terminal = "<terminal_context>\n- Terminal 1 line 4:\n  4 | ls\n</terminal_context>\n\nKeep this literal block";
        assert_eq!(recallable_prompt(typed_terminal), typed_terminal);
        let other_tag = "Please preserve <review_commentary>typed</review_commentary>";
        assert_eq!(recallable_prompt(other_tag), other_tag);
    }

    #[test]
    fn preview_annotation_extractor_rejects_nested_openers_and_handles_siblings() {
        let first = "<preview_annotation>\nPage: first\n</preview_annotation>";
        let second = "<preview_annotation>\nPage: second\n</preview_annotation>";
        assert_eq!(
            recallable_prompt(&format!("Typed\n\n{first}\n\n{second}")),
            "Typed"
        );
        let incomplete = "Typed\n<preview_annotation>\nPage: outer\n<preview_annotation>broken\n</preview_annotation>";
        assert_eq!(recallable_prompt(incomplete), incomplete);
    }

    #[test]
    fn generated_sends_and_empty_messages_do_not_enter_history() {
        for text in [
            "   ",
            ATTACHMENT_ONLY_BOOTSTRAP_PROMPT,
            "PLEASE IMPLEMENT THIS PLAN:\n# Plan\n1. do it",
        ] {
            assert_eq!(recallable_prompt(text), "");
        }
        assert_eq!(
            recallable_prompt("Explain PLEASE IMPLEMENT THIS PLAN:\nwithout executing it"),
            "Explain PLEASE IMPLEMENT THIS PLAN:\nwithout executing it"
        );
        assert_eq!(
            recallable_prompt("\u{feff} trim JS whitespace \u{feff}"),
            "trim JS whitespace"
        );
        assert_eq!(
            recallable_prompt("\u{0085}not JS whitespace\u{0085}"),
            "\u{0085}not JS whitespace\u{0085}"
        );
    }

    #[test]
    fn entries_are_user_only_oldest_first_with_consecutive_newest_id_deduplication() {
        let base = entries();
        assert_eq!(
            base.iter()
                .map(|entry| entry.prompt.as_str())
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
        let collapsed = build_entries([
            Message {
                id: "m1",
                role: "user",
                text: "same",
            },
            Message {
                id: "a",
                role: "assistant",
                text: "reply",
            },
            Message {
                id: "bootstrap",
                role: "user",
                text: ATTACHMENT_ONLY_BOOTSTRAP_PROMPT,
            },
            Message {
                id: "m2",
                role: "user",
                text: "same",
            },
            Message {
                id: "m3",
                role: "user",
                text: "other",
            },
            Message {
                id: "m4",
                role: "user",
                text: "same",
            },
        ]);
        assert_eq!(
            collapsed,
            vec![
                Entry {
                    id: "m2".into(),
                    prompt: "same".into()
                },
                Entry {
                    id: "m3".into(),
                    prompt: "other".into()
                },
                Entry {
                    id: "m4".into(),
                    prompt: "same".into()
                }
            ]
        );
    }

    #[test]
    fn backward_starts_only_empty_walks_to_oldest_and_leaves_caret_at_boundary() {
        let entries = entries();
        assert_eq!(step(Direction::Backward, &entries, None, "typing"), None);
        let newest = step(Direction::Backward, &entries, None, "").unwrap();
        assert_eq!(newest.position, Some(position("m3", "third")));
        let middle = step(
            Direction::Backward,
            &entries,
            newest.position.as_ref(),
            "third",
        )
        .unwrap();
        assert_eq!(middle.prompt, "second");
        let oldest = step(
            Direction::Backward,
            &entries,
            middle.position.as_ref(),
            "second",
        )
        .unwrap();
        assert_eq!(oldest.prompt, "first");
        assert_eq!(
            step(
                Direction::Backward,
                &entries,
                oldest.position.as_ref(),
                "first"
            ),
            None
        );
        assert_eq!(step(Direction::Backward, &[], None, ""), None);
    }

    #[test]
    fn forward_past_newest_empties_draft_and_ends_browsing() {
        let entries = entries();
        let newer = step(
            Direction::Forward,
            &entries,
            Some(&position("m2", "second")),
            "second",
        )
        .unwrap();
        assert_eq!(newer.prompt, "third");
        assert_eq!(
            step(
                Direction::Forward,
                &entries,
                newer.position.as_ref(),
                "third"
            ),
            Some(Step {
                position: None,
                prompt: String::new()
            })
        );
        assert_eq!(step(Direction::Forward, &entries, None, ""), None);
    }

    #[test]
    fn edited_recall_is_a_fresh_draft_and_sent_empty_restarts_at_latest() {
        let entries = entries();
        let previous = position("m3", "third");
        for direction in [Direction::Backward, Direction::Forward] {
            assert_eq!(
                step(direction, &entries, Some(&previous), "third edited"),
                None
            );
        }
        assert_eq!(
            step(Direction::Backward, &entries, Some(&previous), "")
                .unwrap()
                .position,
            Some(previous)
        );
    }

    #[test]
    fn identity_survives_paging_ack_and_duplicate_collapse() {
        let grown = build_entries([
            Message {
                id: "m0",
                role: "user",
                text: "zeroth",
            },
            Message {
                id: "m1",
                role: "user",
                text: "A",
            },
            Message {
                id: "m2",
                role: "user",
                text: "B",
            },
            Message {
                id: "m3",
                role: "user",
                text: "A",
            },
        ]);
        assert_eq!(
            step(Direction::Backward, &grown, Some(&position("m1", "A")), "A")
                .unwrap()
                .prompt,
            "zeroth"
        );
        assert_eq!(
            step(
                Direction::Forward,
                &grown,
                Some(&position("gone", "not sent")),
                "not sent"
            ),
            None
        );
        // Missing optimistic ID chooses the newest matching text, not the first.
        assert_eq!(
            step(
                Direction::Backward,
                &grown,
                Some(&position("optimistic", "A")),
                "A"
            )
            .unwrap()
            .prompt,
            "B"
        );
        let collapsed = build_entries([
            Message {
                id: "m1",
                role: "user",
                text: "first",
            },
            Message {
                id: "m3",
                role: "user",
                text: "A",
            },
        ]);
        assert_eq!(
            step(
                Direction::Backward,
                &collapsed,
                Some(&position("m2", "A")),
                "A"
            )
            .unwrap()
            .prompt,
            "first"
        );
    }
}
