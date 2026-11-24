//! Descriptor fields shared by vault readers and write admission.
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::{fmt, fs, io, path::Path};

pub const DIRECTORY: &str = ".okilum";
pub const FILE: &str = "vault.json";
pub const ARCHIVE_MARKER: &str = ".brain-read-only";

/// Require real metadata so a missing file cannot mask a broken symlink.
pub fn read_bytes(root: &Path) -> io::Result<Option<Vec<u8>>> {
    let directory = root.join(DIRECTORY);
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(io::Error::other("Invalid vault metadata directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let file = directory.join(FILE);
    match fs::symlink_metadata(&file) {
        Ok(metadata) if metadata.is_file() => fs::read(file).map(Some),
        Ok(_) => Err(io::Error::other("Invalid vault descriptor file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Parse JSON without silently accepting conflicting members, including in
/// unknown fields whose meaning a newer writer may understand.
pub fn parse(bytes: &[u8]) -> serde_json::Result<Value> {
    serde_json::from_slice::<UniqueValue>(bytes).map(|value| value.0)
}

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(UniqueVisitor).map(Self)
    }
}

struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value with unique object keys")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(value.into())
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(value.into())
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(value.into())
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Ok(value.into())
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(value.into())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut entries: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(UniqueValue(value)) = entries.next_element()? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some((key, UniqueValue(value))) = entries.next_entry::<String, UniqueValue>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate JSON key {key:?}")));
            }
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    Writable,
    Archive,
    Uncertain,
}

impl Lifecycle {
    pub fn load(root: &Path) -> Self {
        match fs::symlink_metadata(root.join(ARCHIVE_MARKER)) {
            Ok(_) => return Self::Archive,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Self::Uncertain,
        }
        let descriptor = match read_bytes(root) {
            Ok(None) => return Self::Writable,
            Ok(Some(bytes)) => parse(&bytes),
            Err(_) => return Self::Uncertain,
        };
        let Ok(Value::Object(descriptor)) = descriptor else {
            return Self::Uncertain;
        };
        let lifecycle = match descriptor.get("lifecycle") {
            None => return Self::Writable,
            Some(Value::Object(lifecycle)) => lifecycle,
            Some(_) => return Self::Uncertain,
        };
        let state = lifecycle.get("state").and_then(Value::as_str);
        let read_only = lifecycle.get("read_only");
        if state == Some("archive") || read_only == Some(&Value::Bool(true)) {
            Self::Archive
        } else if state == Some("active")
            && matches!(read_only, None | Some(Value::Bool(false)))
            && !lifecycle.contains_key("successor")
            && !lifecycle.contains_key("path_map")
        {
            Self::Writable
        } else {
            Self::Uncertain
        }
    }

    pub(crate) fn check_ancestors(directory: &Path) -> anyhow::Result<()> {
        for root in directory.ancestors() {
            match Self::load(root) {
                Self::Writable => {}
                Self::Archive => {
                    anyhow::bail!("This vault is a read-only archive: {}", root.display())
                }
                Self::Uncertain => anyhow::bail!(
                    "Vault writes are blocked because the lifecycle cannot be verified: {}",
                    root.display()
                ),
            }
        }
        Ok(())
    }
}
