//! Header tokens with their line and column (a token grammar, not a C++ front end). Comments and blanks are
//! dropped; a `#` line is one `Directive` token. Columns count characters, so a Vietnamese comment earlier on the
//! line does not shift them.

use crate::refusal::{Position, Reason, Refusal};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Ident(String),
    Number(String),
    /// A string literal, kept only to name it in a refusal (`extern "C"`).
    Text(String),
    Punct(&'static str),
    /// The line after `#`, trimmed, without comments.
    Directive(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub at: Position,
}

/// Longest first, so `>>` is not read as two `>` (the parser splits it when it closes a template).
const PUNCTUATION: [&str; 18] =
    ["...", "::", ">>", "&&", "->", "<", ">", "(", ")", ",", ";", "&", "*", "[", "]", "{", "}", "="];

pub fn lex(file: &str, text: &str) -> Result<Vec<Token>, Refusal> {
    let mut lexer =
        Lexer { file, chars: text.chars().collect(), index: 0, line: 1, column: 1, line_start: true };
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next_token()? {
        tokens.push(token);
    }
    Ok(tokens)
}

struct Lexer<'a> {
    file: &'a str,
    chars: Vec<char>,
    index: usize,
    line: u32,
    column: u32,
    /// Only blanks and comments so far on this line: a `#` here starts a directive.
    line_start: bool,
}

