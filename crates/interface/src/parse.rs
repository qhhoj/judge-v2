//! Grammar v1 over `lex` tokens. Whatever the grammar does not accept is refused at the first token of the
//! declaration or directive that holds it, quoting that declaration. The grammar, in order of the code:
//! `header := { directive | "using namespace std;" | declaration }`,
//! `declaration := ["extern"] return NAME "(" [params] ")" ";"`, `params := param { "," param }`.

use crate::lex::{Token, TokenKind, lex};
use crate::model::{Function, Interface, Param, Pass, Ret, Type};
use crate::refusal::{Position, Reason, Refusal};
use crate::{MAX_FUNCTIONS, MAX_HEADER_BYTES, MAX_NESTING, MAX_PARAMETERS};

type Parsed<T> = Result<T, Refusal>;

/// A refusal quotes at most this many characters of its construct, then `...`.
const MAX_CONSTRUCT_CHARS: usize = 200;

/// Reads a header. `file` is only used in messages.
pub fn parse_header(file: &str, text: &str) -> Parsed<Interface> {
    if text.len() > MAX_HEADER_BYTES {
        let at = Some(Position { line: 1, column: 1 });
        return Err(Refusal {
            file: file.into(),
            at,
            construct: String::new(),
            reason: Reason::HeaderTooLarge,
        });
    }
    let tokens = lex(file, text)?;
    let parser = Parser {
        file,
        tokens,
        index: 0,
        start: 0,
        using_std: false,
        guard: Guard::Absent,
        interface: Interface { includes: Vec::new(), functions: Vec::new() },
    };
    parser.parse()
}

/// The include guard: `#ifndef X` as the first token, `#define X` (no value) right after it, `#endif` last.
enum Guard {
    Absent,
    Opened(String),
    Defined,
    Closed,
}

struct Parser<'a> {
    file: &'a str,
    tokens: Vec<Token>,
    index: usize,
    /// Index of the first token of the declaration or directive being read (refusals point here).
    start: usize,
    using_std: bool,
    guard: Guard,
    interface: Interface,
}

/// Words that open a construct the grammar refuses before any type is read, with their reason.
fn refused_opening(word: &str) -> Option<Reason> {
    let reason = match word {
        "struct" | "class" | "enum" | "union" => Reason::Aggregate(word.into()),
        "typedef" | "using" => Reason::Alias,
        "namespace" => Reason::Namespace,
        "template" => Reason::Template("template".into()),
        "__attribute__" => Reason::Attribute,
        "static" | "inline" | "virtual" | "constexpr" | "friend" | "register" | "thread_local"
        | "explicit" => Reason::Specifier(word.into()),
        _ => return None,
    };
    Some(reason)
}

/// C++ words that can never name a function or a parameter (a generated call or proxy would not compile).
const RESERVED_WORDS: &[&str] = &[
    "const",
    "void",
    "int",
    "long",
    "double",
    "bool",
    "extern",
    "unsigned",
    "signed",
    "short",
    "char",
    "float",
    "auto",
    "new",
    "delete",
    "operator",
    "this",
    "return",
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "goto",
    "try",
    "catch",
    "throw",
    "true",
    "false",
    "nullptr",
    "sizeof",
    "typename",
    "volatile",
    "mutable",
    "public",
    "private",
    "protected",
    "and",
    "or",
    "not",
    "xor",
    "decltype",
    "static_cast",
    "const_cast",
    "dynamic_cast",
    "reinterpret_cast",
    "typeid",
    "alignof",
    "alignas",
    "noexcept",
    "wchar_t",
    "char16_t",
    "char32_t",
    "export",
    "asm",
    "bitand",
];

