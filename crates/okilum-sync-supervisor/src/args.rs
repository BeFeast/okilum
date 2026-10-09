//! Closed command line: `--instance <uuid>` and `--state <dir>`, each at most once, and
//! nothing else. The Windows task passes both; the macOS plist passes neither.
use anyhow::{bail, ensure, Context, Result};
use std::{ffi::OsString, path::PathBuf};
use uuid::Uuid;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub instance: Option<Uuid>,
    pub state: Option<PathBuf>,
}

/// `argv` excludes the program name.
pub fn parse(argv: impl IntoIterator<Item = OsString>) -> Result<Args> {
    let mut args = Args::default();
    let mut argv = argv.into_iter();
    while let Some(option) = argv.next() {
        let value = argv.next().context("option is missing its value")?;
        match option.to_str() {
            Some("--instance") => {
                ensure!(args.instance.is_none(), "--instance given twice");
                let text = value.to_str().context("--instance is not valid text")?;
                let id: Uuid = text.parse().context("--instance is not a UUID")?;
                ensure!(!id.is_nil(), "--instance is nil");
                args.instance = Some(id);
            }
            Some("--state") => {
                ensure!(args.state.is_none(), "--state given twice");
                let path = PathBuf::from(value);
                ensure!(path.is_absolute(), "--state must be an absolute path");
                args.state = Some(path);
            }
            _ => bail!("unexpected argument {option:?}"),
        }
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(items: &[&str]) -> Result<Args> {
        parse(items.iter().map(OsString::from))
    }

    #[test]
    fn exactly_the_two_options_are_accepted() {
        let id = "6f1c2d3e-4a5b-4c6d-8e7f-0123456789ab";
        assert_eq!(parse_str(&[]).unwrap(), Args::default());
        let full = parse_str(&["--instance", id, "--state", "/state"]).unwrap();
        assert_eq!(full.instance, Some(id.parse().unwrap()));
        assert_eq!(full.state, Some(PathBuf::from("/state")));
        // Order is free; each option only once.
        assert!(parse_str(&["--state", "/state", "--instance", id]).is_ok());
        for bad in [
            &["--instance", id, "--instance", id][..],
            &["--state", "/a", "--state", "/b"],
            &["--instance"],
            &["--instance", "not-a-uuid"],
            &["--instance", "00000000-0000-0000-0000-000000000000"],
            &["--state", "relative/dir"],
            &["--state", ""],
            &["--config", "/x"],
            &["--instance", id, "extra"],
            &["serve"],
            &["--no-browser", "x"],
            &["--help", "x"],
        ] {
            assert!(parse_str(bad).is_err(), "{bad:?}");
        }
    }
}
