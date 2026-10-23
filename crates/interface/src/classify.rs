//! Which side implements each function, decided by the compiler: a probe that takes the address of every
//! declaration, minus a baseline that only includes the header, maps each declaration to its symbol; the real `-O2`
//! entry object then defines it (callback: the jury implements it, the contestant calls it), references it
//! (contestant function) or neither (unwired: no glue). So the required set equals today's link exactly.

use std::collections::BTreeSet;

use crate::model::{Function, Interface, Ret};
use crate::refusal::{Position, Reason, Refusal};

/// One undefined symbol of the probe object, mangled as the linker sees it and demangled (`c++filt`), same order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub mangled: String,
    pub demangled: String,
}

/// The jury entry object's symbols (`nm --defined-only`, weak W/V included, and `nm -u`), mangled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySymbols {
    pub defined: BTreeSet<String>,
    pub undefined: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The grader defines it; the contestant calls it.
    Callback,
    /// The contestant defines it; the grader calls it.
    Contestant,
    /// The grader neither defines nor calls it: no glue.
    Unwired,
}

/// Roles and compiler symbols, both indexed by `Function::id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub roles: Vec<Role>,
    pub symbols: Vec<String>,
}

/// `solve[abi:cxx11](int)` -> `solve`: the name before the parameter list or ABI tag.
fn declared_name(demangled: &str) -> &str {
    demangled.split(['(', '[']).next().unwrap_or(demangled)
}

/// Probe minus baseline, one symbol per declaration, by demangled name (overloads are refused, names are unique).
/// Symbols that name no declaration (a runtime helper the probe references) are ignored.
pub fn map_declarations(
    interface: &Interface,
    probe: &[Symbol],
    baseline: &BTreeSet<String>,
    file: &str,
) -> Result<Vec<String>, Refusal> {
    let fresh: Vec<&Symbol> = probe.iter().filter(|symbol| !baseline.contains(&symbol.mangled)).collect();
    let symbol_of = |function: &Function| {
        fresh
            .iter()
            .find(|symbol| declared_name(&symbol.demangled) == function.name)
            .map(|symbol| symbol.mangled.clone())
    };
    interface
        .functions
        .iter()
        .map(|function| {
            symbol_of(function)
                .ok_or_else(|| problem(file, function, Reason::UnmappedDeclaration(function.name.clone())))
        })
        .collect()
}

pub fn classify(
    interface: &Interface,
    symbols: Vec<String>,
    entry: &EntrySymbols,
    file: &str,
) -> Result<Classification, Refusal> {
    let roles: Vec<Role> = symbols.iter().map(|symbol| role_of(symbol, entry)).collect();
    if !roles.contains(&Role::Contestant) {
        let reason = Reason::NoContestantFunction;
        return Err(Refusal { file: file.into(), at: None, construct: String::new(), reason });
    }
    for (function, role) in interface.functions.iter().zip(&roles) {
        if *role == Role::Contestant && matches!(function.ret, Ret::ConstRef(_)) {
            return Err(problem(file, function, Reason::ConstRefContestantReturn(function.name.clone())));
        }
    }
    Ok(Classification { roles, symbols })
}

fn role_of(symbol: &str, entry: &EntrySymbols) -> Role {
    if entry.defined.contains(symbol) {
        Role::Callback
    } else if entry.undefined.contains(symbol) {
        Role::Contestant
    } else {
        Role::Unwired
    }
}

/// The entry's undefined symbols that are no declaration: the build step checks them by linking the entry with
/// `glue::probe::null_contestant`; listed here so the refusal can name them.
pub fn foreign_symbols(classification: &Classification, entry: &EntrySymbols) -> Vec<String> {
    entry.undefined.iter().filter(|symbol| !classification.symbols.contains(symbol)).cloned().collect()
}

