//! Bounded Discover Rison decoder. URL decoding belongs to the caller, once.
use anyhow::{bail, ensure, Result};
use serde_json::{Map, Value};

pub fn decode(text: &str) -> Result<Value> {
    ensure!(text.len() <= 65_536, "Discover state exceeds 64 KiB");
    let mut parser = Parser {
        chars: text.chars().collect(),
        at: 0,
    };
    let value = parser.value(0)?;
    ensure!(
        parser.at == parser.chars.len(),
        "Unexpected trailing Discover state"
    );
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}
impl Parser {
    fn take(&mut self) -> Result<char> {
        let c = self
            .chars
            .get(self.at)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Incomplete Discover state"))?;
        self.at += 1;
        Ok(c)
    }
    fn expect(&mut self, expected: char) -> Result<()> {
        ensure!(
            self.take()? == expected,
            "Expected '{expected}' in Discover state"
        );
        Ok(())
    }
    fn value(&mut self, depth: usize) -> Result<Value> {
        ensure!(depth < 32, "Discover state is nested too deeply");
        match self.take()? {
            '(' => {
                let mut object = Map::new();
                if self.chars.get(self.at) == Some(&')') {
                    self.at += 1;
                    return Ok(Value::Object(object));
                }
                loop {
                    let key = self.word()?;
                    self.expect(':')?;
                    ensure!(
                        !object.contains_key(&key),
                        "Duplicate Discover field: {key}"
                    );
                    object.insert(key, self.value(depth + 1)?);
                    match self.take()? {
                        ')' => break,
                        ',' => {}
                        _ => bail!("Expected ',' or ')' in Discover object"),
                    }
                }
                Ok(Value::Object(object))
            }
            '!' => match self.take()? {
                't' => Ok(Value::Bool(true)),
                'f' => Ok(Value::Bool(false)),
                'n' => Ok(Value::Null),
                '(' => {
                    let mut values = Vec::new();
                    if self.chars.get(self.at) == Some(&')') {
                        self.at += 1;
                        return Ok(Value::Array(values));
                    }
                    loop {
                        values.push(self.value(depth + 1)?);
                        match self.take()? {
                            ')' => break,
                            ',' => {}
                            _ => bail!("Expected ',' or ')' in Discover array"),
                        }
                    }
                    Ok(Value::Array(values))
                }
                _ => bail!("Unknown Discover Rison escape"),
            },
            _ => {
                self.at -= 1;
                let quoted = self.chars[self.at] == '\'';
                let word = self.word()?;
                if !quoted {
                    if let Ok(number) = word.parse::<serde_json::Number>() {
                        return Ok(Value::Number(number));
                    }
                }
                Ok(Value::String(word))
            }
        }
    }
    fn word(&mut self) -> Result<String> {
        let mut word = String::new();
        if self.chars.get(self.at) == Some(&'\'') {
            self.at += 1;
            loop {
                match self.take()? {
                    '\'' => return Ok(word),
                    '!' => {
                        let c = self.take()?;
                        ensure!(matches!(c, '!' | '\''), "Invalid quoted Rison escape");
                        word.push(c);
                    }
                    c => word.push(c),
                }
            }
        }
        while let Some(&c) = self.chars.get(self.at) {
            if matches!(c, '(' | ')' | ':' | ',' | '!' | '\'') || c.is_whitespace() {
                break;
            }
            word.push(c);
            self.at += 1;
        }
        ensure!(!word.is_empty(), "Expected a Discover Rison value");
        Ok(word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_escapes_nested_state_and_ambiguity() {
        let v = decode("(query:'שלום !'世界!!',flags:!(!t,!f,!n),sort:!(!('@timestamp',desc)))")
            .unwrap();
        assert_eq!(v["query"], "שלום '世界!");
        assert_eq!(v["sort"][0][1], "desc");
        for bad in ["(a:1,a:2)", "(a:'unfinished)", "(a:!x)", "()junk"] {
            assert!(decode(bad).is_err(), "{bad}");
        }
    }
}
