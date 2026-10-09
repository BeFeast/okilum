//! Code-signing requirements for the supervisor image, built from policy input and
//! never from literals in this repository (docs/sync-sidecar-discovery.md). The text
//! is a macOS code requirement; building and validating it is portable so the rules
//! are tested everywhere, while evaluating it needs the macOS Security framework.
//! Every component is validated to a closed alphabet so policy input can never add
//! clauses to the requirement language.
use anyhow::{ensure, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeRequirement(String);
impl CodeRequirement {
    /// Release policy: an Apple-anchored signature by `team_id` for `identifier`.
    /// Both come from the release build configuration (`OKILUM_SIGNING_TEAM_ID` and
    /// the helper's bundle identifier derived from the app's), not from this code.
    pub fn team_and_identifier(team_id: &str, identifier: &str) -> Result<Self> {
        ensure!(
            team_id.len() == 10
                && team_id
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
            "invalid Apple Team ID"
        );
        Self::check_identifier(identifier)?;
        Ok(Self(format!(
            "anchor apple generic and identifier \"{identifier}\" and certificate leaf[subject.OU] = \"{team_id}\""
        )))
    }
    /// Development policy: exactly one code directory hash (the pinned digest of the
    /// staged binary). Selected only behind a compile-time `cfg` by the caller.
    pub fn code_directory_hash(hex: &str) -> Result<Self> {
        ensure!(
            hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid code directory hash"
        );
        Ok(Self(format!("cdhash H\"{}\"", hex.to_ascii_lowercase())))
    }
    fn check_identifier(identifier: &str) -> Result<()> {
        ensure!(
            !identifier.is_empty()
                && identifier.len() <= 155
                && identifier
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_'),
            "invalid code identifier"
        );
        Ok(())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements_are_built_only_from_closed_alphabets() {
        let release =
            CodeRequirement::team_and_identifier("ABCDE12345", "org.example.helper-1_a").unwrap();
        assert_eq!(
            release.as_str(),
            "anchor apple generic and identifier \"org.example.helper-1_a\" and certificate leaf[subject.OU] = \"ABCDE12345\""
        );
        let dev = CodeRequirement::code_directory_hash("0123456789ABCDEF0123456789abcdef01234567")
            .unwrap();
        assert_eq!(
            dev.as_str(),
            "cdhash H\"0123456789abcdef0123456789abcdef01234567\""
        );
        for team in [
            "",
            "abcde12345",
            "ABCDE1234",
            "ABCDE123456",
            "ABCDE1234\"",
            "ABCDE 1234",
        ] {
            assert!(
                CodeRequirement::team_and_identifier(team, "org.example.helper").is_err(),
                "{team:?}"
            );
        }
        for identifier in [
            "",
            "a b",
            "a\"b",
            "a\" or anchor apple or identifier \"b",
            "a\\nb",
            "a/b",
            "é",
            &"a".repeat(156),
        ] {
            assert!(
                CodeRequirement::team_and_identifier("ABCDE12345", identifier).is_err(),
                "{identifier:?}"
            );
        }
        for hash in [
            "",
            "zz",
            &"a".repeat(39),
            &"a".repeat(41),
            "0123456789abcdef0123456789abcdef0123456\"",
        ] {
            assert!(
                CodeRequirement::code_directory_hash(hash).is_err(),
                "{hash:?}"
            );
        }
    }
}
