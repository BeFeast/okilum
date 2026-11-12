use okilum_inbox_domain::{Capture, OwnerId};
use okilum_inboxd::store::Store;
use rusqlite::Connection;
use uuid::Uuid;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::path::PathBuf::from(std::env::var("LEGACY_FIXTURE_OUTPUT")?);
    std::fs::create_dir_all(&output)?;
    let owner = OwnerId(Uuid::from_u128(1));
    let request = Capture {
        operation_id: Uuid::from_u128(2),
        item_id: Uuid::from_u128(3),
        text: "  Legacy capture\r\nOriginal text 📝\n".into(),
    };
    for version in [1, 12] {
        let path = output.join(format!("rusqlite-0.37-schema-{version}.db"));
        Store::open(&path)?.capture(owner, &request, 123)?;
        let db = Connection::open(&path)?;
        if version == 1 {
            db.execute_batch("DROP TABLE execution_outputs; DROP TABLE execution_results; DROP TABLE execution_launches; DROP TABLE execution_replies; DROP TABLE execution_questions; DROP TABLE execution_mutations; DROP TABLE execution_briefs; DROP TABLE execution_projects; DROP TABLE publications; DROP TABLE discussion_turns; DROP TABLE auth_owner; DROP TABLE auth_passkeys; DROP TABLE sync_scopes; DROP TABLE sync_requests; DROP TABLE sync_grants; PRAGMA user_version=1;")?;
        }
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE; VACUUM;")?;
        assert_eq!(db.pragma_query_value::<i64,_>(None,"user_version",|r| r.get(0))?,version);
    }
    println!("SQLite writer version: {}", rusqlite::version());
    Ok(())
}