impl Lexer<'_> {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.index + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek(0)?;
        self.index += 1;
        if c == '\n' {
            self.line += 1;
            self.column = 1;
            self.line_start = true;
        } else {
            self.column += 1;
        }
        Some(c)
    }

    fn here(&self) -> Position {
        Position { line: self.line, column: self.column }
    }

    fn refuse(&self, at: Position, reason: Reason) -> Refusal {
        Refusal { file: self.file.to_string(), at: Some(at), construct: String::new(), reason }
    }

    fn at_comment(&self) -> bool {
        self.peek(0) == Some('/') && matches!(self.peek(1), Some('/' | '*'))
    }

    /// Skips one `//` comment (up to, not including, the line end) or one `/* */` comment.
    fn skip_comment(&mut self) -> Result<(), Refusal> {
        let start = self.here();
        self.bump();
        if self.bump() == Some('/') {
            while self.peek(0).is_some_and(|c| c != '\n') {
                self.bump();
            }
            return Ok(());
        }
        while let Some(c) = self.bump() {
            if c == '*' && self.peek(0) == Some('/') {
                self.bump();
                return Ok(());
            }
        }
        Err(self.refuse(start, Reason::UnterminatedComment))
    }

    fn skip_blanks(&mut self) -> Result<(), Refusal> {
        loop {
            match self.peek(0) {
                Some(' ' | '\t' | '\r' | '\n') => {
                    self.bump();
                }
                _ if self.at_comment() => self.skip_comment()?,
                _ => return Ok(()),
            }
        }
    }

    fn next_token(&mut self) -> Result<Option<Token>, Refusal> {
        self.skip_blanks()?;
        let Some(first) = self.peek(0) else { return Ok(None) };
        let at = self.here();
        if first == '#' && self.line_start {
            return self.directive(at).map(Some);
        }
        self.line_start = false;
        let kind = match first {
            c if c.is_ascii_alphabetic() || c == '_' => {
                TokenKind::Ident(self.take_while(|c| c.is_ascii_alphanumeric() || c == '_'))
            }
            c if c.is_ascii_digit() => {
                TokenKind::Number(self.take_while(|c| c.is_ascii_alphanumeric() || c == '.'))
            }
            '"' => self.string_literal(at)?,
            _ => self.punctuation(at)?,
        };
        Ok(Some(Token { kind, at }))
    }

    fn take_while(&mut self, keep: fn(char) -> bool) -> String {
        let mut text = String::new();
        while let Some(c) = self.peek(0).filter(|c| keep(*c)) {
            text.push(c);
            self.bump();
        }
        text
    }

    /// A literal runs to the next `"` on the same line.
    fn string_literal(&mut self, at: Position) -> Result<TokenKind, Refusal> {
        self.bump();
        let text = self.take_while(|c| c != '"' && c != '\n');
        if self.peek(0) == Some('"') {
            self.bump();
            Ok(TokenKind::Text(text))
        } else {
            Err(self.refuse(at, Reason::Unexpected("\"".into())))
        }
    }

    fn punctuation(&mut self, at: Position) -> Result<TokenKind, Refusal> {
        for candidate in PUNCTUATION {
            if candidate.chars().enumerate().all(|(i, c)| self.peek(i) == Some(c)) {
                for _ in candidate.chars() {
                    self.bump();
                }
                return Ok(TokenKind::Punct(candidate));
            }
        }
        let c = self.peek(0).unwrap_or('\0');
        Err(self.refuse(at, Reason::UnexpectedCharacter(c)))
    }

    /// The rest of the line after `#`. A trailing `\` would continue it: no supported directive does, so it is
    /// refused under the directive's own name. `//` and `/* */` comments on the line are dropped.
    fn directive(&mut self, at: Position) -> Result<Token, Refusal> {
        self.bump(); // '#'
        self.line_start = false; // set again only by the newline that ends the directive
        let mut text = String::new();
        while let Some(c) = self.peek(0).filter(|c| *c != '\n') {
            if self.at_comment() {
                self.skip_comment()?;
            } else {
                text.push(c);
                self.bump();
            }
        }
        let text = text.trim().to_string();
        if text.ends_with('\\') {
            let name = text.split_whitespace().next().unwrap_or("").to_string();
            return Err(self.refuse(at, Reason::Directive(name)));
        }
        Ok(Token { kind: TokenKind::Directive(text), at })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<TokenKind> {
        lex("h.h", text).unwrap().into_iter().map(|token| token.kind).collect()
    }

    fn punct(text: &'static str) -> TokenKind {
        TokenKind::Punct(text)
    }

    fn ident(text: &str) -> TokenKind {
        TokenKind::Ident(text.into())
    }

    #[test]
    fn comments_line_ends_and_the_missing_final_newline_do_not_matter() {
        let crlf = "// đề bài: hàm\r\nint /* số */ ask(int i);\r\n";
        let lf_no_final = "int ask(int i);";
        let want =
            vec![ident("int"), ident("ask"), punct("("), ident("int"), ident("i"), punct(")"), punct(";")];
        assert_eq!(kinds(crlf), want);
        assert_eq!(kinds(lf_no_final), want);
    }

    #[test]
    fn positions_are_one_based_lines_and_character_columns() {
        let tokens = lex("h.h", "/* ồ */ int\n  ask();").unwrap();
        assert_eq!(tokens[0].at, Position { line: 1, column: 9 });
        assert_eq!(tokens[1].at, Position { line: 2, column: 3 });
    }

    #[test]
    fn multi_character_punctuation_is_one_token() {
        assert_eq!(
            kinds("std::vector<std::vector<int>>&& f(...)->"),
            vec![
                ident("std"),
                punct("::"),
                ident("vector"),
                punct("<"),
                ident("std"),
                punct("::"),
                ident("vector"),
                punct("<"),
                ident("int"),
                punct(">>"),
                punct("&&"),
                ident("f"),
                punct("("),
                punct("..."),
                punct(")"),
                punct("->"),
            ]
        );
    }

    #[test]
    fn a_directive_is_one_token_per_line_without_its_comments() {
        assert_eq!(
            kinds("  #  include <vector> // why\n#pragma once\n"),
            vec![TokenKind::Directive("include <vector>".into()), TokenKind::Directive("pragma once".into())]
        );
    }

    #[test]
    fn what_the_grammar_cannot_read_is_refused_with_its_position() {
        let refused = |text: &str| lex("h.h", text).unwrap_err();
        let unterminated = refused("int f();\n/* never closed");
        assert_eq!(
            (unterminated.reason, unterminated.at),
            (Reason::UnterminatedComment, Some(Position { line: 2, column: 1 }))
        );
        assert_eq!(refused("int f(int a: 3);").reason, Reason::UnexpectedCharacter(':'));
        assert_eq!(refused("int f(); ồ").reason, Reason::UnexpectedCharacter('ồ'));
        assert_eq!(refused("#define LONG_MACRO \\\n  1\n").reason, Reason::Directive("define".into()));
        assert_eq!(refused("int f() \\\n;").reason, Reason::UnexpectedCharacter('\\'));
    }

    #[test]
    fn string_and_number_literals_are_kept_for_messages() {
        assert_eq!(
            kinds("extern \"C\" 42"),
            vec![ident("extern"), TokenKind::Text("C".into()), TokenKind::Number("42".into())]
        );
    }

    #[test]
    fn an_unclosed_string_and_a_directive_comment_that_never_ends_are_refused_not_panicked() {
        assert_eq!(lex("h.h", "extern \"C").unwrap_err().reason, Reason::Unexpected("\"".into()));
        assert_eq!(lex("h.h", "#include <a> /* open").unwrap_err().reason, Reason::UnterminatedComment);
    }
}
