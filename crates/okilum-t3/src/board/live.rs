//! Rebuildable repository membership. Manual cards annotate issues; only pins
//! and human stages survive closure without recent merged-PR evidence.
use super::*;
use std::{collections::BTreeSet, time::Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct Pull {
    pub number: u64,
    pub merged_at: Option<u64>,
    pub checks: String,
}
impl Pull {
    pub fn recent(&self, at: u64) -> bool {
        self.merged_at
            .is_some_and(|merged| at.checked_sub(merged).is_some_and(|age| age < 86400))
    }
}
fn timestamp(value: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|date| u64::try_from(date.timestamp()).ok())
}

/// Read only the explicitly bound repository. A partial page is never a new
/// membership snapshot. The shared deadline bounds all requests, not just each.
pub fn read(binding: &Binding) -> Result<Vec<Issue>, String> {
    if binding.repository.is_empty() {
        return Ok(Vec::new());
    }
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let result = (|| -> Result<Vec<Issue>> {
        let repo = repository_url(&binding.repository)?;
        let base = format!(
            "{}/api/v1/repos{}",
            repo.origin().ascii_serialization(),
            repo.path().trim_end_matches('/')
        );
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .user_agent("Okilum-Board/1")
            .build()?;
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut fetch = |path: &str| -> Result<serde_json::Value> {
            ensure!(
                crate::NetworkAccess::current().allowed(),
                "Agents is disabled"
            );
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("Board live read deadline exceeded")?;
            let response = client
                .get(format!("{base}/{path}"))
                .timeout(remaining.min(Duration::from_secs(5)))
                .header("Accept", "application/json")
                .send()?
                .error_for_status()?;
            ensure!(
                response.status() == reqwest::StatusCode::OK,
                "Forgejo did not return a complete response"
            );
            let next = response
                .headers()
                .get("Link")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("rel=\"next\""));
            use std::io::Read;
            let mut bytes = Vec::new();
            response.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 2 * 1024 * 1024,
                "Forgejo response exceeds 2 MiB; not truncated"
            );
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            ensure!(
                !next || value.as_array().is_some_and(|rows| rows.len() == 50),
                "Forgejo returned a short page with more results; previous Board retained"
            );
            Ok(value)
        };
        read_with(binding, &repo, at, &mut fetch)
    })();
    result.map_err(|error| format!("Live Forgejo Board: {error:#}"))
}

fn pages(
    path: &str,
    stop_before: Option<u64>,
    fetch: &mut impl FnMut(&str) -> Result<serde_json::Value>,
) -> Result<Vec<serde_json::Value>> {
    let mut rows = Vec::new();
    for page in 1..=20 {
        let value = fetch(&format!("{path}&limit=50&page={page}"))?;
        let values = value.as_array().context("Forgejo list is not an array")?;
        let old = stop_before.is_some_and(|cutoff| {
            values
                .last()
                .and_then(|v| v["updated_at"].as_str())
                .and_then(timestamp)
                .is_some_and(|at| at < cutoff)
        });
        rows.extend(values.iter().cloned());
        if values.len() < 50 || old {
            return Ok(rows);
        }
    }
    anyhow::bail!("Forgejo list exceeds 1000 entries; previous Board retained")
}

