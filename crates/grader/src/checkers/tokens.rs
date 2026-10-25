//! Streaming tokenizer shared by the checkers: one token at a time, never a whole file.
//! A token's `line` counts the non-empty lines before it, so blank lines collapse while
//! line breaks stay significant: `1 2` and `1\n2` differ.

use super::CheckError;
use super::text::is_space as is_space_byte;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: Vec<u8>,
    pub line: u64,
}

pub fn open(path: &Path) -> Result<BufReader<File>, CheckError> {
    File::open(path).map(BufReader::new).map_err(|e| CheckError(format!("{}: {e}", path.display())))
}

pub fn token_stream<R: BufRead>(reader: R, unicode: bool) -> TokenStream<R> {
    TokenStream { units: Units { reader, unicode }, token: Vec::new(), line: 0, line_has_tokens: false }
}

/// True when both files hold the same tokens on the same lines.
pub fn same_tokens(expected: &Path, actual: &Path, unicode: bool) -> Result<bool, CheckError> {
    streams_match_files(expected, actual, unicode, |want, got| want == got)
}

pub fn streams_match_files(
    expected: &Path,
    actual: &Path,
    unicode: bool,
    matches: impl Fn(&Token, &Token) -> bool,
) -> Result<bool, CheckError> {
    streams_match(open(expected)?, open(actual)?, unicode, matches).map_err(|e| CheckError(e.to_string()))
}

/// Walks both token streams in step and stops at the first pair that does not match.
pub fn streams_match<A: BufRead, B: BufRead>(
    expected: A,
    actual: B,
    unicode: bool,
    matches: impl Fn(&Token, &Token) -> bool,
) -> io::Result<bool> {
    let (mut want, mut got) = (token_stream(expected, unicode), token_stream(actual, unicode));
    loop {
        match (want.next().transpose()?, got.next().transpose()?) {
            (None, None) => return Ok(true),
            (Some(w), Some(g)) if matches(&w, &g) => {}
            _ => return Ok(false),
        }
    }
}

/// One character read from the stream: its bytes, and whether it separates tokens.
struct Unit {
    bytes: [u8; 4],
    len: usize,
    space: bool,
}

struct Units<R> {
    reader: R,
    unicode: bool,
}

impl<R: BufRead> Units<R> {
    fn byte(&mut self) -> io::Result<Option<u8>> {
        let Some(&first) = self.reader.fill_buf()?.first() else { return Ok(None) };
        self.reader.consume(1);
        Ok(Some(first))
    }

    fn next_unit(&mut self) -> io::Result<Option<Unit>> {
        let Some(first) = self.byte()? else { return Ok(None) };
        let want = if self.unicode { utf8_len(first) } else { 1 };
        let mut bytes = [first, 0, 0, 0];
        let mut len = 1;
        while len < want {
            let Some(&next) = self.reader.fill_buf()?.first() else { break };
            if next & 0xC0 != 0x80 {
                break;
            }
            bytes[len] = next;
            len += 1;
            self.reader.consume(1);
        }
        Ok(Some(Unit { bytes, len, space: is_space(&bytes[..len]) }))
    }
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

fn is_space(bytes: &[u8]) -> bool {
    if let [single] = bytes {
        return is_space_byte(*single);
    }
    std::str::from_utf8(bytes).ok().and_then(|s| s.chars().next()).is_some_and(char::is_whitespace)
}

pub struct TokenStream<R> {
    units: Units<R>,
    token: Vec<u8>,
    line: u64,
    line_has_tokens: bool,
}

impl<R: BufRead> Iterator for TokenStream<R> {
    type Item = io::Result<Token>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let unit = match self.units.next_unit() {
                Ok(Some(unit)) => unit,
                Ok(None) => return self.finish_token().map(Ok),
                Err(error) => return Some(Err(error)),
            };
            if !unit.space {
                self.token.extend_from_slice(&unit.bytes[..unit.len]);
                continue;
            }
            let finished = self.finish_token();
            if unit.len == 1 && unit.bytes[0] == b'\n' && self.line_has_tokens {
                self.line += 1;
                self.line_has_tokens = false;
            }
            if let Some(token) = finished {
                return Some(Ok(token));
            }
        }
    }
}

impl<R> TokenStream<R> {
    fn finish_token(&mut self) -> Option<Token> {
        if self.token.is_empty() {
            return None;
        }
        self.line_has_tokens = true;
        Some(Token { text: std::mem::take(&mut self.token), line: self.line })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    fn tokens_of(reader: impl BufRead, unicode: bool) -> Vec<(String, u64)> {
        token_stream(reader, unicode)
            .map(|t| {
                let t = t.unwrap();
                (String::from_utf8(t.text).unwrap(), t.line)
            })
            .collect()
    }

    fn pairs(list: &[(&str, u64)]) -> Vec<(String, u64)> {
        list.iter().map(|(text, line)| (text.to_string(), *line)).collect()
    }

    #[test]
    fn vertical_tab_and_form_feed_separate_tokens_like_c_isspace() {
        let got = tokens_of(Cursor::new(b"1\x0b2\x0c3".as_slice()), false);
        assert_eq!(got, pairs(&[("1", 0), ("2", 0), ("3", 0)]));
    }

    #[test]
    fn ascii_split_tags_each_token_with_its_line() {
        let got = tokens_of(Cursor::new(b"1  2\t3\n\n\n4 \r\n".as_slice()), false);
        assert_eq!(got, pairs(&[("1", 0), ("2", 0), ("3", 0), ("4", 1)]));
    }

    #[test]
    fn unicode_space_splits_only_in_unicode_mode() {
        let text = "a\u{3000}b".as_bytes(); // ideographic space
        assert_eq!(tokens_of(Cursor::new(text), true).len(), 2);
        assert_eq!(tokens_of(Cursor::new(text), false).len(), 1);
        assert_eq!(tokens_of(BufReader::with_capacity(1, text), true).len(), 2, "split across reads");
    }

    #[test]
    fn invalid_utf8_is_kept_inside_its_token() {
        let raw: Vec<Vec<u8>> =
            token_stream(Cursor::new(&[0xff, b'x'][..]), true).map(|t| t.unwrap().text).collect();
        assert_eq!(raw, vec![vec![0xff, b'x']]);
    }

    struct Poisoned {
        head: Option<&'static [u8]>,
    }

    impl Read for Poisoned {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.head.take() {
                Some(bytes) => {
                    buf[..bytes.len()].copy_from_slice(bytes);
                    Ok(bytes.len())
                }
                None => Err(io::Error::other("read past the first difference")),
            }
        }
    }

    #[test]
    fn stops_reading_at_the_first_difference() {
        let actual = BufReader::new(Poisoned { head: Some(b"2 ") });
        assert!(!streams_match(Cursor::new(b"1 Z".as_slice()), actual, false, |w, g| w == g).unwrap());
    }
}
