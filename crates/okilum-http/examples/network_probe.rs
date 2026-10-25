//! Native regression companion: calls the production Feedback client policy.
//! GET-only public health controls, or explicit isolated fixture POST controls.
use anyhow::Result;
use std::{path::PathBuf, time::Duration};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 4,
        "usage: network_probe <health|legacy|fixed> <url> <output-file>"
    );
    let mode = &args[1];
    let endpoint = &args[2];
    let output = PathBuf::from(&args[3]);
    let result = (|| -> Result<String> {
        if mode == "health" {
            anyhow::ensure!(
                matches!(
                    endpoint.as_str(),
                    "https://pomoi.co/healthz" | "https://feedback.befeast.com/healthz"
                ),
                "Health mode permits only the two approved GET health endpoints"
            );
            // Exercise the same async client builder that actual Feedback uses.
            let client =
                okilum_http::async_client(endpoint, "0.1.network-health", Duration::from_secs(45))?;
            return tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let response = client.get(endpoint).send().await?;
                    anyhow::ensure!(
                        response.status() == reqwest::StatusCode::OK,
                        "GET {endpoint} returned HTTP {}",
                        response.status().as_u16()
                    );
                    Ok(format!(
                        "GET {endpoint} status=200 os={} client=production-async",
                        std::env::consts::OS
                    ))
                });
        }
        let url = url::Url::parse(endpoint)?;
        anyhow::ensure!(
            matches!(
                url.host_str(),
                Some("localhost" | "127.0.0.1" | "feedback.okilum.test")
            ),
            "POST probe is restricted to isolated fixture hosts"
        );
        let client = if mode == "legacy" {
            // Bundled roots; fixture clears proxy env vars. Disable the new global
            // system-proxy feature to reproduce the old no-environment case.
            reqwest::blocking::Client::builder()
                .use_rustls_tls()
                .no_proxy()
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