impl Parser<'_> {
    // ----- token access -----

    fn kind(&self, ahead: usize) -> Option<&TokenKind> {
        self.tokens.get(self.index + ahead).map(|token| &token.kind)
    }

    fn is_punct(&self, punct: &str) -> bool {
        matches!(self.kind(0), Some(TokenKind::Punct(found)) if *found == punct)
    }

    fn is_word_at(&self, ahead: usize, word: &str) -> bool {
        matches!(self.kind(ahead), Some(TokenKind::Ident(found)) if found == word)
    }

    fn eat_punct(&mut self, punct: &str) -> bool {
        let found = self.is_punct(punct);
        self.index += usize::from(found);
        found
    }

    fn eat_word(&mut self, word: &str) -> bool {
        let found = self.is_word_at(0, word);
        self.index += usize::from(found);
        found
    }

    fn expect_punct(&mut self, punct: &str) -> Parsed<()> {
        if self.eat_punct(punct) { Ok(()) } else { Err(self.unexpected()) }
    }

    // ----- refusals -----

    fn unexpected(&self) -> Refusal {
        let reason = match self.tokens.get(self.index) {
            Some(token) => Reason::Unexpected(token_text(&token.kind)),
            None => Reason::UnexpectedEnd,
        };
        self.refuse(reason)
    }

    /// A refusal at the first token of the construct being read, quoting it up to its `;` or `{`.
    fn refuse(&self, reason: Reason) -> Refusal {
        let Some(first) = self.tokens.get(self.start) else {
            return Refusal { file: self.file.into(), at: None, construct: String::new(), reason };
        };
        let construct = match &first.kind {
            TokenKind::Directive(text) => format!("#{text}"),
            _ => {
                let from = self.index.max(self.start);
                let end = (from..self.tokens.len())
                    .find(|&i| matches!(self.tokens[i].kind, TokenKind::Punct(";" | "{")))
                    .unwrap_or(self.tokens.len());
                render(&self.tokens[self.start..end.max(self.start + 1)])
            }
        };
        Refusal { file: self.file.into(), at: Some(first.at), construct: shortened(construct), reason }
    }

    // ----- header level -----

    fn parse(mut self) -> Parsed<Interface> {
        while self.index < self.tokens.len() {
            self.start = self.index;
            self.require_guard_define()?;
            self.item()?;
        }
        if matches!(self.guard, Guard::Opened(_) | Guard::Defined) {
            self.start = 0;
            return Err(self.refuse(Reason::GuardNotClosed));
        }
        Ok(self.interface)
    }

    /// Right after `#ifndef X` only `#define X` may follow.
    fn require_guard_define(&self) -> Parsed<()> {
        let is_define =
            matches!(self.kind(0), Some(TokenKind::Directive(text)) if text.starts_with("define"));
        if matches!(self.guard, Guard::Opened(_)) && !is_define {
            return Err(self.refuse(Reason::GuardMismatch));
        }
        Ok(())
    }

    fn item(&mut self) -> Parsed<()> {
        if let Some(TokenKind::Directive(text)) = self.kind(0) {
            let text = text.clone();
            self.index += 1;
            return self.directive(&text);
        }
        if self.is_word_at(0, "using") && self.is_word_at(1, "namespace") {
            return self.using_namespace_std();
        }
        self.declaration()
    }

    fn using_namespace_std(&mut self) -> Parsed<()> {
        self.index += 2;
        if self.eat_word("std") && self.eat_punct(";") {
            self.using_std = true;
            return Ok(());
        }
        Err(self.refuse(Reason::Namespace))
    }

    fn directive(&mut self, text: &str) -> Parsed<()> {
        let name: String = text.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let rest = text[name.len()..].trim();
        let accepted = match name.as_str() {
            "include" => return self.include(rest),
            "define" => return self.define_guard(rest),
            "pragma" => rest == "once",
            "ifndef" => return self.open_guard(rest),
            "endif" => rest.is_empty() && self.close_guard(),
            _ => false,
        };
        if accepted { Ok(()) } else { Err(self.refuse(Reason::Directive(name))) }
    }

    fn include(&mut self, rest: &str) -> Parsed<()> {
        let inner = rest.strip_prefix('<').and_then(|tail| tail.strip_suffix('>'));
        match inner {
            Some(name) if is_include_name(name) => {
                self.interface.includes.push(name.to_string());
                Ok(())
            }
            _ if rest.starts_with('"') => Err(self.refuse(Reason::QuotedInclude)),
            _ => Err(self.refuse(Reason::Directive("include".into()))),
        }
    }

    /// `#ifndef X` as the first token; `X` is a name the glue and the runtime leave free.
    fn open_guard(&mut self, rest: &str) -> Parsed<()> {
        let first_token = self.start == 0 && matches!(self.guard, Guard::Absent);
        if !first_token || !is_identifier(rest) {
            return Err(self.refuse(Reason::Directive("ifndef".into())));
        }
        self.reserved_name(rest)?;
        self.guard = Guard::Opened(rest.to_string());
        Ok(())
    }

    fn define_guard(&mut self, rest: &str) -> Parsed<()> {
        let Guard::Opened(name) = &self.guard else {
            return Err(self.refuse(Reason::Directive("define".into())));
        };
        if rest != name {
            return Err(self.refuse(Reason::GuardMismatch));
        }
        self.guard = Guard::Defined;
        Ok(())
    }

    fn close_guard(&mut self) -> bool {
        let last = self.index == self.tokens.len();
        if last && matches!(self.guard, Guard::Defined) {
            self.guard = Guard::Closed;
            return true;
        }
        false
    }

    // ----- declarations -----

    fn opening_checks(&self) -> Parsed<()> {
        if self.is_punct("[") {
            return Err(self.refuse(Reason::Attribute));
        }
        let opening = match self.kind(0) {
            Some(TokenKind::Ident(word)) => refused_opening(word),
            _ => None,
        };
        if let Some(reason) = opening {
            return Err(self.refuse(reason));
        }
        Ok(())
    }

    fn declaration(&mut self) -> Parsed<()> {
        self.opening_checks()?;
        let line = self.tokens[self.start].at.line;
        if self.eat_word("extern") {
            if matches!(self.kind(0), Some(TokenKind::Text(_))) {
                return Err(self.refuse(Reason::ExternC));
            }
            self.opening_checks()?;
        }
        let ret = self.ret()?;
        let name = self.function_name()?;
        let params = self.params()?;
        self.expect_punct(")")?;
        self.after_parameters()?;
        let id = self.interface.functions.len() as u16;
        self.add_function(Function { id, name, ret, params, line })
    }

    /// Checks the limits and the unique name, then takes the declaration's `;`.
    fn add_function(&mut self, function: Function) -> Parsed<()> {
        let functions = &self.interface.functions;
        if functions.len() >= MAX_FUNCTIONS {
            return Err(self.refuse(Reason::TooManyFunctions));
        }
        if functions.iter().any(|known| known.name == function.name) {
            return Err(self.refuse(Reason::Overload(function.name)));
        }
        self.interface.functions.push(function);
        self.index += 1; // the ';' checked by after_parameters
        Ok(())
    }

    /// The function's name and its opening parenthesis; `name;` and `name = ...` are variables.
    fn function_name(&mut self) -> Parsed<String> {
        let Some(TokenKind::Ident(name)) = self.kind(0).cloned() else {
            return Err(self.unexpected());
        };
        self.reserved_name(&name)?;
        self.index += 1;
        if let Some(reason) = self.variable_shape() {
            return Err(self.refuse(reason));
        }
        self.expect_punct("(")?;
        Ok(name)
    }

    /// After a name: `;` or `=` declares a variable, `[` an array.
    fn variable_shape(&self) -> Option<Reason> {
        match self.kind(0) {
            Some(TokenKind::Punct(";" | "=")) => Some(Reason::Variable),
            Some(TokenKind::Punct("[")) => Some(Reason::Array),
            _ => None,
        }
    }

    fn reserved_name(&self, name: &str) -> Parsed<()> {
        if name == "main" || name.to_ascii_lowercase().starts_with("qhhoj") {
            return Err(self.refuse(Reason::ReservedName(name.into())));
        }
        if RESERVED_WORDS.contains(&name) || refused_opening(name).is_some() {
            return Err(self.refuse(Reason::Unexpected(name.into())));
        }
        Ok(())
    }

    /// What may follow `)`: only the `;` (left in place for `add_function`).
    fn after_parameters(&self) -> Parsed<()> {
        match self.kind(0) {
            Some(TokenKind::Punct(";")) => Ok(()),
            Some(TokenKind::Punct("{")) => Err(self.refuse(Reason::Body)),
            Some(TokenKind::Punct("[")) => Err(self.refuse(Reason::Attribute)),
            Some(TokenKind::Punct("=")) => Err(self.refuse(Reason::Specifier("=".into()))),
            Some(TokenKind::Ident(word)) if word == "__attribute__" => Err(self.refuse(Reason::Attribute)),
            Some(TokenKind::Ident(word))
                if matches!(word.as_str(), "noexcept" | "throw" | "const" | "override" | "final") =>
            {
                Err(self.refuse(Reason::Specifier(word.clone())))
            }
            _ => Err(self.unexpected()),
        }
    }

    // ----- parameters and returns -----

    fn params(&mut self) -> Parsed<Vec<Param>> {
        if self.is_punct(")") {
            return Ok(Vec::new());
        }
        if self.is_word_at(0, "void") && matches!(self.kind(1), Some(TokenKind::Punct(")"))) {
            self.index += 1;
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        loop {
            params.push(self.param()?);
            if params.len() > MAX_PARAMETERS {
                return Err(self.refuse(Reason::TooManyParameters));
            }
            if !self.eat_punct(",") {
                return Ok(params);
            }
        }
    }

    fn param(&mut self) -> Parsed<Param> {
        if self.is_punct("...") {
            return Err(self.refuse(Reason::Variadic));
        }
        let (ty, pass) = self.qualified_type()?;
        let name = self.param_name()?;
        if self.is_punct("[") {
            return Err(self.refuse(Reason::Array));
        }
        if self.is_punct("=") {
            return Err(self.refuse(Reason::DefaultArgument));
        }
        Ok(Param { ty, pass, name })
    }

    fn param_name(&mut self) -> Parsed<Option<String>> {
        let Some(TokenKind::Ident(name)) = self.kind(0).cloned() else {
            return Ok(None);
        };
        self.reserved_name(&name)?;
        self.index += 1;
        Ok(Some(name))
    }

    fn ret(&mut self) -> Parsed<Ret> {
        if self.eat_word("void") {
            self.refuse_indirection()?;
            if self.is_punct("&") {
                return Err(self.refuse(Reason::Reference));
            }
            return Ok(Ret::Void);
        }
        match self.qualified_type()? {
            (ty, Pass::Value) => Ok(Ret::Value(ty)),
            (ty, Pass::ConstRef) => Ok(Ret::ConstRef(ty)),
            (_, Pass::ConstValue) => Err(self.const_value_return()),
        }
    }

    /// `const T` then a name: a variable (`const int N = 5;`) is refused as one; anything else as a const return.
    fn const_value_return(&mut self) -> Refusal {
        if matches!(self.kind(0), Some(TokenKind::Ident(_))) {
            self.index += 1;
            if let Some(reason) = self.variable_shape() {
                return self.refuse(reason);
            }
        }
        self.refuse(Reason::ConstValueReturn)
    }

    /// `T`, `const T`, `const T&` or `T const&`: the type and how it is passed. A trailing `const` on a value, a
    /// bare `&` and `&&` are refused.
    fn qualified_type(&mut self) -> Parsed<(Type, Pass)> {
        let leading = self.eat_word("const");
        let ty = self.type_(1)?;
        let trailing = self.eat_word("const");
        self.refuse_indirection()?;
        let reference = self.eat_punct("&");
        let pass = match (leading, trailing, reference) {
            (false, false, false) => Pass::Value,
            (true, false, false) => Pass::ConstValue,
            (true, false, true) | (false, true, true) => Pass::ConstRef,
            (false, false, true) => return Err(self.refuse(Reason::Reference)),
            _ => return Err(self.refuse(Reason::Unexpected("const".into()))),
        };
        Ok((ty, pass))
    }

    /// `*` (pointer) and `&&` (rvalue reference) after a type are never accepted.
    fn refuse_indirection(&self) -> Parsed<()> {
        if self.is_punct("*") {
            return Err(self.refuse(Reason::Pointer));
        }
        if self.is_punct("&&") {
            return Err(self.refuse(Reason::Reference));
        }
        Ok(())
    }

    // ----- types -----

    fn type_(&mut self, depth: usize) -> Parsed<Type> {
        if depth > MAX_NESTING {
            return Err(self.refuse(Reason::TooDeep));
        }
        let Some(TokenKind::Ident(word)) = self.kind(0).cloned() else {
            return Err(self.unexpected());
        };
        if word == "const" {
            return Err(self.unexpected());
        }
        self.index += 1;
        match word.as_str() {
            "int" => Ok(Type::Int),
            "bool" => Ok(Type::Bool),
            "double" => Ok(Type::Double),
            "long" => self.long_type(),
            "std" if self.eat_punct("::") => self.std_type(depth),
            "string" | "vector" | "pair" if !self.using_std => Err(self.refuse(Reason::Unqualified(word))),
            "string" | "vector" | "pair" => self.library_type(&word, depth),
            "struct" | "class" | "enum" | "union" => Err(self.refuse(Reason::Aggregate(word))),
            _ if self.is_punct("<") => Err(self.refuse(Reason::Template(word))),
            _ => Err(self.refuse(Reason::Scalar(word))),
        }
    }

    /// After `long`: `long long [int]` is the one supported spelling; any other is refused by its spelling.
    fn long_type(&mut self) -> Parsed<Type> {
        if self.eat_word("long") {
            self.eat_word("int");
            return Ok(Type::LongLong);
        }
        let spelling = if self.eat_word("int") {
            "long int"
        } else if self.eat_word("double") {
            "long double"
        } else {
            "long"
        };
        Err(self.refuse(Reason::Scalar(spelling.into())))
    }

    fn std_type(&mut self, depth: usize) -> Parsed<Type> {
        let Some(TokenKind::Ident(name)) = self.kind(0).cloned() else {
            return Err(self.unexpected());
        };
        self.index += 1;
        match name.as_str() {
            "string" | "vector" | "pair" => self.library_type(&name, depth),
            _ if self.is_punct("<") => Err(self.refuse(Reason::Template(format!("std::{name}")))),
            _ => Err(self.refuse(Reason::Scalar(format!("std::{name}")))),
        }
    }

    /// `string`, `vector<T>` or `pair<A, B>` once its name is read.
    fn library_type(&mut self, name: &str, depth: usize) -> Parsed<Type> {
        match name {
            "string" => Ok(Type::String),
            "vector" => {
                self.expect_punct("<")?;
                let item = self.type_(depth + 1)?;
                self.close_angle()?;
                Ok(Type::Vector(Box::new(item)))
            }
            _ => {
                self.expect_punct("<")?;
                let first = self.type_(depth + 1)?;
                self.expect_punct(",")?;
                let second = self.type_(depth + 1)?;
                self.close_angle()?;
                Ok(Type::Pair(Box::new(first), Box::new(second)))
            }
        }
    }

    /// A `>` is consumed; a `>>` is rewritten in place to `>` and not consumed, so the enclosing template takes
    /// the second half.
    fn close_angle(&mut self) -> Parsed<()> {
        match self.kind(0) {
            Some(TokenKind::Punct(">")) => self.index += 1,
            Some(TokenKind::Punct(">>")) => self.tokens[self.index].kind = TokenKind::Punct(">"),
            _ => {
                self.refuse_indirection()?;
                if self.is_punct("&") {
                    return Err(self.refuse(Reason::Reference));
                }
                if self.is_punct("[") {
                    return Err(self.refuse(Reason::Array));
                }
                return Err(self.unexpected());
            }
        }
        Ok(())
    }
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let starts_well = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    starts_well && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A header name in `#include <...>`: relative (no leading `/`), no `..`, no blanks, only path characters.
fn is_include_name(name: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '+' | '-');
    !name.is_empty() && !name.starts_with('/') && !name.contains("..") && name.chars().all(allowed)
}