/// A problem-level refusal about one function: the declaration's line, no column worth quoting.
fn problem(file: &str, function: &Function, reason: Reason) -> Refusal {
    let at = Some(Position { line: function.line, column: 1 });
    Refusal { file: file.into(), at, construct: String::new(), reason }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_header;

    fn header() -> Interface {
        parse_header(
            "grader.h",
            "std::string solve(int);\nint ask(int);\nvoid note(int);\nint spare(int);\nconst std::vector<int>& view();\n",
        )
        .unwrap()
    }

    fn probe() -> Vec<Symbol> {
        let pairs = [
            ("_Z5solveB5cxx11i", "solve[abi:cxx11](int)"),
            ("_Z3aski", "ask(int)"),
            ("_Z4notei", "note(int)"),
            ("_Z5sparei", "spare(int)"),
            ("_Z4viewv", "view()"),
            ("_Z7helperv", "helper()"),
        ];
        pairs.iter().map(|(m, d)| Symbol { mangled: m.to_string(), demangled: d.to_string() }).collect()
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn each_declaration_gets_its_compiler_symbol_abi_tag_included() {
        let symbols = map_declarations(&header(), &probe(), &set(&["_Z7helperv"]), "grader.h").unwrap();
        assert_eq!(symbols, vec!["_Z5solveB5cxx11i", "_Z3aski", "_Z4notei", "_Z5sparei", "_Z4viewv"]);
    }

    #[test]
    fn a_declaration_without_a_symbol_is_refused_by_name() {
        let mut short = probe();
        short.retain(|symbol| symbol.mangled != "_Z4notei");
        let refusal = map_declarations(&header(), &short, &set(&[]), "grader.h").unwrap_err();
        assert_eq!(refusal.reason, Reason::UnmappedDeclaration("note".into()));
    }

    #[test]
    fn defined_is_a_callback_referenced_is_the_contestants_neither_is_unwired() {
        let symbols = map_declarations(&header(), &probe(), &set(&[]), "grader.h").unwrap();
        let entry = EntrySymbols {
            defined: set(&["main", "_Z3aski", "_Z4notei", "_Z4viewv"]),
            undefined: set(&["_Z5solveB5cxx11i", "printf", "_ZSt4cout"]),
        };
        let classification = classify(&header(), symbols, &entry, "grader.h").unwrap();
        use Role::*;
        assert_eq!(classification.roles, vec![Contestant, Callback, Callback, Unwired, Callback]);
        assert_eq!(
            foreign_symbols(&classification, &entry),
            vec!["_ZSt4cout".to_string(), "printf".to_string()]
        );
    }

    #[test]
    fn an_interface_without_contestant_functions_is_refused() {
        let symbols = map_declarations(&header(), &probe(), &set(&[]), "grader.h").unwrap();
        let entry = EntrySymbols { defined: set(&["_Z3aski"]), undefined: set(&[]) };
        let refusal = classify(&header(), symbols, &entry, "grader.h").unwrap_err();
        assert_eq!(refusal.reason, Reason::NoContestantFunction);
        assert_eq!(refusal.at, None);
    }

    #[test]
    fn only_callbacks_may_return_const_references() {
        let symbols = map_declarations(&header(), &probe(), &set(&[]), "grader.h").unwrap();
        let entry = EntrySymbols { defined: set(&[]), undefined: set(&["_Z4viewv", "_Z5solveB5cxx11i"]) };
        let refusal = classify(&header(), symbols, &entry, "grader.h").unwrap_err();
        assert_eq!(refusal.reason, Reason::ConstRefContestantReturn("view".into()));
        assert_eq!(refusal.at, Some(Position { line: 5, column: 1 }));
    }

    #[test]
    fn a_name_that_prefixes_another_is_not_mixed_up_with_it() {
        let interface = parse_header("grader.h", "int ask(int);\nint ask2(int);\n").unwrap();
        let probe = vec![
            Symbol { mangled: "_Z4ask2i".into(), demangled: "ask2(int)".into() },
            Symbol { mangled: "_Z3aski".into(), demangled: "ask(int)".into() },
        ];
        let symbols = map_declarations(&interface, &probe, &set(&[]), "grader.h").unwrap();
        assert_eq!(symbols, vec!["_Z3aski", "_Z4ask2i"]);
    }
}
