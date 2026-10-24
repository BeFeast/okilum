//! Native regression companion: calls the production Feedback client policy.
//! Targets only the explicit fixture URL; never sends automatic product feedback.
use anyhow::{Context, Result};
use std::{path::PathBuf, time::Duration};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 4,
        "usage: network_probe <legacy|fixed> <fixture-url> <output-file>"
    );
    let mode = &args[1];
    let endpoint = &args[2];
    let output = PathBuf::from(&args[3]);
    let result = (|| -> Result<String> {
        let client = if mode == "legacy" {
            // Exact pre-fix policy: bundled roots and environment-only proxy discovery.
            reqwest::blocking::Client::builder()
                .use_rustls_tls()
                .timeout(Duration::from_secs(15))
                .build()?
        } else {
            anyhow::ensure!(mode == "fixed", "unknown mode");
            okilum_http::client(endpoint, "0.1.native-regression", Duration::from_secs(15))?
        };
        let response = client
            .post(endpoint)
            .multipart(
                reqwest::blocking::multipart::Form::new()
                    .text("text", "isolated native regression"),
            )
            .send()?
            .error_for_status()?;
        Ok(format!("status={}", response.status().as_u16()))
    })();
    let report = match &result {
        Ok(result) => format!("PASS {result}\n"),
        Err(error) => format!("FAIL {}\nchain: {error:#}\n", okilum_http::reason(error)),
    };
    std::fs::write(output, &report)?;
    print!("{report}");
    result.map(|_| ())
}