/// The construct cut to `MAX_CONSTRUCT_CHARS` characters, marked with `...` when cut.
fn shortened(construct: String) -> String {
    if construct.chars().count() <= MAX_CONSTRUCT_CHARS {
        return construct;
    }
    let kept: String = construct.chars().take(MAX_CONSTRUCT_CHARS).collect();
    format!("{kept}...")
}

fn token_text(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Ident(text) | TokenKind::Number(text) => text.clone(),
        TokenKind::Text(text) => format!("\"{text}\""),
        TokenKind::Punct(punct) => (*punct).to_string(),
        TokenKind::Directive(text) => format!("#{text}"),
    }
}

/// The tokens on one line: a space goes before a word or number that follows a word, number, `*`, `&`, `&&`, `>`
/// or `>>`, and after every `,`.
fn render(tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut previous: Option<&TokenKind> = None;
    for token in tokens {
        let is_word = matches!(token.kind, TokenKind::Ident(_) | TokenKind::Number(_));
        let spaced = match previous {
            Some(TokenKind::Ident(_) | TokenKind::Number(_)) => is_word,
            Some(TokenKind::Punct("*" | "&" | "&&" | ">" | ">>")) => is_word,
            Some(TokenKind::Punct(",")) => true,
            _ => false,
        };
        if spaced {
            out.push(' ');
        }
        out += &token_text(&token.kind);
        previous = Some(&token.kind);
    }
    out
}
