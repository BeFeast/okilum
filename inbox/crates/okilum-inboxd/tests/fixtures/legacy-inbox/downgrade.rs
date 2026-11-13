// Copy into the old-stack inboxd examples directory; never run with the new stack.
use okilum_inbox_domain::{Capture, OwnerId};
use okilum_inboxd::store::Store;
use uuid::Uuid;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(rusqlite::version(), "3.50.2");
    let path = std::path::PathBuf::from(std::env::var("INBOX_DOWNGRADE_DB")?);
    let owner = OwnerId(Uuid::from_u128(1));
    let pending = Capture { operation_id: Uuid::from_u128(6), item_id: Uuid::from_u128(7), text: "Committed only in the legacy WAL 📝\r\n".into() };
    let mut store = Store::open(&path)?;
    assert_eq!(store.captures(owner, 0, None, 100)?.changes.len(), 3);
    let replay = store.capture(owner, &pending, 999)?;
    assert_eq!(replay.item.original_text, pending.text);
    assert_eq!(replay.item.received_at_ms, 789);
    store.capture(owner, &Capture { operation_id: Uuid::from_u128(10), item_id: Uuid::from_u128(11), text: "Old writer after downgrade".into() }, 1_234)?;
    drop(store);
    let mut store = Store::open(&path)?;
    assert_eq!(store.captures(owner, 0, None, 100)?.changes.len(), 4);
    println!("Old rusqlite 0.37 / SQLite 3.50.2 reopened and wrote the new-stack schema-12 DB; durable WAL replay preserved");
    Ok(())
}
