//! The supervisor's signature policy, taken from the build configuration (design:
//! docs/sync-sidecar-discovery.md, "Signature policy"). Nothing here names a Team ID,
//! bundle identifier or signer: the release build supplies them as environment values
//! that are compiled in, so a rebrand changes the build configuration and no code.
//!
//! * `OKILUM_SIGNING_TEAM_ID` - the Apple Team ID of the release signing identity.
//! * `OKILUM_BUNDLE_ID` - the app's bundle identifier; the helper's code identifier is
//!   this plus `.sync`.
//! * `OKILUM_APP_EXECUTABLE` - the app's executable name next to the helper (optional).
//!
//! Both of the first two or neither: one without the other is a broken configuration
//! and never silently weakens the policy.
use anyhow::{bail, Result};
use okilum_sync_controller::sidecar::supervisor::ipc::code_requirement::CodeRequirement;

/// Suffix that turns the app's bundle identifier into the helper's code identifier.
pub const HELPER_SUFFIX: &str = ".sync";
const DEFAULT_APP_EXECUTABLE: &str = "okilum";

#[derive(Debug, PartialEq, Eq)]
pub struct ReleasePolicy {
    /// Requirement the app must satisfy to talk to the supervisor.
    pub app_requirement: CodeRequirement,
    /// File name of the app's executable, next to the helper in the bundle.
    pub app_executable: String,
    /// Requirement the supervisor must satisfy for the app to talk to it.
    pub helper_requirement: CodeRequirement,
}

fn plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && name != "."
        && name != ".."
}

/// `None` when no signing configuration was compiled in (a development build).
pub fn release_policy(
    team_id: Option<&str>,
    bundle_id: Option<&str>,
    app_executable: Option<&str>,
) -> Result<Option<ReleasePolicy>> {
    let (team, bundle) = match (team_id, bundle_id) {
        (None, None) => return Ok(None),
        (Some(team), Some(bundle)) => (team, bundle),
        _ => bail!("incomplete signing configuration: Team ID and bundle identifier go together"),
    };
    let app_executable = app_executable.unwrap_or(DEFAULT_APP_EXECUTABLE);
    if !plain_file_name(app_executable) {
        bail!("invalid app executable name in the signing configuration");
    }
    Ok(Some(ReleasePolicy {
        app_requirement: CodeRequirement::team_and_identifier(team, bundle)?,
        helper_requirement: CodeRequirement::team_and_identifier(
            team,
            &format!("{bundle}{HELPER_SUFFIX}"),
        )?,
        app_executable: app_executable.to_string(),
    }))
}

/// The policy this binary was built with.
pub fn build_policy() -> Result<Option<ReleasePolicy>> {
    release_policy(
        option_env!("OKILUM_SIGNING_TEAM_ID"),
        option_env!("OKILUM_BUNDLE_ID"),
        option_env!("OKILUM_APP_EXECUTABLE"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Obviously synthetic values: the real ones come from the release build only.
    const TEAM: &str = "ABCDE12345";
    const BUNDLE: &str = "org.example.app";

    #[test]
    fn no_configuration_is_a_development_build_not_an_error() {
        assert_eq!(release_policy(None, None, None).unwrap(), None);
        assert_eq!(release_policy(None, None, Some("whatever")).unwrap(), None);
    }

    #[test]
    fn a_complete_configuration_builds_both_directions_of_the_policy() {
        let policy = release_policy(Some(TEAM), Some(BUNDLE), None)
            .unwrap()
            .unwrap();
        assert_eq!(
            policy.app_requirement,
            CodeRequirement::team_and_identifier(TEAM, "org.example.app").unwrap()
        );
        assert_eq!(
            policy.helper_requirement,
            CodeRequirement::team_and_identifier(TEAM, "org.example.app.sync").unwrap()
        );
        assert_ne!(policy.app_requirement, policy.helper_requirement);
        assert_eq!(policy.app_executable, "okilum");
        let named = release_policy(Some(TEAM), Some(BUNDLE), Some("Renamed-App")).unwrap();
        assert_eq!(named.unwrap().app_executable, "Renamed-App");
    }

    #[test]
    fn half_a_configuration_never_weakens_into_a_development_build() {
        assert!(release_policy(Some(TEAM), None, None).is_err());
        assert!(release_policy(None, Some(BUNDLE), None).is_err());
    }

    #[test]
    fn malformed_values_are_refused_whatever_the_source() {
        for team in ["", "abcde12345", "ABCDE1234", "ABCDE1234\"", "ABCDE 1234"] {
            assert!(
                release_policy(Some(team), Some(BUNDLE), None).is_err(),
                "{team:?}"
            );
        }
        for bundle in [
            "",
            "a b",
            "a\"b",
            "a\" or anchor apple or identifier \"b",
            "a/b",
        ] {
            assert!(
                release_policy(Some(TEAM), Some(bundle), None).is_err(),
                "{bundle:?}"
            );
        }
        // The helper identifier is validated too: a bundle id at the limit cannot grow.
        assert!(release_policy(Some(TEAM), Some(&"a".repeat(155)), None).is_err());
        for name in ["", ".", "..", "a/b", "a b", "a\\b", "é"] {
            assert!(
                release_policy(Some(TEAM), Some(BUNDLE), Some(name)).is_err(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn this_test_build_has_no_signing_configuration() {
        // CI test builds never carry release values; a release build carries both.
        if option_env!("OKILUM_SIGNING_TEAM_ID").is_none()
            && option_env!("OKILUM_BUNDLE_ID").is_none()
        {
            assert_eq!(build_policy().unwrap(), None);
        } else {
            assert!(build_policy().unwrap().is_some());
        }
    }
}
