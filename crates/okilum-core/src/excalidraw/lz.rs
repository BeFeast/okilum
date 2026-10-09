//! LZ-string's Base64 wire format (UTF-16 dictionary): bounded decoding, and
//! the `compressToBase64` encoder the Obsidian plugin uses for `compressed-json`.
use anyhow::{bail, ensure, Context, Result};
use std::collections::{HashMap, HashSet};

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

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Packs values LSB-first into 6-bit Base64 characters, as lz-string does.
struct Writer {
    out: String,
    value: u8,
    position: u32,
}
impl Writer {
    fn bit(&mut self, bit: u32) {
        self.value = (self.value << 1) | (bit & 1) as u8;
        if self.position == 5 {
            self.out.push(BASE64[self.value as usize] as char);
            self.position = 0;
            self.value = 0;
        } else {
            self.position += 1;
        }
    }
    fn bits(&mut self, mut value: u32, count: u32) {
        for _ in 0..count {
            self.bit(value & 1);
            value >>= 1;
        }
    }
}

/// Code-width state shared by the encoder's emit steps.
struct Codes {
    width: u32,
    enlarge_in: u32,
}
impl Codes {
    fn grow(&mut self) {
        self.enlarge_in -= 1;
        if self.enlarge_in == 0 {
            self.enlarge_in = 1 << self.width;
            self.width += 1;
        }
    }
}

/// Emits a phrase: a literal the first time a unit is used, its code otherwise.
fn emit(
    out: &mut Writer,
    codes: &mut Codes,
    pending: &mut HashSet<u16>,
    (code, unit): (u32, Option<u16>),
) {
    match unit.filter(|unit| pending.remove(unit)) {
        Some(unit) if unit < 256 => {
            out.bits(0, codes.width);
            out.bits(unit.into(), 8);
            codes.grow();
        }
        Some(unit) => {
            out.bits(1, codes.width);
            out.bits(unit.into(), 16);
            codes.grow();
        }
        None => out.bits(code, codes.width),
    }
    codes.grow();
}

/// `LZString.compressToBase64`, output-identical to lz-string 1.5.
pub(super) fn compress(input: &str) -> String {
    let mut out = Writer {
        out: String::new(),
        value: 0,
        position: 0,
    };
    let mut codes = Codes {
        width: 2,
        enlarge_in: 2,
    };
    // Single units and (prefix code, unit) pairs share one code space.
    let mut chars: HashMap<u16, u32> = HashMap::new();
    let mut pairs: HashMap<(u32, u16), u32> = HashMap::new();
    let mut pending = HashSet::new();
    let mut next_code = 3u32;
    // The current phrase: its code, and its unit when it is a single unit.
    let mut phrase: Option<(u32, Option<u16>)> = None;
    for unit in input.encode_utf16() {
        let unit_code = *chars.entry(unit).or_insert_with(|| {
            pending.insert(unit);
            next_code += 1;
            next_code - 1
        });
        let Some(current) = phrase else {
            phrase = Some((unit_code, Some(unit)));
            continue;
        };
        if let Some(&code) = pairs.get(&(current.0, unit)) {
            phrase = Some((code, None));
            continue;
        }
        emit(&mut out, &mut codes, &mut pending, current);
        pairs.insert((current.0, unit), next_code);
        next_code += 1;
        phrase = Some((unit_code, Some(unit)));
    }
    if let Some(current) = phrase {
        emit(&mut out, &mut codes, &mut pending, current);
    }
    out.bits(2, codes.width);
    // Flush exactly as lz-string does, including its trailing character.
    loop {
        out.value <<= 1;
        if out.position == 5 {
            out.out.push(BASE64[out.value as usize] as char);
            break;
        }
        out.position += 1;
    }
    let padding = (4 - out.out.len() % 4) % 4;
    out.out.extend(std::iter::repeat_n('=', padding));
    out.out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_round_trips_through_the_decoder() {
        let long = "abcabcabd".repeat(5_000);
        for input in [
            "",
            "a",
            "hello hello hello",
            "Привет, мир — 你好 🙂🙂",
            "\u{0}\u{ff}\u{100}\u{ffff}",
            long.as_str(),
        ] {
            let encoded = compress(input);
            assert_eq!(encoded.len() % 4, 0, "{encoded}");
            assert_eq!(decompress(&encoded).unwrap(), input);
        }
    }

    /// The fixture's Drawing block was produced by JS lz-string 1.5.0, as the
    /// Obsidian plugin writes it; the encoder reproduces it exactly.
    #[test]
    fn compress_matches_js_lz_string_output() {
        let file = include_str!("../../tests/fixtures/excalidraw/board.excalidraw.md");
        let body = file
            .split("```compressed-json\n")
            .nth(1)
            .and_then(|rest| rest.split("\n```").next())
            .unwrap();
        let js: String = body.split_whitespace().collect();
        let json = decompress(&js).unwrap();
        assert!(json.starts_with("{\n\t\"type\": \"excalidraw\""));
        assert_eq!(compress(&json), js);
    }
}
