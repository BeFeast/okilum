//! T3 modelPickerSearch/searchRanking tiers, using UTF-16 offsets like its JS
//! reference. Every query token must match; short tokens do not use fuzzy match.
fn field_score(value: &str, token: &str, base: i32) -> Option<i32> {
    let value: Vec<_> = value.encode_utf16().collect();
    let token: Vec<_> = token.encode_utf16().collect();
    if value.is_empty() || token.is_empty() {
        return None;
    }
    if value == token {
        return Some(base);
    }
    let penalty = value.len().saturating_sub(token.len()).min(64) as i32;
    if value.starts_with(&token) {
        return Some(base + 2 + penalty);
    }
    let index = value.windows(token.len()).position(|part| part == token);
    let boundary = value
        .windows(token.len())
        .enumerate()
        .find_map(|(i, part)| {
            (i > 0 && matches!(value[i - 1], 32 | 45 | 95 | 47) && part == token).then_some(i)
        });
    if let Some(index) = boundary {
        return Some(base + 4 + index as i32 * 2 + penalty);
    }
    if let Some(index) = index {
        return Some(base + 6 + index as i32 * 2 + penalty);
    }
    if token.len() < 3 {
        return None;
    }
    let mut next = 0;
    let mut first = 0;
    let mut previous = 0;
    let mut gaps = 0;
    for (index, ch) in value.iter().enumerate() {
        if *ch != token[next] {
            continue;
        }
        if next == 0 {
            first = index;
        } else {
            gaps += index - previous - 1;
        }
        previous = index;
        next += 1;
        if next == token.len() {
            let span = index - first + 1 - token.len();
            return Some(base + 100 + (first * 2 + gaps * 3 + span) as i32 + penalty);
        }
    }
    None
}

pub(super) fn score(query: &str, fields: &[&str], favorite: bool) -> Option<i32> {
    let mut fields: Vec<_> = fields
        .iter()
        .map(|field| field.trim().to_lowercase())
        .collect();
    fields.push(fields.join(" ").trim().to_owned());
    let mut total = 0;
    for token in query.split_whitespace().map(str::to_lowercase) {
        total += fields
            .iter()
            .enumerate()
            .filter_map(|(index, field)| field_score(field, &token, index as i32 * 10))
            .min()?;
    }
    Some(total - if favorite { 24 } else { 0 })
}
