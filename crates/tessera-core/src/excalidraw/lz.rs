//! Bounded decoding of LZ-string's Base64 wire format (UTF-16 dictionary).
use anyhow::{bail, ensure, Context, Result};

const MAX_UNITS: usize = 16 * 1024 * 1024;
const MAX_DICTIONARY_UNITS: usize = 32 * 1024 * 1024;

struct Bits {
    values: Vec<u8>,
    position: usize,
}
impl Bits {
    fn read(&mut self, count: u32) -> Result<usize> {
        ensure!(count <= 24, "Compressed drawing dictionary is too large");
        let mut value = 0;
        for bit in 0..count {
            let source = *self
                .values
                .get(self.position / 6)
                .context("Truncated compressed drawing")?;
            value |= (((source >> (5 - self.position % 6)) & 1) as usize) << bit;
            self.position += 1;
        }
        Ok(value)
    }
}

pub(super) fn decompress(input: &str) -> Result<String> {
    let mut values = Vec::new();
    for ch in input.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if ch == b'=' {
            break;
        }
        values.push(match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => bail!("Invalid compressed drawing Base64"),
        });
    }
    let mut bits = Bits {
        values,
        position: 0,
    };
    let first = match bits.read(2)? {
        0 => bits.read(8)? as u16,
        1 => bits.read(16)? as u16,
        2 => return Ok(String::new()),
        _ => bail!("Invalid compressed drawing header"),
    };
    let mut dict = vec![vec![], vec![], vec![], vec![first]];
    let mut dictionary_units = 1;
    let mut previous = vec![first];
    let mut output = previous.clone();
    let mut width = 3;
    let mut remaining = 4;
    loop {
        let mut code = bits.read(width)?;
        match code {
            0 | 1 => {
                let unit = bits.read(if code == 0 { 8 } else { 16 })? as u16;
                code = dict.len();
                dict.push(vec![unit]);
                dictionary_units += 1;
                remaining -= 1;
            }
            2 => return String::from_utf16(&output).context("Invalid UTF-16 drawing"),
            _ => {}
        }
        if remaining == 0 {
            remaining = 1 << width;
            width += 1;
        }
        let entry = if let Some(entry) = dict.get(code) {
            ensure!(!entry.is_empty(), "Invalid dictionary code");
            entry.clone()
        } else if code == dict.len() {
            let mut entry = previous.clone();
            entry.push(previous[0]);
            entry
        } else {
            bail!("Invalid compressed drawing dictionary reference");
        };
        ensure!(
            output.len() + entry.len() <= MAX_UNITS,
            "Drawing decompression limit exceeded"
        );
        output.extend_from_slice(&entry);
        previous.push(entry[0]);
        dictionary_units += previous.len();
        ensure!(
            dictionary_units <= MAX_DICTIONARY_UNITS && dict.len() < 1_000_000,
            "Drawing dictionary limit exceeded"
        );
        dict.push(previous);
        previous = entry;
        remaining -= 1;
        if remaining == 0 {
            remaining = 1 << width;
            width += 1;
        }
    }
}
