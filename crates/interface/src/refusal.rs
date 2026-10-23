//! Every reason a header or an interface is refused, with its one text.

use std::fmt;

use crate::MAX_HEADER_BYTES;

pub const SUPPORTED_TYPES: &str = "Supported: int, long long, double, bool, std::string, std::vector<T>, \
std::pair<A,B> of supported types, by value or const&; void or const& returns.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct Refusal {
    pub file: String,
    /// The construct's first token; None for a problem-level refusal (roles, symbols).
    pub at: Option<Position>,
    /// The declaration or directive on one line; empty when none applies.
    pub construct: String,
    pub reason: Reason,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.file)?;
        if let Some(at) = self.at {
            write!(f, "{}:{}:", at.line, at.column)?;
        }
        if !self.construct.is_empty() {
            write!(f, " '{}':", self.construct)?;
        }
        write!(f, " {}.", self.reason)?;
        if self.reason.lists_supported_types() {
            write!(f, " {SUPPORTED_TYPES}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    UnexpectedCharacter(char),
    LoneCarriageReturn,
    LineSplice,
    HeaderTooLarge,
    UnterminatedComment,
    QuotedInclude,
    Directive(String),
    GuardMismatch,
    GuardNotClosed,
    TextAfterEndif,
    Pointer,
    Array,
    Reference,
    Scalar(String),
    Template(String),
    Unqualified(String),
    Aggregate(String),
    Alias,
    Namespace,
    DefaultArgument,
    Variadic,
    Body,
    Specifier(String),
    Attribute,
    ExternC,
    Variable,
    ConstValueReturn,
    Overload(String),
    ReservedName(String),
    TooManyFunctions,
    TooManyParameters,
    TooDeep,
    Unexpected(String),
    UnexpectedEnd,
    NoContestantFunction,
    ConstRefContestantReturn(String),
    UnmappedDeclaration(String),
    ForeignSymbols(Vec<String>),
}

impl Reason {
    pub fn lists_supported_types(&self) -> bool {
        matches!(
            self,
            Reason::Pointer | Reason::Array | Reason::Reference | Reason::Scalar(_) | Reason::Template(_)
        )
    }

    /// Reasons found before or while splitting the header text into tokens.
    fn lexical_text(&self) -> Option<String> {
        let text = match self {
            Reason::UnexpectedCharacter(c) => {
                format!("the character {c:?} is not part of the supported grammar")
            }
            Reason::LoneCarriageReturn => {
                "a carriage return without a line feed is not a supported line end; save the header with LF or CRLF line ends"
                    .into()
            }
            Reason::LineSplice => {
                "a line ending in '\\' continues onto the next line, which is not supported (in comments either)".into()
            }
            Reason::HeaderTooLarge => format!("the header is larger than {MAX_HEADER_BYTES} bytes"),
            Reason::UnterminatedComment => "the comment is not closed".into(),
            _ => return None,
        };
        Some(text)
    }

    /// Reasons found while reading the header's directives and declarations.
    fn syntax_text(&self) -> Option<String> {
        let text = match self {
            Reason::QuotedInclude => {
                "quoted includes are not supported (the header must be self-contained); use #include <...>"
                    .into()
            }
            Reason::Directive(name) => {
                format!("#{name} is not supported (only an include guard, #pragma once and #include <...>)")
            }
            Reason::GuardMismatch => {
                "the include guard must be #ifndef X, then #define X with no value, then #endif at the end"
                    .into()
            }
            Reason::TextAfterEndif => "nothing may follow #endif on its line".into(),
            Reason::GuardNotClosed => {
                "the include guard is not closed by #endif at the end of the header".into()
            }
            Reason::Pointer => "pointer types are not supported".into(),
            Reason::Array => "array types are not supported".into(),
            Reason::Reference => {
                "non-const and rvalue references are not supported (a parameter cannot carry a value back)"
                    .into()
            }
            Reason::Scalar(name) => format!("the type '{name}' is not supported"),
            Reason::Template(name) => format!("the template '{name}' is not supported"),
            Reason::Unqualified(name) => {
                format!("'{name}' without 'std::' needs 'using namespace std;' earlier in the header")
            }
            Reason::Aggregate(word) => format!("'{word}' declarations are not supported"),
            Reason::Alias => "type aliases (typedef, using X =) are not supported".into(),
            Reason::Namespace => "namespaces are not supported".into(),
            Reason::DefaultArgument => "default arguments are not supported".into(),
            Reason::Variadic => "variadic functions are not supported".into(),
            Reason::Body => "function bodies are not supported (declarations only)".into(),
            _ => return None,
        };
        Some(text)
    }

    /// Reasons about declarations, limits and the compiler's symbol tables.
    fn interface_text(&self) -> String {
        match self {
            Reason::Specifier(word) => format!("'{word}' is not supported"),
            Reason::Attribute => "attributes are not supported".into(),
            Reason::ExternC => "extern \"C\" is not supported".into(),
            Reason::Variable => "variables are not supported (functions only)".into(),
            Reason::ConstValueReturn => "a const return must be a reference (const T&)".into(),
            Reason::Overload(name) => format!("'{name}' is declared twice (one name, one declaration)"),
            Reason::ReservedName(name) if name.to_ascii_lowercase().starts_with("qhhoj") => {
                format!("the name '{name}' is reserved (the prefix 'qhhoj' is, in any case)")
            }
            Reason::ReservedName(name) => format!("the name '{name}' is reserved"),
            Reason::TooManyFunctions => "more than 64 functions".into(),
            Reason::TooManyParameters => "more than 16 parameters".into(),
            Reason::TooDeep => "types nested deeper than 8 levels".into(),
            Reason::Unexpected(token) => format!("unexpected '{token}'"),
            Reason::UnexpectedEnd => "the header ends inside a declaration".into(),
            Reason::NoContestantFunction => "the grader calls none of the header's functions, so there is nothing for the contestant to implement".into(),
            Reason::ConstRefContestantReturn(name) => {
                format!("'{name}' returns const&, which only functions the grader provides may do")
            }
            Reason::UnmappedDeclaration(name) => format!("the compiler gave no symbol for '{name}'"),
            Reason::ForeignSymbols(names) => format!(
                "the grader uses {}, which it does not define and the header does not declare; that cannot be provided from another process",
                names.join(", ")
            ),
            // Every lexical and syntax reason is answered above; the test below pins that none lands here.
            other => format!("{other:?}"),
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = self.lexical_text().or_else(|| self.syntax_text());
        f.write_str(&text.unwrap_or_else(|| self.interface_text()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_reads_file_line_column_construct_reason_and_the_supported_set() {
        let refusal = Refusal {
            file: "grader.h".into(),
            at: Some(Position { line: 7, column: 1 }),
            construct: "int* pick(int)".into(),
            reason: Reason::Pointer,
        };
        assert_eq!(
            refusal.to_string(),
            format!("grader.h:7:1: 'int* pick(int)': pointer types are not supported. {SUPPORTED_TYPES}")
        );
    }

    #[test]
    fn a_problem_level_refusal_has_no_position_and_no_type_list() {
        let refusal = Refusal {
            file: "grader.h".into(),
            at: None,
            construct: String::new(),
            reason: Reason::NoContestantFunction,
        };
        assert_eq!(
            refusal.to_string(),
            "grader.h: the grader calls none of the header's functions, so there is nothing for the contestant to implement."
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one list of every reason
    fn every_reason_has_its_own_text() {
        let all = [
            Reason::UnexpectedCharacter('x'),
            Reason::LoneCarriageReturn,
            Reason::LineSplice,
            Reason::HeaderTooLarge,
            Reason::UnterminatedComment,
            Reason::QuotedInclude,
            Reason::Directive("d".into()),
            Reason::GuardMismatch,
            Reason::GuardNotClosed,
            Reason::TextAfterEndif,
            Reason::Pointer,
            Reason::Array,
            Reason::Reference,
            Reason::Scalar("s".into()),
            Reason::Template("t".into()),
            Reason::Unqualified("u".into()),
            Reason::Aggregate("a".into()),
            Reason::Alias,
            Reason::Namespace,
            Reason::DefaultArgument,
            Reason::Variadic,
            Reason::Body,
            Reason::Specifier("s".into()),
            Reason::Attribute,
            Reason::ExternC,
            Reason::Variable,
            Reason::ConstValueReturn,
            Reason::Overload("o".into()),
            Reason::ReservedName("r".into()),
            Reason::TooManyFunctions,
            Reason::TooManyParameters,
            Reason::TooDeep,
            Reason::Unexpected("u".into()),
            Reason::UnexpectedEnd,
            Reason::NoContestantFunction,
            Reason::ConstRefContestantReturn("c".into()),
            Reason::UnmappedDeclaration("m".into()),
            Reason::ForeignSymbols(vec!["a".into(), "b".into()]),
        ];
        for reason in all {
            let text = reason.to_string();
            assert_ne!(text, format!("{reason:?}"), "fell through to the Debug form");
        }
    }
}
