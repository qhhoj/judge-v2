//! The interface log shown on the Data tab: which side implements what, in declaration order, with short types.

use crate::GRAMMAR;
use crate::classify::{Classification, Role};
use crate::model::{Function, Interface, Ret};

/// The Data tab text: one line per non-empty side, then the contract note when a result is refreshed.
pub fn interface_log(interface: &Interface, classification: &Classification) -> String {
    let with = |role: Role| -> Vec<&Function> {
        let roles = classification.roles.iter();
        interface
            .functions
            .iter()
            .zip(roles)
            .filter(|(_, found)| **found == role)
            .map(|(function, _)| function)
            .collect()
    };
    let callbacks = with(Role::Callback);
    let refreshed = callbacks.iter().any(|function| matches!(function.ret, Ret::ConstRef(_)));
    let mut lines = vec![format!("interface ok (grammar {GRAMMAR})")];
    lines.extend(section("contestant implements", with(Role::Contestant)));
    lines.extend(section("grader provides", callbacks));
    lines.extend(section("unwired", with(Role::Unwired)));
    if refreshed {
        lines.push("refreshed results must live until the grader exits (static or global storage)".into());
    }
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// `label: sig; sig`, or nothing when the side has no function.
fn section(label: &str, functions: Vec<&Function>) -> Option<String> {
    if functions.is_empty() {
        return None;
    }
    let signatures: Vec<String> = functions.into_iter().map(signature).collect();
    Some(format!("{label}: {}", signatures.join("; ")))
}

/// `name(p1, p2) -> ret`, with `[refreshed]` after a `const&` return.
fn signature(function: &Function) -> String {
    let params: Vec<String> = function.params.iter().map(|param| param.short()).collect();
    let refreshed = if matches!(function.ret, Ret::ConstRef(_)) { " [refreshed]" } else { "" };
    format!("{}({}) -> {}{refreshed}", function.name, params.join(", "), function.ret.short())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role::*;

    #[test]
    fn the_log_lists_each_side_in_declaration_order_with_short_types() {
        let interface = crate::parse_header(
            "g.h",
            "std::string solve(int);\nint ask(int);\nconst std::vector<std::vector<int> >& view();\nint spare(const std::vector<bool>& m, long long k);\n",
        )
        .unwrap();
        let classification = Classification {
            roles: vec![Contestant, Callback, Callback, Unwired],
            symbols: vec![String::new(); 4],
        };
        assert_eq!(
            interface_log(&interface, &classification),
            "interface ok (grammar v1)\n\
             contestant implements: solve(int) -> string\n\
             grader provides: ask(int) -> int; view() -> const vector<vector<int>>& [refreshed]\n\
             unwired: spare(const vector<bool>&, long long) -> int\n\
             refreshed results must live until the grader exits (static or global storage)\n"
        );
    }

    #[test]
    fn empty_sections_are_left_out() {
        let interface = crate::parse_header("g.h", "void solve();\n").unwrap();
        let classification = Classification { roles: vec![Contestant], symbols: vec![String::new()] };
        assert_eq!(
            interface_log(&interface, &classification),
            "interface ok (grammar v1)\ncontestant implements: solve() -> void\n"
        );
    }
}
