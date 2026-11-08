//! Bounded JSON array projection. Only complete rows enter the preview.
use super::*;
use serde::de::{DeserializeSeed, SeqAccess, Visitor};
use serde_json::Value;

impl Table {
    pub fn read_json(reader: impl Read, limits: ReadLimits) -> io::Result<Self> {
        let mut bytes = Vec::new();
        reader
            .take(limits.bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)?;
        let complete = bytes.len() <= limits.bytes;
        bytes.truncate(limits.bytes);
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
        // A numeric token at our byte boundary could continue in the file.
        // Feed serde only elements followed by a real comma or closing bracket.
        let prefix = (!complete).then(|| complete_array_prefix(bytes));
        let bytes = prefix.as_deref().unwrap_or(bytes);
        let mut table = Table {
            rows: vec![vec![]],
            columns: 0,
            delimiter: '\t',
            malformed: false,
            ragged: false,
            ambiguous: false,
            limited: !complete,
        };
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let mut stopped = false;
        let result = JsonRows {
            table: &mut table,
            limits,
            stopped: &mut stopped,
        }
        .deserialize(&mut deserializer);
        match result {
            Ok(()) if !table.limited => deserializer.end().map_err(invalid)?,
            Ok(()) => {}
            Err(_) if stopped => {}
            Err(error) if error.is_eof() && !complete => {}
            Err(error) => return Err(invalid(error)),
        }
        Ok(table)
    }
}

fn complete_array_prefix(bytes: &[u8]) -> Vec<u8> {
    let Some(start) = bytes.iter().position(|b| !b.is_ascii_whitespace()) else {
        return bytes.to_vec();
    };
    if bytes[start] != b'[' {
        return bytes.to_vec();
    }
    let (mut depth, mut quoted, mut escaped, mut end) = (0usize, false, false, start + 1);
    for (i, &byte) in bytes.iter().enumerate().skip(start) {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'[' | b'{' => depth += 1,
            b']' if depth == 1 => return bytes.to_vec(),
            b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 1 => end = i,
            _ => {}
        }
    }
    let mut prefix = bytes[..end].to_vec();
    prefix.push(b']');
    prefix
}

fn invalid(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

struct JsonRows<'a> {
    table: &'a mut Table,
    limits: ReadLimits,
    stopped: &'a mut bool,
}
impl<'de> DeserializeSeed<'de> for JsonRows<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, de: D) -> Result<(), D::Error> {
        de.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for JsonRows<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON array (use Source for other JSON)")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(value) = seq.next_element::<Value>()? {
            let fields: Vec<(String, String)> = match value {
                Value::Object(object) => object
                    .into_iter()
                    .map(|(key, value)| (key, cell(value)))
                    .collect(),
                Value::Array(values) => values
                    .into_iter()
                    .enumerate()
                    .map(|(i, value)| (format!("Column {}", i + 1), cell(value)))
                    .collect(),
                value => vec![("Value".into(), cell(value))],
            };
            let header = &self.table.rows[0];
            let added = fields
                .iter()
                .filter(|(key, _)| !header.contains(key))
                .count();
            let columns = header.len() + added;
            if columns > self.limits.columns
                || self.table.rows.len() >= self.limits.rows
                || (self.table.rows.len() + 1).saturating_mul(columns) > self.limits.cells
                || fields.iter().any(|(key, value)| {
                    key.len() > self.limits.field_bytes || value.len() > self.limits.field_bytes
                })
            {
                self.table.limited = true;
                *self.stopped = true;
                return Ok(());
            }
            let header = &mut self.table.rows[0];
            for (key, _) in &fields {
                if !header.contains(key) {
                    header.push(key.clone());
                }
            }
            let mut row = vec![String::new(); header.len()];
            for (key, value) in fields {
                row[header.iter().position(|h| *h == key).unwrap()] = value;
            }
            self.table.columns = header.len();
            self.table.rows.push(row);
        }
        Ok(())
    }
}
fn cell(value: Value) -> String {
    match value {
        Value::String(text) => text,
        value => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> ReadLimits {
        ReadLimits {
            bytes: 4096,
            rows: 100,
            cells: 1000,
            columns: 16,
            field_bytes: 512,
        }
    }
    #[test]
    fn json_table_preserves_union_nested_null_missing_and_scalar_values() {
        let source =
            r#"[{"name":"שלום","n":10,"nested":{"a":1}}, {"name":null,"extra":true}, ["x",2],42]"#;
        let table = Table::read_json(source.as_bytes(), limits()).unwrap();
        assert!(!table.limited);
        let at = |key: &str| table.rows[0].iter().position(|h| h == key).unwrap();
        assert_eq!(table.cell(1, at("name")), "שלום");
        assert_eq!(table.cell(2, at("name")), "null");
        assert_eq!(table.cell(1, at("extra")), "");
        assert_eq!(table.cell(1, at("nested")), r#"{"a":1}"#);
        assert_eq!(table.cell(3, at("Column 2")), "2");
        assert_eq!(table.cell(4, at("Value")), "42");
        assert_eq!(
            Table::read_json(b"[]".as_slice(), limits())
                .unwrap()
                .rows
                .len(),
            1
        );
        assert!(Table::read_json(b"{}".as_slice(), limits()).is_err());
        assert!(Table::read_json(b"[1,]".as_slice(), limits()).is_err());
        assert!(Table::read_json(b"[1]garbage".as_slice(), limits()).is_err());
    }
    #[test]
    fn json_table_bounds_rows_columns_cells_fields_and_cut_records() {
        let source = br#"[{"a":1},{"a":2},{"a":"cut"}]"#;
        let table = Table::read_json(
            source.as_slice(),
            ReadLimits {
                bytes: 22,
                ..limits()
            },
        )
        .unwrap();
        assert!(table.limited);
        assert_eq!(table.rows.len(), 3);
        assert_eq!(table.cell(2, 0), "2");
        let table = Table::read_json(
            b"[1,2345]".as_slice(),
            ReadLimits {
                bytes: 5,
                ..limits()
            },
        )
        .unwrap();
        assert_eq!(
            table.rows,
            vec![vec!["Value"], vec!["1"]],
            "a cut numeric token never becomes a false value"
        );
        for budget in [
            ReadLimits {
                rows: 2,
                ..limits()
            },
            ReadLimits {
                cells: 2,
                ..limits()
            },
        ] {
            let table = Table::read_json(source.as_slice(), budget).unwrap();
            assert!(table.limited);
            assert_eq!(table.rows.len(), 2);
        }
        let table = Table::read_json(
            br#"[{"a":1},{"b":2}]"#.as_slice(),
            ReadLimits {
                columns: 1,
                ..limits()
            },
        )
        .unwrap();
        assert!(table.limited);
        assert_eq!(table.rows.len(), 2);
        let table = Table::read_json(
            br#"["long"]"#.as_slice(),
            ReadLimits {
                field_bytes: 2,
                ..limits()
            },
        )
        .unwrap();
        assert!(table.limited);
        assert_eq!(table.rows.len(), 1);
    }
}
