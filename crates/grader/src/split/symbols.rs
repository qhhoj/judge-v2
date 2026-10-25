//! `nm` listings (`nm -p` order, BSD format) read into the symbol sets `judge_interface::classify` takes. A
//! line is `[address] type name`; a demangled name may hold spaces, so the name is the rest of the line.

use judge_interface::{EntrySymbols, Symbol};
use std::collections::BTreeSet;

/// The name on one listing line, or None for a blank or malformed line.
fn symbol_name(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let (first, rest) = line.split_once(' ')?;
    let is_address = first.len() > 1 && first.bytes().all(|b| b.is_ascii_hexdigit());
    let typed = if is_address { rest.trim_start() } else { line };
    let (kind, name) = typed.split_once(' ')?;
    (kind.len() == 1 && !name.is_empty()).then_some(name)
}

pub fn names(listing: &str) -> Vec<String> {
    listing.lines().filter_map(symbol_name).map(str::to_string).collect()
}

/// The probe's undefined symbols, paired line by line with the same listing run through `c++filt`.
pub fn probe_symbols(mangled: &str, demangled: &str) -> Vec<Symbol> {
    names(mangled)
        .into_iter()
        .zip(names(demangled))
        .map(|(mangled, demangled)| Symbol { mangled, demangled })
        .collect()
}

pub fn entry_symbols(defined: &str, undefined: &str) -> EntrySymbols {
    EntrySymbols {
        defined: names(defined).into_iter().collect::<BTreeSet<_>>(),
        undefined: names(undefined).into_iter().collect::<BTreeSet<_>>(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defined_and_undefined_lines_give_their_names() {
        let defined = "0000000000000000 T _Z1hi\n0000000000000010 W _Z4viewv\n0000000000000000 B g\n";
        let undefined = "                 U _Z1fB5cxx11i\n                 w __gmon_start__\n\n";
        let entry = entry_symbols(defined, undefined);
        assert_eq!(entry.defined.into_iter().collect::<Vec<_>>(), ["_Z1hi", "_Z4viewv", "g"]);
        assert_eq!(entry.undefined.into_iter().collect::<Vec<_>>(), ["_Z1fB5cxx11i", "__gmon_start__"]);
    }

    #[test]
    fn a_demangled_name_keeps_its_spaces_and_pairs_with_its_mangled_line() {
        let mangled = "                 U _Z3addxx\n                 U _Z1fB5cxx11i\n";
        let demangled =
            "                 U add(long long, long long)\n                 U f[abi:cxx11](int)\n";
        let symbols = probe_symbols(mangled, demangled);
        assert_eq!(
            symbols[0],
            Symbol { mangled: "_Z3addxx".into(), demangled: "add(long long, long long)".into() }
        );
        assert_eq!(symbols[1].demangled, "f[abi:cxx11](int)");
    }
}
