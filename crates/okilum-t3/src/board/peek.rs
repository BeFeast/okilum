//! One bounded, public comments read for the selected issue. No pagination,
//! tracker credentials, Manager RPC, or work for unopened cards.
use super::*;
use std::io::Read;

pub const COMMENT_LIMIT: usize = 12;
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Comment {
    pub created_at: String,
    #[serde(default)]
    pub body: String,
    pub user: Author,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Author {
    pub login: String,
}

pub fn comments(binding: &Binding, number: u64) -> Result<Vec<Comment>, String> {
    let read = || -> Result<Vec<Comment>> {
        ensure!(
            crate::NetworkAccess::current().allowed(),
            "Agents is disabled"
        );
        let repo = repository_url(&binding.repository)?;
        let endpoint = format!(
            "{}/api/v1/repos{}/issues/{number}/comments?limit={COMMENT_LIMIT}&page=1",
            repo.origin().ascii_serialization(),
            repo.path().trim_end_matches('/')
        );
        let response = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .user_agent("Okilum-Board/1")
            .build()?
            .get(endpoint)
            .header("Accept", "application/json")
            .send()?
            .error_for_status()?;
        ensure!(
            response.status() == reqwest::StatusCode::OK,
            "Incomplete comments response"
        );
        let mut bytes = Vec::new();
        response.take(256 * 1024 + 1).read_to_end(&mut bytes)?;
        decode(&bytes)
    };
    read().map_err(|e| format!("Issue comments unavailable: {e:#}"))
}
fn decode(bytes: &[u8]) -> Result<Vec<Comment>> {
    ensure!(bytes.len() <= 256 * 1024, "Comments exceed 256 KiB");
    let mut comments: Vec<Comment> = serde_json::from_slice(bytes)?;
    ensure!(
        comments.len() <= COMMENT_LIMIT,
        "Comments exceed requested bound"
    );
    for comment in &mut comments {
        comment.body = comment.body.chars().take(240).collect();
    }
    Ok(comments)
}
/// Literal same-repository shorthand, never a guessed dependency. Bound work to
/// the description preview and twelve links; cross-repository URLs are excluded.
pub fn mentions(body: &str, own: u64) -> Vec<u64> {
    let preview: String = body.chars().take(6000).collect();
    let mut numbers = std::collections::BTreeSet::new();
    for (at, _) in preview.match_indices('#') {
        if preview[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || "_/.-".contains(c))
        {
            continue;
        }
        let digits = preview[at + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        if digits.is_empty()
            || preview[at + 1 + digits.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        if let Ok(number) = digits.parse::<u64>() {
            if number > 0 && number != own {
                numbers.insert(number);
            }
        }
        if numbers.len() == 12 {
            break;
        }
    }
    numbers.into_iter().collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peek_2531_mentions_are_literal_bounded_and_do_not_invent_dependencies() {
        assert_eq!(
            mentions(
                "Refs #2 and (#3). #2 #1 repo/#4 https://other.test/pull/#5 #6suffix abc#7",
                1
            ),
            vec![2, 3]
        );
        assert_eq!(
            mentions(&(1..=40).map(|n| format!(" #{n}")).collect::<String>(), 99).len(),
            12
        );
        assert!(mentions(&format!("{} #8", "x".repeat(6000)), 1).is_empty());
    }
    #[test]
    fn peek_2531_comments_are_bounded_and_keep_literal_author_and_date() {
        let comment = serde_json::json!({"created_at":"2026-11-04T09:30:00Z", "user":{"login":"owner"}, "body":"gypд".repeat(100)});
        let result =
            decode(&serde_json::to_vec(&vec![comment.clone(); COMMENT_LIMIT]).unwrap()).unwrap();
        assert_eq!(result.len(), COMMENT_LIMIT);
        assert_eq!(result[0].user.login, "owner");
        assert_eq!(result[0].body.chars().count(), 240);
        assert!(decode(&serde_json::to_vec(&vec![comment; COMMENT_LIMIT + 1]).unwrap()).is_err());
        assert!(decode(&vec![b' '; 256 * 1024 + 1]).is_err());
        assert!(decode(b"{}").is_err());
    }
}
