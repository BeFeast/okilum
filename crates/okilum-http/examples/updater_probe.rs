//! Native transport control: fixed production Source or unmodified Velopack HttpSource.
//! Fixture-only URLs; no installer/restart command is executed by this companion.
#[cfg(feature = "updates")]
fn main() -> anyhow::Result<()> {
    use std::{path::PathBuf, time::Duration};
    use velopack::{
        bundle::Manifest,
        sources::{HttpSource, UpdateSource},
    };
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 4,
        "usage: updater_probe <legacy|fixed> <fixture-base> <output>"
    );
    let url = url::Url::parse(&args[2])?;
    anyhow::ensure!(
        matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "updater.okilum.test")
        ),
        "Fixture hosts only"
    );
    let output = PathBuf::from(&args[3]);
    let app = Manifest {
        id: "Okilum.NetworkRegression".into(),
        version: "0.1.1".parse()?,
        ..Default::default()
    };
    let result = (|| -> anyhow::Result<String> {
        let source: Box<dyn UpdateSource> = match args[1].as_str() {
            "fixed" => Box::new(okilum_http::updates::Source::new(
                url.as_str(),
                "0.1.native-updater",
            )?),
            "legacy" => Box::new(HttpSource::new_with_options(
                url.as_str(),
                velopack::HttpOptions {
                    TimeoutMilliseconds: Duration::from_secs(15).as_millis() as u64,
                    ..Default::default()
                },
            )),
            _ => anyhow::bail!("Unknown mode"),
        };
        let feed = source.get_release_feed("stable", &app, "native-fixture")?;
        anyhow::ensure!(feed.Assets.len() == 1, "Expected exactly one fixture asset");
        let path = output.with_extension("package");
        source.download_release_entry(&feed.Assets[0], &path, None)?;
        anyhow::ensure!(
            std::fs::read(path)? == b"isolated-updater-fixture",
            "Unexpected fixture bytes"
        );
        Ok("feed=200 package=200 bytes=verified".into())
    })();
    let report = match &result {
        Ok(s) => format!("PASS {s}\n"),
        Err(e) => format!("FAIL {e:#}\n"),
    };
    std::fs::write(output, &report)?;
    print!("{report}");
    result.map(|_| ())
}
#[cfg(not(feature = "updates"))]
fn main() {
    panic!("Enable feature updates");
}