fn read_with(
    binding: &Binding,
    repo: &url::Url,
    at: u64,
    fetch: &mut impl FnMut(&str) -> Result<serde_json::Value>,
) -> Result<Vec<Issue>> {
    let mut issues = BTreeMap::new();
    for value in pages("issues?state=open&type=issues", None, fetch)? {
        if value.get("pull_request").is_some_and(|v| !v.is_null()) {
            continue;
        }
        let issue: Issue = serde_json::from_value(value)?;
        ensure!(
            issue.state == "open",
            "Open issue list returned a non-open issue"
        );
        ensure!(
            issues.insert(issue.number, issue).is_none(),
            "Conflicting issue pagination; previous Board retained"
        );
    }
    for scope in &binding.issues {
        if (scope.pinned || scope.stage.is_some()) && !issues.contains_key(&scope.number) {
            let issue: Issue = serde_json::from_value(fetch(&format!("issues/{}", scope.number))?)?;
            ensure!(
                issue.number == scope.number,
                "Forgejo returned a different pinned issue"
            );
            issues.insert(scope.number, issue);
        }
    }
    let mut pulls = pages("pulls?state=open", None, fetch)?;
    pulls.extend(pages(
        "pulls?state=closed&sort=recentupdate",
        Some(at.saturating_sub(86400)),
        fetch,
    )?);
    let mut seen = BTreeSet::new();
    for value in pulls {
        let pull: PullRequest = serde_json::from_value(value.clone())?;
        ensure!(
            seen.insert(pull.number),
            "Conflicting PR pagination; previous Board retained"
        );
        let merged_at = value["merged_at"].as_str().and_then(timestamp);
        let recent = pull.merged
            && merged_at.is_some_and(|m| at.checked_sub(m).is_some_and(|age| age < 86400));
        let open = pull.state == "open" && !pull.merged;
        if !open && !recent {
            continue;
        }
        // Resolve literal references against this repository, never titles or
        // executor names. Fetch closed referenced issues only for recent merges.
        let mut numbers: BTreeSet<u64> = issues.keys().copied().collect();
        numbers.extend(binding.issues.iter().map(|s| s.number));
        if recent {
            for token in
                format!("{}\n{}", pull.title, pull.body).split(|c: char| !c.is_ascii_digit())
            {
                if let Ok(number) = token.parse::<u64>() {
                    if number > 0 && pull.references(repo, number) {
                        numbers.insert(number);
                    }
                }
            }
        }
        let references: Vec<u64> = numbers
            .into_iter()
            .filter(|n| {
                pull.references(repo, *n)
                    || binding
                        .issues
                        .iter()
                        .any(|s| s.number == *n && s.pull_requests.contains(&pull.number))
            })
            .collect();
        if references.is_empty() {
            continue;
        }
        let checks = if open {
            let sha = value["head"]["sha"]
                .as_str()
                .context("Open PR has no head source")?;
            ensure!(
                !sha.is_empty() && sha.chars().all(|c| c.is_ascii_hexdigit()),
                "Invalid PR source"
            );
            fetch(&format!("commits/{sha}/status"))?["state"]
                .as_str()
                .context("PR checks state missing")?
                .to_owned()
        } else {
            String::new()
        };
        for number in references {
            if !issues.contains_key(&number) && recent {
                let issue: Issue = serde_json::from_value(fetch(&format!("issues/{number}"))?)?;
                ensure!(issue.number == number, "Forgejo returned a different issue");
                issues.insert(number, issue);
            }
            if let Some(issue) = issues.get_mut(&number) {
                if open {
                    issue.open_pull_requests.push(pull.number);
                }
                issue.live_pulls.push(Pull {
                    number: pull.number,
                    merged_at,
                    checks: checks.clone(),
                });
            }
        }
    }
    Ok(issues.into_values().collect())
}

/// Presentation-only copy: never persist generated membership or PR links.
pub fn project(binding: &Binding, issues: &BTreeMap<u64, Issue>, at: u64) -> Binding {
    let mut projected = binding.clone();
    projected.issues = issues
        .values()
        .filter(|issue| {
            issue.state == "open"
                || issue.live_pulls.iter().any(|p| p.recent(at))
                || binding
                    .issues
                    .iter()
                    .any(|s| s.number == issue.number && (s.pinned || s.stage.is_some()))
        })
        .map(|issue| {
            let mut scope = binding
                .issues
                .iter()
                .find(|s| s.number == issue.number)
                .cloned()
                .unwrap_or_else(|| Scope {
                    number: issue.number,
                    pinned: false,
                    expected: issue.title.clone(),
                    executor: "Unassigned".into(),
                    current_task: false,
                    agent_thread_id: None,
                    stage: None,
                    stuck: false,
                    notes: Vec::new(),
                    pull_requests: Vec::new(),
                    builds: Vec::new(),
                    execution: None,
                });
            scope.pull_requests = issue
                .live_pulls
                .iter()
                .filter(|p| p.merged_at.is_none() || p.recent(at))
                .map(|p| p.number)
                .collect();
            scope
        })
        .collect();
    projected
}

pub fn checks(issue: &Issue) -> bool {
    issue
        .live_pulls
        .iter()
        .any(|p| p.merged_at.is_none() && p.checks != "success")
}

#[cfg(test)]
mod tests;
