//! Debug-only synthetic server. Build and run on a development/build host.
#[cfg(debug_assertions)]
fn main() -> anyhow::Result<()> {
    use okilum_core::remote_log::fake::{record, FakeKibana, Scenario};
    let mut args = std::env::args().skip(1);
    let address = args.next().unwrap_or_else(|| "127.0.0.1:5609".into());
    anyhow::ensure!(
        address.parse::<std::net::SocketAddr>()?.ip().is_loopback(),
        "Bind only to loopback"
    );
    let mode = args.next().unwrap_or_else(|| "sync".into());
    let scenario = match mode.as_str() {
        "sync" | "investigation" => Scenario::default(),
        "catalog-403" => Scenario { catalog_status: Some(403), ..Default::default() },
        "catalog-404" => Scenario { catalog_status: Some(404), ..Default::default() },
        "catalog-empty" => Scenario { catalog_empty: true, ..Default::default() },
        "catalog-malformed" => Scenario { catalog_malformed: true, ..Default::default() },
        "async" => Scenario {
            async_polls: 3,
            ..Default::default()
        },
        "partial-id" => Scenario {
            async_polls: 3,
            partial_with_id: true,
            ..Default::default()
        },
        "partial" => Scenario {
            partial: true,
            ..Default::default()
        },
        "timeout" => Scenario {
            never_complete: true,
            ..Default::default()
        },
        "legacy" => Scenario {
            legacy_total: true,
            ..Default::default()
        },
        "lower-bound" => Scenario {
            lower_bound: true,
            ..Default::default()
        },
        _ => anyhow::bail!(
            "Scenario: sync | investigation | async | partial-id | partial | timeout | legacy | lower-bound | catalog-403 | catalog-404 | catalog-empty | catalog-malformed"
        ),
    };
    let server = FakeKibana::start(&address, 603, scenario)?;
    println!("Endpoint: {}\nIndex: remote-*:events\nTime: 2026-11-01T00:00:00Z to 2026-11-05T00:00:00Z\nAPI key: test-secret\nSynthetic only; adds one current and one late event every two seconds", server.endpoint);
    if mode == "investigation" {
        server.state.lock().unwrap().records = (0..603)
            .map(okilum_core::remote_log::fake::investigation_record)
            .collect();
    }
    let mut next = 603;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let make = |id| {
            if mode == "investigation" {
                okilum_core::remote_log::fake::investigation_record(id)
            } else {
                record(id)
            }
        };
        let mut current = make(next);
        let now = time::OffsetDateTime::now_utc();
        current["_source"]["@timestamp"] =
            serde_json::json!(now.format(&time::format_description::well_known::Rfc3339)?);
        current["sort"][0] = serde_json::json!(i64::try_from(now.unix_timestamp_nanos())?);
        server.add(current);
        next += 1;
        server.add(make(next));
        next += 1;
    }
}
#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("The fake Kibana server is available only in debug builds");
}
