//! English labels for visible numeric counts.

pub(crate) fn count_label(count: usize, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{count} {noun}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singular_only_for_one() {
        for (singular, plural) in [
            ("task", "tasks"),
            ("note", "notes"),
            ("link", "links"),
            ("item", "items"),
        ] {
            assert_eq!(count_label(0, singular, plural), format!("0 {plural}"));
            assert_eq!(count_label(1, singular, plural), format!("1 {singular}"));
            assert_eq!(count_label(2, singular, plural), format!("2 {plural}"));
        }
    }
}
