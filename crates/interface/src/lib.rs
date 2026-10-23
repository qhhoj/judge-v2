//! Split signature interfaces: the problem header under grammar v1, the role
//! of each function, the interface log, and the C++ sources a split run compiles. Pure: no file, process or clock.
//!
//! Where things live: tokens in `lex`, the grammar in `parse`, the model in `model`, every refusal text in
//! `refusal`, roles from symbol tables in `classify`, the Data tab text in `log`, generated C++ in `glue`.

pub mod classify;
pub mod glue;
pub mod lex;
pub mod log;
pub mod model;
pub mod parse;
pub mod refusal;

pub use classify::{Classification, EntrySymbols, Role, Symbol, classify, foreign_symbols, map_declarations};
pub use model::{Function, Interface, Param, Pass, Ret, Type};
pub use parse::parse_header;
pub use refusal::{Position, Reason, Refusal};

/// The header grammar this crate accepts; a new scalar bumps it.
pub const GRAMMAR: &str = "v1";
/// Version of the generated C++; part of the jury build cache key, so a generator change rebuilds juries.
pub const GENERATOR_VERSION: u32 = 1;
pub const MAX_FUNCTIONS: usize = 64;
pub const MAX_PARAMETERS: usize = 16;
pub const MAX_NESTING: usize = 8;
