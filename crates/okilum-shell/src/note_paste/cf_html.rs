/// CF_HTML offsets are byte offsets into the complete UTF-8 payload, not chars.
/// Invalid/absent HTML falls back to the same snapshot's declared plain flavor.
pub(crate) fn fragment(raw: &str) -> Option<(String, Option<String>)> {
    let field = |key: &str| {
        raw.lines()
            .take(32)
            .find_map(|line| line.strip_prefix(key).map(str::trim))
    };
    let start: usize = field("StartFragment:")?.parse().ok()?;
    let end: usize = field("EndFragment:")?.parse().ok()?;
    let html_start = field("StartHTML:")?.parse::<isize>().ok()?;
    let html_end = field("EndHTML:")?.parse::<isize>().ok()?;
    // CF_HTML permits -1 when no surrounding HTML context is supplied.
    if html_start < -1 || html_end < -1 {
        return None;
    }
    let html_start = if html_start == -1 {
        start
    } else {
        html_start as usize
    };
    let html_end = if html_end == -1 {
        raw.len()
    } else {
        html_end as usize
    };
    if !(html_start <= start && start <= end && end <= html_end && html_end <= raw.len()) {
        return None;
    }
    let header = raw.get(..html_start)?;
    // Offsets cannot point back into the CF_HTML header itself.
    if !header.contains("EndFragment:") {
        return None;
    }
    let html = raw.get(start..end)?;
    if html.contains('\0') {
        return None;
    }
    let base = header
        .lines()
        .find_map(|line| line.strip_prefix("SourceURL:"))
        .and_then(|s| url::Url::parse(s.trim()).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https" | "file"))
        .map(|u| u.to_string());
    Some((html.to_owned(), base))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_html_fragment_uses_byte_offsets_and_rejects_invalid_ranges() {
        let header = "Version:1.0\r\nStartHTML:-1\r\nEndHTML:-1\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
        let html = "<b>שלום 🧠</b>";
        let raw = format!(
            "{}{}",
            header
                .replace(
                    "StartFragment:0000000000",
                    &format!("StartFragment:{:010}", header.len())
                )
                .replace(
                    "EndFragment:0000000000",
                    &format!("EndFragment:{:010}", header.len() + html.len())
                ),
            html
        );
        assert_eq!(fragment(&raw).unwrap().0, html);
        for (start, end) in [
            (0, raw.len()),
            (header.len(), raw.len() + 1),
            (raw.len(), header.len()),
            (header.len() + 4, raw.len()),
        ] {
            let invalid = raw
                .replace(
                    &format!("StartFragment:{:010}", header.len()),
                    &format!("StartFragment:{start:010}"),
                )
                .replace(
                    &format!("EndFragment:{:010}", raw.len()),
                    &format!("EndFragment:{end:010}"),
                );
            assert!(fragment(&invalid).is_none());
        }
    }
}
