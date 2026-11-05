//! Rebuildable repository membership. Manual cards annotate issues; only pins
//! and human stages survive closure without recent merged-PR evidence.
use super::*;
use std::{collections::BTreeSet, time::Instant};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pull {
    pub title: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
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
pub(super) fn timestamp(value: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|date| u64::try_from(date.timestamp()).ok())
}

/// A Board-owned, origin-scoped conditional read cache. It contains only
/// rebuildable public API responses; Manager credentials never cross origins.
#[derive(Default)]
pub struct Reader {
    repository: String,
    client: Option<reqwest::blocking::Client>,
    cache: BTreeMap<String, Cached>,
    retry_at: Option<Instant>,
}
struct Cached {
    etag: Option<reqwest::header::HeaderValue>,
    value: serde_json::Value,
    next: bool,
    bytes: usize,
}

/// Keep one reader for the Board session, including hide/show cycles.
pub fn reader() -> impl Fn(&Binding) -> Result<Vec<Issue>, String> + Send + Sync {
    let reader = std::sync::Mutex::new(Reader::default());
    move |binding| {
        reader
            .lock()
            .map_err(|_| "Board reader unavailable".to_owned())?
            .read(binding)
    }
}

/// Stateless compatibility entry point for callers without a Board session.
pub fn read(binding: &Binding) -> Result<Vec<Issue>, String> {
    Reader::default().read(binding)
}
impl Reader {
    /// A failed or partial read never becomes a membership snapshot.
    pub fn read(&mut self, binding: &Binding) -> Result<Vec<Issue>, String> {
        if binding.repository.is_empty() {
            return Ok(Vec::new());
        }
        let result = (|| -> Result<Vec<Issue>> {
            let repo = repository_url(&binding.repository)?;
            let base = format!(
                "{}/api/v1/repos{}",
                repo.origin().ascii_serialization(),
                repo.path().trim_end_matches('/')
            );
            self.read_at(binding, &repo, &base, crate::unix_now())
        })();
        result.map_err(|error| format!("Live Forgejo Board: {error:#}"))
    }
    fn read_at(
        &mut self,
        binding: &Binding,
        repo: &url::Url,
        base: &str,
        at: u64,
    ) -> Result<Vec<Issue>> {
        if self.repository != base {
            self.repository = base.to_owned();
            self.cache.clear();
            self.retry_at = None;
        }
        if let Some(remaining) = self
            .retry_at
            .and_then(|until| until.checked_duration_since(Instant::now()))
        {
            anyhow::bail!(
                "Forgejo requested a retry in {} s; showing last good Board",
                remaining.as_secs() + 1
            );
        }
        if self.client.is_none() {
            self.client = Some(
                reqwest::blocking::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(Duration::from_secs(3))
                    .user_agent("Okilum-Board/1")
                    .build()?,
            );
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut used = BTreeSet::new();
        let issues = read_with(binding, repo, at, &mut |path| {
            used.insert(path.to_owned());
            self.fetch(base, path, deadline)
        })?;
        self.cache.retain(|path, _| used.contains(path));
        Ok(issues)
    }
    fn fetch(&mut self, base: &str, path: &str, deadline: Instant) -> Result<serde_json::Value> {
        ensure!(
            crate::NetworkAccess::current().allowed(),
            "Agents is disabled"
        );
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("Board live read deadline exceeded")?;
        let mut request = self
            .client
            .as_ref()
            .context("Board client unavailable")?
            .get(format!("{base}/{path}"))
            .timeout(remaining.min(Duration::from_secs(5)))
            .header("Accept", "application/json");
        if let Some(etag) = self.cache.get(path).and_then(|entry| entry.etag.as_ref()) {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag.clone());
        }
        let response = request.send()?;
        if matches!(response.status().as_u16(), 429 | 503) {
            let retry = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| {
                    v.parse::<u64>().ok().or_else(|| {
                        chrono::DateTime::parse_from_rfc2822(v)
                            .ok()
                            .and_then(|time| u64::try_from(time.timestamp()).ok())
                            .map(|until| until.saturating_sub(crate::unix_now()))
                    })
                })
                .unwrap_or(60)
                .clamp(1, 86400);
            self.retry_at = Some(Instant::now() + Duration::from_secs(retry));
            anyhow::bail!(
                "Forgejo HTTP {}; retry in {retry} s; showing last good Board",
                response.status().as_u16()
            );
        }
        let (value, next) = if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            let cached = self
                .cache
                .get(path)
                .context("Forgejo returned 304 without a cached response")?;
            (cached.value.clone(), cached.next)
        } else {
            let response = response.error_for_status()?;
            ensure!(
                response.status() == reqwest::StatusCode::OK,
                "Forgejo did not return a complete response"
            );
            let next = response
                .headers()
                .get("Link")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("rel=\"next\""));
            let etag = response.headers().get(reqwest::header::ETAG).cloned();
            use std::io::Read;
            let mut bytes = Vec::new();
            response.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 2 * 1024 * 1024,
                "Forgejo response exceeds 2 MiB; not truncated"
            );
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            // Bound retained pages even across repeated incomplete reads.
            let retained: usize = self
                .cache
                .iter()
                .filter(|(key, _)| key.as_str() != path)
                .map(|(_, entry)| entry.bytes)
                .sum();
            if retained + bytes.len() > 8 * 1024 * 1024
                || (self.cache.len() >= 256 && !self.cache.contains_key(path))
            {
                self.cache.clear();
            }
            self.cache.insert(
                path.to_owned(),
                Cached {
                    etag,
                    value: value.clone(),
                    next,
                    bytes: bytes.len(),
                },
            );
            (value, next)
        };
        ensure!(
            !next || value.as_array().is_some_and(|rows| rows.len() == 50),
            "Forgejo returned a short page with more results; previous Board retained"
        );
        Ok(value)
    }
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
    // The same deadline/cache/refresh owns these bounded reads. Unknown or
    // incomplete comments fail the snapshot rather than inventing "unanswered".
    for issue in issues
        .values_mut()
        .filter(|issue| waiting::recent_report(issue, at))
    {
        let count = issue
            .report
            .comments
            .context("Fresh report has no comment count")?;
        if count == 0 {
            continue;
        }
        let comments = pages(&format!("issues/{}/comments?", issue.number), None, fetch)?;
        ensure!(
            comments.len() as u64 == count,
            "Report comments changed during refresh"
        );
        let filer = issue
            .report
            .user
            .as_ref()
            .and_then(|user| user.id)
            .context("Fresh report has no filing identity")?;
        ensure!(
            comments
                .iter()
                .all(|comment| comment["user"]["id"].as_u64().is_some_and(|id| id > 0)),
            "Report comment has no author identity"
        );
        issue.report.filing_bot_only = comments
            .iter()
            .all(|comment| comment["user"]["id"].as_u64() == Some(filer));
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
                    title: pull.title.clone(),
                    state: if pull.merged {
                        "merged".into()
                    } else {
                        pull.state.clone()
                    },
                    created_at: value["created_at"].as_str().unwrap_or_default().into(),
                    updated_at: value["updated_at"].as_str().unwrap_or_default().into(),
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
    // Build one annotation index. A redraw/refresh must not scan the complete
    // manifest once per live issue; duplicates remain explicitly ambiguous.
    let mut annotations: BTreeMap<u64, Vec<&Scope>> = BTreeMap::new();
    for scope in &binding.issues {
        annotations.entry(scope.number).or_default().push(scope);
    }
    let mut projected = binding.clone();
    projected.issues = issues
        .values()
        .filter(|issue| {
            issue.state == "open"
                || issue.live_pulls.iter().any(|p| p.recent(at))
                || annotations
                    .get(&issue.number)
                    .is_some_and(|scopes| scopes.iter().any(|s| s.pinned || s.stage.is_some()))
        })
        .map(|issue| {
            let matching = annotations
                .get(&issue.number)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut scope = if matching.len() == 1 {
                matching[0].clone()
            } else {
                Scope {
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
                }
            };
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

/// Missing annotation is explicit; a generated card never invents ownership or
/// inherits one of several conflicting manual cards.
pub fn mapping_problem(binding: &Binding, number: u64) -> Option<&'static str> {
    let count = binding
        .issues
        .iter()
        .filter(|scope| scope.number == number)
        .count();
    if count > 1
        || binding
            .card_problems
            .iter()
            .any(|p| p.number == Some(number))
    {
        Some("Card annotation is invalid or ambiguous; live issue shown without manual context")
    } else if count == 0 {
        Some("No card annotation; title and status come from Forgejo")
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
