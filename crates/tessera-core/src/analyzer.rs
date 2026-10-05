//! The text analyzer for a bilingual vault (#12).
//!
//! 2077 of 8103 notes in the target vault contain Cyrillic, and Russian and
//! English routinely share a sentence. One stemmer is therefore wrong for a
//! large part of the corpus whichever language it is: an English stemmer
//! leaves «перезапуска» unrelated to «перезапуск», a Russian one mangles
//! "running".
//!
//! Per-note language detection was considered and rejected — the mixing is
//! within lines, not between notes. The stemmer here decides **per token**, by
//! script: a token in Cyrillic stems as Russian, a token in Latin stems as
//! English, anything else (digits, CJK, mixed) passes through untouched.
//!
//! Registered under [`TOKENIZER`]; every text field that should stem uses it.

use std::borrow::Cow;
use std::mem;
use std::sync::OnceLock;

use tantivy::tokenizer::{
    LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer, Token, TokenFilter, TokenStream,
    Tokenizer,
};

/// Name the analyzer is registered under in the index's tokenizer manager.
/// Stored in the schema, so it must be registered again on every `open`.
pub const TOKENIZER: &str = "tessera_bilingual";

pub fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .filter(ScriptStemmer)
        .build()
}

/// Stem each token in the language its script implies.
#[derive(Clone, Copy, Default)]
pub struct ScriptStemmer;

impl TokenFilter for ScriptStemmer {
    type Tokenizer<T: Tokenizer> = ScriptStemmerFilter<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> ScriptStemmerFilter<T> {
        ScriptStemmerFilter { inner: tokenizer }
    }
}

#[derive(Clone)]
pub struct ScriptStemmerFilter<T> {
    inner: T,
}

impl<T: Tokenizer> Tokenizer for ScriptStemmerFilter<T> {
    type TokenStream<'a> = ScriptStemmerStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        // One stemmer pair per process, not per token stream. A stream is
        // created for every field of every document at index time and for
        // every query term at search time; building two Snowball stemmers
        // each time was the first L1 suspect named in SEARCH-ACCEPTANCE.md,
        // and `Stemmer` is Sync, so sharing it costs nothing.
        static EN: OnceLock<rust_stemmers::Stemmer> = OnceLock::new();
        static RU: OnceLock<rust_stemmers::Stemmer> = OnceLock::new();
        ScriptStemmerStream {
            tail: self.inner.token_stream(text),
            en: EN
                .get_or_init(|| rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::English)),
            ru: RU
                .get_or_init(|| rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::Russian)),
            buffer: String::new(),
        }
    }
}

pub struct ScriptStemmerStream<T> {
    tail: T,
    en: &'static rust_stemmers::Stemmer,
    ru: &'static rust_stemmers::Stemmer,
    buffer: String,
}

#[derive(PartialEq, Eq)]
enum Script {
    Latin,
    Cyrillic,
    Other,
}

fn script_of(s: &str) -> Script {
    let mut latin = false;
    let mut cyrillic = false;
    for c in s.chars() {
        if c.is_ascii_alphabetic() {
            latin = true;
        } else if ('\u{0400}'..='\u{04FF}').contains(&c) {
            cyrillic = true;
        } else if c.is_alphabetic() {
            // Some other script: do not guess a stemmer for it.
            return Script::Other;
        }
    }
    match (latin, cyrillic) {
        (true, false) => Script::Latin,
        (false, true) => Script::Cyrillic,
        // Mixed within one token (transliteration, a typo) or no letters at
        // all: leave it alone. Stemming half a word is worse than none.
        _ => Script::Other,
    }
}

impl<T: TokenStream> TokenStream for ScriptStemmerStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        let token = self.tail.token_mut();
        let stemmer = match script_of(&token.text) {
            Script::Latin => self.en,
            Script::Cyrillic => self.ru,
            Script::Other => return true,
        };
        match stemmer.stem(&token.text) {
            Cow::Owned(s) => token.text = s,
            Cow::Borrowed(s) => {
                self.buffer.clear();
                self.buffer.push_str(s);
                mem::swap(&mut token.text, &mut self.buffer);
            }
        }
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(text: &str) -> Vec<String> {
        let mut a = analyzer();
        let mut stream = a.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(stream.token().text.clone());
        }
        out
    }

    #[test]
    fn english_stems_as_english() {
        assert_eq!(terms("running runs ran"), vec!["run", "run", "ran"]);
    }

    #[test]
    fn russian_stems_as_russian() {
        // перезапуск / перезапуска / перезапуском share a stem
        let t = terms("перезапуск перезапуска перезапуском");
        assert_eq!(t[0], t[1]);
        assert_eq!(t[1], t[2]);
    }

    #[test]
    fn one_sentence_mixing_both_stems_each_word_in_its_own_language() {
        // The case the vault is full of: Russian prose around an English term.
        let t = terms("Конфигурация выполняется через systemd, restarting обязателен");
        assert!(t.contains(&"restart".to_string()), "{t:?}");
        assert_eq!(
            terms("конфигурации")[0],
            t[0],
            "Russian inflection folds: {t:?}"
        );
    }

    #[test]
    fn digits_and_mixed_script_tokens_are_left_alone() {
        assert_eq!(
            terms("2026 v0.26 systemd1"),
            vec!["2026", "v0", "26", "systemd1"]
        );
    }

    #[test]
    fn case_folds_before_stemming() {
        assert_eq!(terms("RUNNING"), terms("running"));
        assert_eq!(terms("КОНФИГУРАЦИЯ"), terms("конфигурация"));
    }
}
