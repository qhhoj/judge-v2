//! Sources compiled only to learn symbols: the probe, its baseline, and the null contestant of the
//! foreign-symbol link check.

use super::{signature, with_role};
use crate::classify::Role;
use crate::model::Interface;

/// Takes every declaration's address, so `nm -u` names its symbol.
pub fn probe(interface: &Interface, header: &str) -> String {
    let mut out = format!(
        "#include \"{header}\"\ntypedef void (*qhhoj_any_function)();\nqhhoj_any_function qhhoj_probe[] = {{\n"
    );
    for function in &interface.functions {
        out += &format!("  (qhhoj_any_function)&{},\n", function.name);
    }
    out + "};\n"
}

/// The same includes, nothing taken: its undefined symbols are not the declarations'.
pub fn baseline(header: &str) -> String {
    format!("#include \"{header}\"\n")
}

/// A definition of every contestant function, so a link of the entry with it fails only for symbols the entry
/// needs that are neither declared nor in the libraries. Parameters are named; an unused-parameter warning does
/// not fail a build without `-Werror`.
pub fn null_contestant(interface: &Interface, roles: &[Role], header: &str) -> String {
    let mut out = baseline(header);
    for function in with_role(interface, roles, Role::Contestant) {
        out += &format!("{} {{ __builtin_trap(); }}\n", signature(function));
    }
    out
}
