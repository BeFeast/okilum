//! Stable, human-meaningful order of issues inside one stage/column.
//!
//! The same ordering drives both the List and Board views, so it lives here in
//! the shared board model next to column classification. It is a pure function
//! of each issue's labels, open-PR state and Forgejo `updated_at` (parsed once per
//! comparison; an empty or unparsable value sorts after a real timestamp); the same input always
//! produces the same order, and an unrelated issue change never moves an issue.
use super::Issue;
use std::cmp::Ordering;

/// Within-stage priority band: Oleg's own requests first, then tester requests,
/// then bugs, then everything else. Only explicit labels assign a band; a missing
/// label never invents one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Band {
    FromOleg,
    FromTester,
    Bug,
    Rest,
}

fn band(issue: &Issue) -> Band {
    if issue.has_label("from-oleg") {
        Band::FromOleg
    } else if issue.has_label("from-tester") {
        Band::FromTester
    } else if issue.has_label("bug") {
        Band::Bug
    } else {
        Band::Rest
    }
}

/// Unix time of the issue's last Forgejo update; `None` for a missing value.
fn recency(issue: &Issue) -> Option<u64> {
    super::live::timestamp(&issue.updated_at)
}

/// Compare two issues for stable ordering inside a stage/column:
///
/// 1. band first: `from-oleg`, then `from-tester`, then `bug`, then the rest;
/// 2. inside a band: issues with an open PR first, then most recently updated;
/// 3. stable tie-break by descending issue number.
pub fn compare(a: &Issue, b: &Issue) -> Ordering {
    band(a)
        .cmp(&band(b))
        .then_with(|| {
            a.open_pull_requests
                .is_empty()
                .cmp(&b.open_pull_requests.is_empty())
        })
        .then_with(|| recency(b).cmp(&recency(a)))
        .then_with(|| b.number.cmp(&a.number))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::IssueLabel;

    fn issue(number: u64, labels: &[&str], open_pr: bool, updated: Option<u64>) -> Issue {
        Issue {
            number,
            title: String::new(),
            state: "open".into(),
            body: String::new(),
            labels: labels
                .iter()
                .map(|name| IssueLabel {
                    name: (*name).to_owned(),
                })
                .collect(),
            open_pull_requests: if open_pr { vec![1] } else { vec![] },
            live_pulls: vec![],
            updated_at: updated.map(rfc3339).unwrap_or_default(),
            report: Default::default(),
        }
    }

    fn rfc3339(seconds: u64) -> String {
        chrono::DateTime::from_timestamp(seconds as i64, 0)
            .unwrap()
            .to_rfc3339()
    }

    fn order(issues: &[Issue]) -> Vec<u64> {
        let mut issues: Vec<&Issue> = issues.iter().collect();
        issues.sort_by(|a, b| compare(a, b));
        issues.into_iter().map(|issue| issue.number).collect()
    }

    #[test]
    fn bands_order_from_oleg_tester_bug_rest() {
        let issues = [
            issue(1, &["bug"], false, None),
            issue(2, &["from-tester"], false, None),
            issue(3, &[], false, None),
            issue(4, &["from-oleg"], false, None),
        ];
        assert_eq!(order(&issues), vec![4, 2, 1, 3]);
    }

    #[test]
    fn open_pr_ranks_first_within_a_band() {
        let issues = [
            issue(1, &["bug"], false, Some(200)),
            issue(2, &["bug"], true, Some(100)),
        ];
        assert_eq!(order(&issues), vec![2, 1]);
    }

    #[test]
    fn most_recently_updated_ranks_first_within_a_band() {
        let issues = [
            issue(1, &["bug"], false, Some(100)),
            issue(2, &["bug"], false, Some(300)),
            issue(3, &["bug"], false, Some(200)),
        ];
        assert_eq!(order(&issues), vec![2, 3, 1]);
    }

    #[test]
    fn missing_updated_at_breaks_before_a_real_timestamp() {
        let issues = [
            issue(1, &["bug"], false, None),
            issue(2, &["bug"], false, Some(300)),
        ];
        assert_eq!(order(&issues), vec![2, 1]);
    }

    #[test]
    fn equal_everything_else_breaks_by_descending_number() {
        let issues = [
            issue(5, &["bug"], false, None),
            issue(3, &["bug"], false, None),
            issue(9, &["bug"], false, None),
        ];
        assert_eq!(order(&issues), vec![9, 5, 3]);
    }

    #[test]
    fn same_input_produces_the_same_order_every_time() {
        let issues = [
            issue(1, &["bug"], false, Some(100)),
            issue(2, &["from-oleg"], true, Some(50)),
            issue(3, &[], false, None),
            issue(4, &["from-tester"], false, Some(200)),
            issue(5, &["bug"], false, Some(100)),
        ];
        let first = order(&issues);
        for _ in 0..5 {
            assert_eq!(order(&issues), first);
        }
    }

    #[test]
    fn an_unrelated_issue_change_never_moves_an_existing_issue() {
        let mut issues = vec![
            issue(1, &["bug"], false, Some(100)),
            issue(2, &["from-oleg"], true, Some(50)),
            issue(3, &[], false, None),
            issue(4, &["from-tester"], false, Some(200)),
        ];
        let before = order(&issues);
        // A refreshed read may drop or add a different issue with a newer
        // timestamp; the relative order of the surviving issues must not jump.
        issues.push(issue(8, &["bug"], false, Some(500)));
        let after = order(&issues);
        let survivors: Vec<u64> = after
            .into_iter()
            .filter(|number| before.contains(number))
            .collect();
        assert_eq!(survivors, before);
    }
}
