//! Create-only publication journal. Payloads and replay identities are canonical.
use crate::store::{Error, Store};
use okilum_inbox_domain::OwnerId;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) const SCHEMA: &str = "CREATE TABLE publications (
 owner_id TEXT NOT NULL, operation_id TEXT NOT NULL, item_id TEXT NOT NULL,
 folder TEXT NOT NULL, filename TEXT NOT NULL, content TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('queued','prepared','published')),
 PRIMARY KEY(owner_id,operation_id),
 FOREIGN KEY(owner_id,item_id) REFERENCES captures(owner_id,item_id)
); PRAGMA user_version=4;";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Publish {
    pub operation_id: Uuid,
    pub folder: String,
    pub filename: String,
    pub content: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Publication {
    #[serde(flatten)]
    pub request: Publish,
    pub item_id: Uuid,
    pub state: String,
    pub conflict: Option<String>,
}
pub fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('.')
        && value.len() <= 200
        && !value
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}
pub fn valid_markdown_path(value: &str) -> bool {
    value.len() <= 1000
        && value.ends_with(".md")
        && value.split('/').count() <= 10
        && value.split('/').all(valid_component)
}
impl Store {
    pub fn mark_publication_conflict(
        &mut self,
        owner: OwnerId,
        operation: Uuid,
    ) -> Result<(), Error> {
        self.connection.execute("UPDATE publications SET conflict='file_exists' WHERE owner_id=?1 AND operation_id=?2 AND state != 'published'", params![owner.0.to_string(), operation.to_string()])?;
        Ok(())
    }
    pub fn publications(&self, owner: OwnerId, item: Uuid) -> Result<Vec<Publication>, Error> {
        if self.item(owner, item)?.is_none() {
            return Err(Error::MissingItem);
        }
        let mut q=self.connection.prepare("SELECT operation_id,folder,filename,content,state,conflict FROM publications WHERE owner_id=?1 AND item_id=?2 ORDER BY rowid")?;
        let rows = q.query_map(params![owner.0.to_string(), item.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
        rows.map(|r| {
            let (id, folder, filename, content, state, conflict) = r?;
            Ok(Publication {
                request: Publish {
                    operation_id: Uuid::parse_str(&id).map_err(|_| Error::InvalidStoredIdentity)?,
                    folder,
                    filename,
                    content,
                },
                item_id: item,
                state,
                conflict,
            })
        })
        .collect()
    }
    pub fn prepare_publication(
        &mut self,
        owner: OwnerId,
        item: Uuid,
        request: &Publish,
    ) -> Result<Publication, Error> {
        if request.operation_id.is_nil()
            || !valid_component(&request.folder)
            || !valid_markdown_path(&request.filename)
            || request.content.trim().is_empty()
            || request.content.len() > 64 * 1024
        {
            return Err(Error::InvalidPublication);
        }
        if self.item(owner, item)?.is_none() {
            return Err(Error::MissingItem);
        }
        let old: Option<String> = self
            .connection
            .query_row(
                "SELECT item_id FROM publications WHERE owner_id=?1 AND operation_id=?2",
                params![owner.0.to_string(), request.operation_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            if old != item.to_string() {
                return Err(Error::OperationConflict);
            }
            let publication = self
                .publications(owner, item)?
                .into_iter()
                .find(|p| p.request.operation_id == request.operation_id)
                .ok_or(Error::InvalidStoredIdentity)?;
            if publication.request != *request {
                return Err(Error::OperationConflict);
            }
            return Ok(publication);
        }
        self.connection.execute("INSERT INTO publications(owner_id,operation_id,item_id,folder,filename,content,state) VALUES (?1,?2,?3,?4,?5,?6,'queued')",params![owner.0.to_string(),request.operation_id.to_string(),item.to_string(),request.folder,request.filename,request.content])?;
        Ok(Publication {
            request: request.clone(),
            item_id: item,
            state: "queued".into(),
            conflict: None,
        })
    }
    pub fn advance_publication(
        &mut self,
        owner: OwnerId,
        operation: Uuid,
        from: &str,
        to: &str,
    ) -> Result<(), Error> {
        if !matches!(
            (from, to),
            ("queued", "prepared") | ("prepared", "published")
        ) {
            return Err(Error::InvalidPublication);
        }
        let count = self.connection.execute(
            "UPDATE publications SET state=?1 WHERE owner_id=?2 AND operation_id=?3 AND state=?4",
            params![to, owner.0.to_string(), operation.to_string(), from],
        )?;
        if count != 1 {
            return Err(Error::InvalidPublication);
        }
        Ok(())
    }
}
