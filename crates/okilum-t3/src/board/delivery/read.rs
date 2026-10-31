use super::*;
use serde_json::Value;
use std::time::{Duration, Instant};

fn json(bytes: &[u8]) -> Result<Value> {
    Ok(serde_json::from_slice(bytes)?)
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("Missing {key}"))
}
fn number(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .with_context(|| format!("Missing {key}"))
}
fn valid(build: Build, feed: &Feed) -> Result<Build> {
    ensure!(
        (build.platform, build.channel) == (feed.platform, feed.channel),
        "Conflicting feed platform/channel"
    );
    ensure!(
        sha(&build.source) && !build.build.is_empty() && https_link(&build.artifact),
        "Feed lacks exact source/build/artifact identity"
    );
    Ok(build)
}
/// Parsing is independent of fetch, so fixtures exercise real publisher formats.
pub(super) fn parse(
    feed: &Feed,
    bytes: &[u8],
    fetch: &mut impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<Build> {
    let build = match feed.format {
        Format::Actions => {
            let run = json(bytes)?;
            let id = number(&run, "id")?;
            ensure!(
                feed.url
                    .trim_end_matches('/')
                    .ends_with(&format!("/actions/runs/{id}")),
                "Actions binding must name its exact API run ID"
            );
            let artifacts = json(&fetch(&format!(
                "{}/artifacts",
                feed.url.trim_end_matches('/')
            ))?)?;
            let name = feed
                .artifact_name
                .as_deref()
                .context("Platform artifact name absent")?;
            let entries = artifacts
                .get("artifacts")
                .and_then(Value::as_array)
                .context("Actions artifact list absent")?;
            let matches: Vec<_> = entries
                .iter()
                .filter(|a| a.get("name").and_then(Value::as_str) == Some(name))
                .collect();
            ensure!(matches.len() == 1, "Platform artifact missing or ambiguous");
            let artifact = matches[0];
            ensure!(
                artifact.get("expired").and_then(Value::as_bool) == Some(false)
                    && number(artifact, "size_in_bytes")? > 0,
                "Platform artifact expired or empty"
            );
            if let Some(source) = artifact
                .pointer("/workflow_run/head_sha")
                .and_then(Value::as_str)
            {
                ensure!(
                    source == text(&run, "commit_sha")?,
                    "Artifact/run source conflict"
                );
            }
            Build {
                platform: feed.platform,
                channel: feed.channel,
                build: format!("run-{}", number(&run, "index_in_repo")?),
                source: text(&run, "commit_sha")?.into(),
                artifact: text(artifact, "archive_download_url")?.into(),
            }
        }
        Format::Receipt => serde_json::from_slice(bytes)?,
        Format::Appimage => {
            let value = json(bytes)?;
            ensure!(
                text(&value, "platform")? == "linux-appimage",
                "Not an AppImage descriptor"
            );
            ensure!(
                text(&value, "channel")? == feed.channel.label().to_lowercase(),
                "Descriptor channel conflict"
            );
            ensure!(
                feed.platform == Platform::Linux,
                "AppImage is a Linux artifact"
            );
            let base = url::Url::parse(&feed.url)?;
            let key = text(&value, "key")?;
            ensure!(
                !key.starts_with('/') && !key.split('/').any(|p| p == ".."),
                "Unsafe artifact key"
            );
            let artifact = format!("{}/{}", base.origin().ascii_serialization(), key);
            Build {
                platform: feed.platform,
                channel: feed.channel,
                build: number(&value, "build")?.to_string(),
                source: text(&value, "source")?.into(),
                artifact,
            }
        }
        Format::Appcast => {
            ensure!(feed.platform == Platform::Macos, "Appcast is a Mac feed");
            let doc = roxmltree::Document::parse(std::str::from_utf8(bytes)?)?;
            let sparkle = "http://www.andymatuschak.org/xml-namespaces/sparkle";
            let metadata = "https://git.oklabs.uk/BeFeast/okilum/update-metadata";
            let mut choices = Vec::new();
            for item in doc.descendants().filter(|n| n.has_tag_name("item")) {
                let field = |space, name| {
                    item.children()
                        .find(|n| n.has_tag_name((space, name)))
                        .and_then(|n| n.text())
                };
                if field(sparkle, "channel") != Some(feed.channel.label().to_lowercase().as_str()) {
                    continue;
                }
                let version: u64 = field(sparkle, "version")
                    .context("Appcast build absent")?
                    .parse()?;
                let source = field(metadata, "source").context("Appcast source absent")?;
                let artifact = item
                    .children()
                    .find(|n| n.has_tag_name("enclosure"))
                    .and_then(|n| n.attribute("url"))
                    .context("Appcast artifact absent")?;
                choices.push((version, source.to_owned(), artifact.to_owned()));
            }
            choices.sort();
            let (version, source, artifact) = choices
                .pop()
                .context("No build in selected appcast channel")?;
            ensure!(
                !choices
                    .iter()
                    .any(|(n, s, a)| *n == version && (*s != source || *a != artifact)),
                "Conflicting appcast build identity"
            );
            Build {
                platform: feed.platform,
                channel: feed.channel,
                build: version.to_string(),
                source,
                artifact,
            }
        }
        Format::Velopack => {
            ensure!(
                feed.platform == Platform::Windows,
                "Velopack is a Windows feed"
            );
            let value = json(bytes)?;
            let assets = value
                .get("Assets")
                .and_then(Value::as_array)
                .context("Velopack Assets absent")?;
            let mut choices = Vec::new();
            for asset in assets
                .iter()
                .filter(|v| v.get("Type").and_then(Value::as_str) == Some("Full"))
            {
                ensure!(
                    text(asset, "PackageId")? == "BeFeast.Okilum",
                    "Unexpected package identity"
                );
                let version = text(asset, "Version")?;
                let build: u64 = version
                    .strip_prefix("0.1.")
                    .context("Unsupported package version")?
                    .parse()?;
                choices.push((build, asset));
            }
            choices.sort_by_key(|v| v.0);
            let (build, asset) = choices.pop().context("No full Windows package")?;
            ensure!(
                !choices.iter().any(|(n, a)| *n == build && *a != asset),
                "Conflicting full Windows packages"
            );
            let url = feed
                .metadata
                .as_ref()
                .context("Archive metadata binding absent")?
                .replace("{build}", &build.to_string());
            let archived = json(&fetch(&url)?)?;
            ensure!(
                number(&archived, "build")? == build
                    && text(&archived, "version")? == text(asset, "Version")?,
                "Channel/archive build conflict"
            );
            ensure!(
                archived
                    .pointer("/feed/Assets")
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.contains(asset)),
                "Channel/archive artifact conflict"
            );
            let filename = text(asset, "FileName")?;
            ensure!(
                !filename.is_empty()
                    && filename
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)),
                "Unsafe package filename"
            );
            let artifact = url::Url::parse(&feed.url)?.join(filename)?.to_string();
            Build {
                platform: feed.platform,
                channel: feed.channel,
                build: build.to_string(),
                source: text(&archived, "source")?.into(),
                artifact,
            }
        }
    };
    valid(build, feed)
}
fn repository(binding: &Binding) -> Result<String> {
    let url = url::Url::parse(&binding.repository)?;
    ensure!(
        url.host_str() != Some("github.com") && url.host_str() != Some("api.github.com"),
        "Board uses Forgejo, not GitHub API"
    );
    Ok(format!(
        "{}/api/v1/repos{}",
        url.origin().ascii_serialization(),
        url.path().trim_end_matches('/')
    ))
}
pub(super) fn read_ci(
    base: &str,
    pr: u64,
    builds: &[Build],
    fetch: &mut impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<Ci> {
    let pull = json(&fetch(&format!("{base}/pulls/{pr}"))?)?;
    let source = pull
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .context("PR head source absent")?;
    ensure!(sha(source), "Invalid PR head source");
    let mut sources = vec![source];
    if pull.get("merged").and_then(Value::as_bool) == Some(true) {
        let merged = text(&pull, "merge_commit_sha")?;
        ensure!(sha(merged), "Invalid PR merge source");
        if merged != source {
            sources.push(merged);
        }
    }
    let status = json(&fetch(&format!("{base}/commits/{source}/status"))?)?;
    let state = text(&status, "state")?.to_owned();
    let statuses = status
        .get("statuses")
        .and_then(Value::as_array)
        .context("CI statuses absent")?;
    let mut links = Vec::new();
    let origin = url::Url::parse(base)?;
    // Status URLs lead to the exact Actions job. Do not query a GitHub bridge.
    for value in statuses {
        if let Some(target) = value.get("target_url").and_then(Value::as_str) {
            if let Ok(url) = origin.join(target) {
                if url.origin() == origin.origin() && url.path().contains("/actions/runs/") {
                    links.push(url.to_string());
                }
            }
        }
    }
    links.sort();
    links.dedup();
    let mut ci = Ci {
        pr,
        source: source.into(),
        state,
        links,
        inclusion: Default::default(),
    };
    for build in builds {
        if ci.inclusion.contains_key(&build.source) {
            continue;
        }
        let mut included = false;
        let mut error = None;
        for source in &sources {
            if build.source == *source {
                included = true;
                break;
            }
            // Reverse comparison: zero commits means this explicit PR source
            // is an ancestor of the exact build, including pre-merge Sprint.
            let result = (|| -> Result<bool> {
                let compare = json(&fetch(&format!(
                    "{base}/compare/{}...{source}",
                    build.source
                ))?)?;
                let total = number(&compare, "total_commits")?;
                let commits = compare
                    .get("commits")
                    .and_then(Value::as_array)
                    .context("Compare commits absent")?;
                ensure!(
                    total != 0 || commits.is_empty(),
                    "Inconsistent ancestry response"
                );
                Ok(total == 0)
            })();
            match result {
                Ok(true) => {
                    included = true;
                    break;
                }
                Ok(false) => {}
                Err(e) => error = Some(format!("Source inclusion unavailable: {e:#}")),
            }
        }
        let proof = if included {
            Ok(true)
        } else if let Some(error) = error {
            Err(error)
        } else {
            Ok(false)
        };
        ci.inclusion.insert(build.source.clone(), proof);
    }
    Ok(ci)
}
pub fn refresh(area: &Path, binding: &Binding) -> Update {
    let config = config(area).map_err(|e| format!("ops/board-delivery.toml: {e:#}"));
    let mut out = Update {
        config,
        feeds: Vec::new(),
        artifacts: Vec::new(),
        ci: Vec::new(),
        evidence: Vec::new(),
    };
    let Some(config) = out.config.as_ref().ok().and_then(|v| v.as_ref()).cloned() else {
        return out;
    };
    for note in &config.evidence {
        out.evidence.push((
            note.clone(),
            evidence(area, note).map_err(|e| format!("{note}: {e:#}")),
        ));
    }
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Okilum-Board-Delivery/1")
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            out.config = Err(format!("Delivery reader: {e:#}"));
            return out;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut fetch = |url: &str| -> Result<Vec<u8>> {
        ensure!(
            https_link(url) && url::Url::parse(url)?.host_str() != Some("api.github.com"),
            "Invalid public delivery source"
        );
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("Delivery read deadline")?;
        let response = client
            .get(url)
            .timeout(remaining.min(Duration::from_secs(5)))
            .send()?
            .error_for_status()?;
        ensure!(
            response.status() == reqwest::StatusCode::OK,
            "Delivery source did not return metadata"
        );
        bounded(response)
    };
    let mut builds = Vec::new();
    for feed in &config.feeds {
        let result = fetch(&feed.url).and_then(|bytes| parse(feed, &bytes, &mut fetch));
        if let Ok(build) = &result {
            builds.push(build.clone());
        }
        out.feeds.push((
            (feed.platform, feed.channel),
            result.map(Some).map_err(|e| {
                format!(
                    "{} {} feed: {e:#}",
                    feed.platform.label(),
                    feed.channel.label()
                )
            }),
        ));
    }
    for build in &builds {
        let result = (|| -> Result<Artifact> {
            ensure!(
                url::Url::parse(&build.artifact)?.host_str() != Some("api.github.com"),
                "GitHub API is not an artifact source"
            );
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("Artifact availability deadline")?;
            let response = client
                .head(&build.artifact)
                .timeout(remaining.min(Duration::from_secs(5)))
                .send()?
                .error_for_status()?;
            ensure!(
                response.status() == reqwest::StatusCode::OK,
                "Artifact availability unconfirmed (redirect or partial response)"
            );
            Ok(Artifact { available: true })
        })();
        out.artifacts.push((
            build.artifact.clone(),
            result.map_err(|e| format!("Artifact: {e:#}")),
        ));
    }
    let base = repository(binding);
    let prs: std::collections::BTreeSet<_> = binding
        .issues
        .iter()
        .flat_map(|scope| scope.pull_requests.iter().copied())
        .collect();
    for pr in prs {
        let result = match &base {
            Ok(base) => read_ci(base, pr, &builds, &mut fetch),
            Err(error) => Err(anyhow::anyhow!("{error:#}")),
        };
        out.ci
            .push((pr, result.map_err(|e| format!("PR #{pr}: {e:#}"))));
    }
    out
}
