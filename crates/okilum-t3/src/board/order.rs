//! Deterministic issue ordering within each Board stage (#2535).
//!
//! Within each stage column or group, issues are ordered by:
//! 1. Priority band: `from-oleg`, then `from-tester`, then `bug`, then the rest;
//! 2. Inside each band: issues with an open PR first, then most recently updated first;
//! 3. Deterministic tie-break by issue number descending.
use super::{Issue, Scope};
use std::{cmp::Reverse, collections::BTreeMap};

/// Hint displayed in the Queue stage header.
pub const QUEUE_ORDER_HINT: &str = "от Олега сначала";

/// Priority band within a stage column or list group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PriorityBand {
    FromOleg,
    FromTester,
    Bug,
    Rest,
}

/// Total ordering key for an issue inside a stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IssueSortKey {
    pub band: PriorityBand,
    pub no_open_pr: bool,
    pub updated_at_desc: Reverse<Option<i64>>,
    pub number_desc: Reverse<u64>,
}

pub fn priority_band(issue: Option<&Issue>) -> PriorityBand {
    let Some(issue) = issue else {
        return PriorityBand::Rest;
    };
    if issue.has_label("from-oleg") {
        PriorityBand::FromOleg
    } else if issue.has_label("from-tester") {
        PriorityBand::FromTester
    } else if issue.has_label("bug") {
        PriorityBand::Bug
    } else {
        PriorityBand::Rest
    }
}

pub fn has_open_pr(scope: &Scope, issue: Option<&Issue>) -> bool {
    match issue {
        Some(issue) => {
            if !issue.open_pull_requests.is_empty()
                || issue.live_pulls.iter().any(|pull| pull.merged_at.is_none())
            {
                true
            } else if !issue.live_pulls.is_empty() {
                false
            } else {
                !scope.pull_requests.is_empty() && issue.state != "closed"
            }
        }
        None => !scope.pull_requests.is_empty(),
    }
}

pub fn updated_at_timestamp(issue: Option<&Issue>) -> Option<i64> {
    let raw = issue?.updated_at.trim();
    if raw.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.timestamp_millis())
        .ok()
        .or_else(|| {
            raw.parse::<i64>()
                .ok()
                .map(|secs| secs.saturating_mul(1000))
        })
}

pub fn issue_sort_key(scope: &Scope, issue: Option<&Issue>) -> IssueSortKey {
    IssueSortKey {
        band: priority_band(issue),
        no_open_pr: !has_open_pr(scope, issue),
        updated_at_desc: Reverse(updated_at_timestamp(issue)),
        number_desc: Reverse(scope.number),
    }
}

pub fn sort_scopes(scopes: &mut [Scope], issues: &BTreeMap<u64, Issue>) {
    scopes.sort_by_key(|scope| issue_sort_key(scope, issues.get(&scope.number)));
}

pub fn sort_scope_refs<'a>(scopes: &mut [&'a Scope], issues: &BTreeMap<u64, Issue>) {
    scopes.sort_by_key(|scope| issue_sort_key(scope, issues.get(&scope.number)));
}

