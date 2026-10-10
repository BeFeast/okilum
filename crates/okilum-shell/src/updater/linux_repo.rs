//! Which pacman repository delivers Okilum updates (#1036). The package does
//! not record where it came from, so read the enabled repository sections of
//! pacman's configuration. Both enabled is reported as such, never guessed.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Beta,
    Stable,
    /// Both repositories are enabled (docs/linux-releases.md says not to).
    Both,
    /// Neither section found: another distribution, a manual install, or
    /// a configuration this check cannot read.
    Unknown,
}

/// Enabled `[okilum-beta]` / `[okilum-stable]` section headers; commented
/// lines do not count.
pub(crate) fn parse(conf: &str) -> Source {
    let (mut beta, mut stable) = (false, false);
    for line in conf.lines().map(str::trim) {
        match line {
            "[okilum-beta]" => beta = true,
            "[okilum-stable]" => stable = true,
            _ => {}
        }
    }
    match (beta, stable) {
        (true, true) => Source::Both,
        (true, false) => Source::Beta,
        (false, true) => Source::Stable,
        (false, false) => Source::Unknown,
    }
}

/// Read once per launch.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn current() -> Source {
    static SOURCE: std::sync::OnceLock<Source> = std::sync::OnceLock::new();
    *SOURCE.get_or_init(|| {
        std::fs::read_to_string("/etc/pacman.conf")
            .map(|conf| parse(&conf))
            .unwrap_or(Source::Unknown)
    })
}

#[cfg(test)]
mod tests {
    use super::{parse, Source};

    #[test]
    fn sections_name_the_repository() {
        let base = "[options]\nHoldPkg = pacman\n\n[core]\nInclude = /etc/pacman.d/mirrorlist\n";
        assert_eq!(
            parse(base),
            Source::Unknown,
            "positive control: none configured"
        );
        let beta = format!(
            "{base}\n[okilum-beta]\nSigLevel = Required DatabaseRequired\n\
             Server = https://updates.befeast.com/okilum/arch/beta/$arch\n"
        );
        assert_eq!(parse(&beta), Source::Beta);
        assert_eq!(
            parse(&beta.replace("okilum-beta", "okilum-stable")),
            Source::Stable
        );
        assert_eq!(
            parse(&format!("{beta}\n  [okilum-stable]  \nServer = x\n")),
            Source::Both,
            "both enabled is reported, not resolved"
        );
        assert_eq!(
            parse(&format!("{base}\n#[okilum-beta]\n#Server = x\n")),
            Source::Unknown,
            "a commented-out section is not enabled"
        );
    }
}
