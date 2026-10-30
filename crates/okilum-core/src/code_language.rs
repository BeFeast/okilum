//! Display/highlighting language names shared by Reader and Live Preview.

pub const ALIASES: &[(&str, &str)] = &[
    ("c#", "csharp"),
    ("cs", "csharp"),
    ("c-sharp", "csharp"),
    ("c_sharp", "csharp"),
    ("c++", "cpp"),
    ("sh", "bash"),
    ("shell", "bash"),
    ("yml", "yaml"),
    ("js", "javascript"),
    ("ts", "typescript"),
    ("py", "python"),
    ("rs", "rust"),
];

/// An explicit unknown language remains explicit; never infer another grammar.
pub fn canonical(language: &str) -> String {
    let lower = language.trim().to_ascii_lowercase();
    if let Some((_, canonical)) = ALIASES.iter().find(|(alias, _)| *alias == lower) {
        (*canonical).to_owned()
    } else {
        lower
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csharp_and_popular_aliases_share_canonical_names() {
        for (language, expected) in [
            ("csharp", "csharp"),
            ("cs", "csharp"),
            ("C#", "csharp"),
            (" c_sharp ", "csharp"),
            ("C++", "cpp"),
            ("RS", "rust"),
            ("py", "python"),
            ("js", "javascript"),
            ("typescript", "typescript"),
            ("custom-language", "custom-language"),
        ] {
            assert_eq!(canonical(language), expected);
        }
    }
}