pub fn sorted_scope_refs<'a>(
    scopes: impl IntoIterator<Item = &'a Scope>,
    issues: &BTreeMap<u64, Issue>,
) -> Vec<&'a Scope> {
    let mut ordered: Vec<&'a Scope> = scopes.into_iter().collect();
    sort_scope_refs(&mut ordered, issues);
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{live::Pull, IssueLabel};

    fn make_scope(number: u64) -> Scope {
        Scope {
            number,
            pinned: false,
            expected: format!("Issue {number}"),
            executor: "UI".into(),
            current_task: false,
            agent_thread_id: None,
            stage: None,
            stuck: false,
            notes: Vec::new(),
            pull_requests: Vec::new(),
            builds: Vec::new(),
            execution: None,
        }
    }

    fn make_issue(
        number: u64,
        labels: &[&str],
        open_prs: &[u64],
        updated_at: &str,
    ) -> (u64, Issue) {
        (
            number,
            Issue {
                number,
                title: format!("Issue {number}"),
                state: "open".into(),
                body: String::new(),
                labels: labels
                    .iter()
                    .map(|name| IssueLabel {
                        name: (*name).into(),
                    })
                    .collect(),
                updated_at: updated_at.into(),
                open_pull_requests: open_prs.to_vec(),
                live_pulls: open_prs
                    .iter()
                    .map(|&pr| Pull {
                        number: pr,
                        merged_at: None,
                        checks: "success".into(),
                    })
                    .collect(),
            },
        )
    }

    #[test]
    fn bands_order_from_oleg_then_tester_then_bug_then_rest() {
        let issues = BTreeMap::from([
            make_issue(10, &["task"], &[], "2026-11-03T20:00:00Z"),
            make_issue(20, &["bug"], &[], "2026-11-03T19:00:00Z"),
            make_issue(30, &["from-tester"], &[], "2026-11-03T18:00:00Z"),
            make_issue(40, &["from-oleg"], &[], "2026-11-03T17:00:00Z"),
            make_issue(50, &["from-oleg", "bug"], &[], "2026-11-03T16:00:00Z"),
            make_issue(60, &["from-tester", "bug"], &[], "2026-11-03T15:00:00Z"),
        ]);
        let mut scopes: Vec<Scope> = [10, 20, 30, 40, 50, 60]
            .into_iter()
            .map(make_scope)
            .collect();
        sort_scopes(&mut scopes, &issues);
        let ordered: Vec<u64> = scopes.iter().map(|s| s.number).collect();
        assert_eq!(ordered, vec![40, 50, 30, 60, 20, 10]);
    }

    #[test]
    fn open_pr_beats_without_pr_inside_each_band_and_ignores_merged_pr() {
        let mut merged_issue = make_issue(25, &["from-tester"], &[], "2026-11-03T23:00:00Z").1;
        merged_issue.live_pulls.push(Pull {
            number: 99,
            merged_at: Some(1_700_000_000),
            checks: "success".into(),
        });
        let issues = BTreeMap::from([
            make_issue(11, &["from-oleg"], &[], "2026-11-03T22:00:00Z"),
            make_issue(12, &["from-oleg"], &[101], "2026-11-03T10:00:00Z"),
            (25, merged_issue),
            make_issue(26, &["from-tester"], &[102], "2026-11-03T09:00:00Z"),
            make_issue(31, &["bug"], &[], "2026-11-03T22:00:00Z"),
            make_issue(32, &["bug"], &[103], "2026-11-03T08:00:00Z"),
            make_issue(41, &["fr"], &[], "2026-11-03T22:00:00Z"),
            make_issue(42, &["fr"], &[104], "2026-11-03T07:00:00Z"),
        ]);
        let mut scopes: Vec<Scope> = [11, 12, 25, 26, 31, 32, 41, 42]
            .into_iter()
            .map(|number| {
                let mut scope = make_scope(number);
                if number == 25 {
                    scope.pull_requests = vec![99];
                }
                scope
            })
            .collect();
        sort_scopes(&mut scopes, &issues);
        let ordered: Vec<u64> = scopes.iter().map(|s| s.number).collect();
        assert_eq!(ordered, vec![12, 11, 26, 25, 32, 31, 42, 41]);
    }

    #[test]
    fn most_recently_updated_comes_first_within_band_and_pr_state() {
        let issues = BTreeMap::from([
            make_issue(101, &["bug"], &[], "2026-11-03T10:00:00Z"),
            make_issue(102, &["bug"], &[], "2026-11-03T12:30:00Z"),
            make_issue(103, &["bug"], &[], "2026-11-03T14:30:00+03:00"), // 11:30Z
            make_issue(104, &["bug"], &[], ""),
        ]);
        let mut scopes: Vec<Scope> = [101, 102, 103, 104].into_iter().map(make_scope).collect();
        sort_scopes(&mut scopes, &issues);
        let ordered: Vec<u64> = scopes.iter().map(|s| s.number).collect();
        assert_eq!(ordered, vec![102, 103, 101, 104]);
    }

    #[test]
    fn tie_breaks_by_issue_number_descending() {
        let issues = BTreeMap::from([
            make_issue(201, &["from-oleg"], &[], "2026-11-03T12:00:00Z"),
            make_issue(205, &["from-oleg"], &[], "2026-11-03T12:00:00Z"),
            make_issue(203, &["from-oleg"], &[], "2026-11-03T12:00:00Z"),
        ]);
        let mut scopes: Vec<Scope> = [201, 205, 203].into_iter().map(make_scope).collect();
        sort_scopes(&mut scopes, &issues);
        let ordered: Vec<u64> = scopes.iter().map(|s| s.number).collect();
        assert_eq!(ordered, vec![205, 203, 201]);
    }

    #[test]
    fn stability_across_refreshes_and_unrelated_issue_changes() {
        let base_issues = BTreeMap::from([
            make_issue(1, &["from-oleg"], &[10], "2026-11-03T10:00:00Z"),
            make_issue(2, &["from-oleg"], &[], "2026-11-03T11:00:00Z"),
            make_issue(3, &["from-tester"], &[], "2026-11-03T09:00:00Z"),
            make_issue(4, &["bug"], &[11], "2026-11-03T08:00:00Z"),
            make_issue(5, &["bug"], &[], "2026-11-03T12:00:00Z"),
            make_issue(6, &[], &[], "2026-11-03T07:00:00Z"),
            make_issue(7, &[], &[], "2026-11-03T07:00:00Z"),
        ]);
        let forward: Vec<Scope> = (1..=7).map(make_scope).collect();
        let reverse: Vec<Scope> = (1..=7).rev().map(make_scope).collect();
        let rotated: Vec<Scope> = [4, 7, 2, 5, 1, 6, 3].into_iter().map(make_scope).collect();

        let order_forward: Vec<u64> = sorted_scope_refs(&forward, &base_issues)
            .into_iter()
            .map(|s| s.number)
            .collect();
        let order_reverse: Vec<u64> = sorted_scope_refs(&reverse, &base_issues)
            .into_iter()
            .map(|s| s.number)
            .collect();
        let order_rotated: Vec<u64> = sorted_scope_refs(&rotated, &base_issues)
            .into_iter()
            .map(|s| s.number)
            .collect();

        assert_eq!(order_forward, vec![1, 2, 3, 4, 5, 7, 6]);
        assert_eq!(order_reverse, order_forward);
        assert_eq!(order_rotated, order_forward);

        // Mutate issue #6 (gives it an open PR and a new label) and verify all other
        // issues keep their exact relative order.
        let mut updated_issues = base_issues.clone();
        updated_issues.insert(
            6,
            make_issue(6, &["from-tester"], &[12], "2026-11-03T20:00:00Z").1,
        );
        let order_after: Vec<u64> = sorted_scope_refs(&forward, &updated_issues)
            .into_iter()
            .map(|s| s.number)
            .collect();
        let without_six_before: Vec<u64> = order_forward.into_iter().filter(|&n| n != 6).collect();
        let without_six_after: Vec<u64> = order_after.into_iter().filter(|&n| n != 6).collect();
        assert_eq!(without_six_before, without_six_after);
    }
}
