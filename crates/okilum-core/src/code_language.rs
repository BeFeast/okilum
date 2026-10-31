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

/// Canonical language name → the file extension an editor recognises it by.
/// Keyed by [`canonical`] names, so every alias in [`ALIASES`] shares it.
const EXTENSIONS: &[(&str, &str)] = &[
    ("bash", "sh"),
    ("c", "c"),
    ("cmake", "cmake"),
    ("cpp", "cpp"),
    ("csharp", "cs"),
    ("css", "css"),
    ("dart", "dart"),
    ("diff", "diff"),
    ("dockerfile", "dockerfile"),
    ("elixir", "ex"),
    ("go", "go"),
    ("graphql", "graphql"),
    ("haskell", "hs"),
    ("html", "html"),
    ("ini", "ini"),
    ("java", "java"),
    ("javascript", "js"),
    ("json", "json"),
    ("jsx", "jsx"),
    ("kotlin", "kt"),
    ("lua", "lua"),
    ("make", "mk"),
    ("markdown", "md"),
    ("perl", "pl"),
    ("php", "php"),
    ("plaintext", "txt"),
    ("powershell", "ps1"),
    ("proto", "proto"),
    ("python", "py"),
    ("r", "r"),
    ("ruby", "rb"),
    ("rust", "rs"),
    ("scala", "scala"),
    ("scss", "scss"),
    ("sql", "sql"),
    ("svelte", "svelte"),
    ("swift", "swift"),
    ("text", "txt"),
    ("toml", "toml"),
    ("tsx", "tsx"),
    ("typescript", "ts"),
    ("vue", "vue"),
    ("xml", "xml"),
    ("yaml", "yaml"),
    ("zig", "zig"),
];

/// The file extension for a code block's language, for a temporary file an
/// external editor opens (#1993). A known language (by any alias) gets its
/// usual extension; an unknown one that is a short plain word is used as
/// written (`rb`, `kt`); anything else, or no language, is plain text.
pub fn file_extension(language: Option<&str>) -> String {
    let Some(language) = language.map(canonical).filter(|l| !l.is_empty()) else {
        return "txt".into();
    };
    if let Some((_, extension)) = EXTENSIONS.iter().find(|(name, _)| *name == language) {
        return (*extension).into();
    }
    if language.len() <= 16 && language.bytes().all(|b| b.is_ascii_alphanumeric()) {
        language
    } else {
        "txt".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_extensions_follow_the_shared_alias_table() {
        for (language, expected) in [
            (Some("rust"), "rs"),
            (Some("rs"), "rs"),
            (Some("RS"), "rs"),
            (Some("py"), "py"),
            (Some("python"), "py"),
            (Some("C#"), "cs"),
            (Some("cs"), "cs"),
            (Some("c++"), "cpp"),
            (Some("shell"), "sh"),
            (Some("yml"), "yaml"),
            (Some("ts"), "ts"),
            (Some("js"), "js"),
            (Some("rb"), "rb"),
            (Some("text"), "txt"),
            (None, "txt"),
            (Some(""), "txt"),
            (Some("../etc"), "txt"),
            (Some("objective-c"), "txt"),
        ] {
            assert_eq!(file_extension(language), expected, "{language:?}");
        }
        // Every alias resolves to the same extension as its canonical name.
        for (alias, canonical_name) in ALIASES {
            assert_eq!(
                file_extension(Some(alias)),
                file_extension(Some(canonical_name)),
                "{alias}"
            );
        }
    }

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
