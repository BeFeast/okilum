//! Exact archive transfer to the desktop filesystem, independent of backend paths.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{net::SocketAddr, path::Path};

pub fn download(
    endpoint: SocketAddr,
    workspace: &Value,
    destination: &Path,
) -> Result<Value, String> {
    download_with(destination, |body| {
        super::brain::rpc_guarded(endpoint, body, Some(workspace))
    })
}

pub fn download_context(
    endpoint: SocketAddr,
    workspace: Option<&Value>,
    destination: &Path,
    goal_id: &str,
    job_id: &Value,
) -> Result<Value, String> {
    download_prepared_with(
        destination,
        json!({"op":"context_export_prepare","goal_id":goal_id,"job_id":job_id}),
        |body| super::brain::rpc_guarded(endpoint, body, workspace),
    )
}

fn download_with(
    destination: &Path,
    call: impl FnMut(Value) -> Result<Value, String>,
) -> Result<Value, String> {
    download_prepared_with(destination, json!({"op":"export_prepare"}), call)
}

fn download_prepared_with(
    destination: &Path,
    prepare: Value,
    mut call: impl FnMut(Value) -> Result<Value, String>,
) -> Result<Value, String> {
    // Do not prepare a server archive if the chosen local path cannot be published.
    if destination.try_exists().map_err(|e| e.to_string())? {
        return Err("The export destination already exists. Choose a new filename.".into());
    }
    let ready = call(prepare)?;
    let id = ready["export_id"]
        .as_str()
        .ok_or("Export reply has no download identity.")?
        .to_string();
    let result = (|| {
        let bytes = ready["bytes"]
            .as_u64()
            .ok_or("Export reply has no byte length.")?;
        let revision = ready["revision"]
            .as_str()
            .ok_or("Export reply has no checksum.")?;
        okilum_core::export::save_download(destination, bytes, revision, |offset| {
            let chunk = call(json!({"op":"export_chunk","export_id":id,"offset":offset}))
                .map_err(anyhow::Error::msg)?;
            anyhow::ensure!(
                chunk["export_id"] == id && chunk["offset"] == offset,
                "Export chunk identity or offset does not match"
            );
            let decoded = STANDARD.decode(
                chunk["content_base64"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Export chunk has no bytes"))?,
            )?;
            let next = offset
                .checked_add(decoded.len() as u64)
                .ok_or_else(|| anyhow::anyhow!("Export offset overflow"))?;
            anyhow::ensure!(
                chunk["next_offset"] == next && chunk["eof"] == (next == bytes),
                "Export chunk boundary does not match"
            );
            Ok(decoded)
        })
        .map_err(|e| e.to_string())?;
        Ok(ready.clone())
    })();
    // Releasing download staging never changes canonical knowledge. A lost
    // cleanup reply cannot invalidate an already verified client archive.
    let _ = call(json!({"op":"export_release","export_id":id}));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn corrupted_transport_never_publishes_and_releases_server_archive() {
        let temp =
            std::env::temp_dir().join(format!("okilum-download-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&temp).unwrap();
        let path = temp.join("archive.tar");
        let mut released = false;
        let result = download_with(&path, |request| match request["op"].as_str().unwrap() {
            "export_prepare" => Ok(
                json!({"export_id":"download-1","bytes":3,"revision":"sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"}),
            ),
            "export_chunk" => Ok(
                json!({"export_id":"wrong-download","offset":0,"next_offset":3,"eof":true,"content_base64":"YWJj"}),
            ),
            "export_release" => {
                released = true;
                Ok(Value::Null)
            }
            _ => panic!("unexpected provider operation"),
        });
        assert!(result.is_err());
        assert!(released);
        assert!(!path.exists());
        std::fs::remove_dir_all(temp).unwrap();
    }
    #[test]
    fn valid_download_saves_on_client_and_no_execution_operation_is_sent() {
        let temp =
            std::env::temp_dir().join(format!("okilum-download-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&temp).unwrap();
        let path = temp.join("archive.tar");
        let mut ops = Vec::new();
        download_with(&path, |request| {
            let op = request["op"].as_str().unwrap().to_owned();
            ops.push(op.clone());
            match op.as_str() {
                "export_prepare" => Ok(json!({"export_id":"download-1","bytes":3,"revision":"sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"})),
                "export_chunk" => Ok(json!({"export_id":"download-1","offset":0,"next_offset":3,"eof":true,"content_base64":"YWJj"})),
                "export_release" => Ok(Value::Null),
                _ => panic!("unexpected provider operation"),
            }
        }).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        assert_eq!(
            ops,
            vec!["export_prepare", "export_chunk", "export_release"]
        );
        std::fs::remove_dir_all(temp).unwrap();
    }
    #[test]
    fn context_download_requests_the_selected_job_and_retains_verified_transfer() {
        let temp = std::env::temp_dir().join(format!(
            "okilum-context-download-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&temp).unwrap();
        let path = temp.join("context.tar");
        let mut ops = Vec::new();
        download_prepared_with(&path,json!({"op":"context_export_prepare","goal_id":"selected-goal","job_id":"selected-job"}),|request|{
            let op=request["op"].as_str().unwrap().to_string();ops.push(op.clone());
            match op.as_str(){
                "context_export_prepare"=>{assert_eq!(request["goal_id"],"selected-goal");assert_eq!(request["job_id"],"selected-job");Ok(json!({"export_id":"download","bytes":3,"revision":"sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"}))},
                "export_chunk"=>Ok(json!({"export_id":"download","offset":0,"next_offset":3,"eof":true,"content_base64":"YWJj"})),
                "export_release"=>Ok(Value::Null),_=>panic!("unexpected exact export or execution request"),
            }
        }).unwrap();
        assert_eq!(
            ops,
            vec!["context_export_prepare", "export_chunk", "export_release"]
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        std::fs::remove_dir_all(temp).unwrap();
    }
}
