use judge_interface::glue::{contestant::contestant_glue, jury::jury_glue, probe};
use judge_interface::{Role, parse_header};

const HEADER: &str = "#include <vector>\nstd::vector<int> solve(int n, const std::vector<int>& a);\nint ask(int i);\nvoid note(int v);\nconst std::vector<std::vector<int> >& view();\nint spare(int);\n";
const ROLES: [Role; 5] = [Role::Contestant, Role::Callback, Role::Callback, Role::Callback, Role::Unwired];

/// Every top-level definition of a generated file, as its signature line before `{`, turned back into a
/// declaration. The generated parameter names (`qhhoj_a0`) are reserved in headers, so they are renamed first.
fn defined_signatures(source: &str) -> Vec<String> {
    source
        .lines()
        .filter(|line| {
            !line.starts_with(' ')
                && line.ends_with(" {")
                && !line.starts_with("static")
                && !line.starts_with("int main")
        })
        .map(|line| format!("{};", line.trim_end_matches(" {").replace("qhhoj_a", "arg")))
        .collect()
}

fn declarations(text: &str) -> Vec<judge_interface::Function> {
    parse_header("g.h", text).unwrap().functions
}

#[test]
fn the_contestant_side_defines_exactly_the_callbacks_with_their_declared_signatures() {
    let interface = parse_header("g.h", HEADER).unwrap();
    let glue = contestant_glue(&interface, &ROLES, "grader.h");
    let defined: Vec<_> = defined_signatures(&glue).iter().map(|s| declarations(s).remove(0)).collect();
    let names: Vec<_> = defined.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["ask", "note", "view"]);
    for function in defined {
        let declared = interface.functions.iter().find(|f| f.name == function.name).unwrap();
        let shape = |f: &judge_interface::Function| {
            (f.ret.clone(), f.params.iter().map(|p| (p.ty.clone(), p.pass)).collect::<Vec<_>>())
        };
        assert_eq!(shape(&function), shape(declared));
    }
}

#[test]
fn the_jury_side_defines_exactly_the_contestant_functions_and_has_no_main() {
    let interface = parse_header("g.h", HEADER).unwrap();
    let glue = jury_glue(&interface, &ROLES, "grader.h");
    let names: Vec<_> = defined_signatures(&glue).iter().map(|s| declarations(s).remove(0).name).collect();
    assert_eq!(names, vec!["solve"]);
    assert!(!glue.contains("main("), "the entry keeps its own main");
}

#[test]
fn each_side_dispatches_exactly_the_functions_the_other_side_calls() {
    let interface = parse_header("g.h", HEADER).unwrap();
    let cases = |glue: &str| {
        glue.lines()
            .filter_map(|l| l.trim().strip_prefix("case "))
            .map(|l| l.trim_end_matches(": {").to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(cases(&contestant_glue(&interface, &ROLES, "grader.h")), vec!["0"]);
    assert_eq!(cases(&jury_glue(&interface, &ROLES, "grader.h")), vec!["1", "2", "3"]);
}

#[test]
fn every_callback_case_on_the_jury_side_guards_the_call_and_reports_a_throw() {
    let interface = parse_header("g.h", HEADER).unwrap();
    let glue = jury_glue(&interface, &ROLES, "grader.h");
    for id in 1..=3 {
        assert!(glue.contains(&format!("qhhoj::Serving qhhoj_serving({id});")), "{id}");
        assert!(glue.contains(&format!("qhhoj::callback_threw({id});")), "{id}");
    }
    assert!(glue.contains("qhhoj::reply_cached(3, qhhoj_out);"));
}

#[test]
fn a_const_reference_result_names_the_runtime_copy_of_its_object() {
    let interface = parse_header("g.h", HEADER).unwrap();
    let glue = contestant_glue(&interface, &ROLES, "grader.h");
    assert!(!glue.contains("qhhoj_cache"), "no per-function cache that two references would share");
    assert!(glue.contains("return qhhoj_call.result_cached<std::vector<std::vector<int> > >();"));
}

#[test]
fn generated_files_include_only_the_header_and_the_runtime() {
    let interface = parse_header("g.h", "int open(int, int);\nvoid solve();\n").unwrap();
    let roles = [Role::Callback, Role::Contestant];
    for glue in [contestant_glue(&interface, &roles, "grader.h"), jury_glue(&interface, &roles, "grader.h")] {
        let includes: Vec<_> = glue.lines().filter(|l| l.starts_with("#include")).collect();
        assert_eq!(includes, vec!["#include \"grader.h\"", "#include \"qhhoj_rpc.hpp\""]);
    }
}

#[test]
fn the_probe_takes_every_declaration_and_the_null_contestant_defines_only_contestant_functions() {
    let interface = parse_header("g.h", HEADER).unwrap();
    let probe_source = probe::probe(&interface, "grader.h");
    for name in ["solve", "ask", "note", "view", "spare"] {
        assert!(probe_source.contains(&format!("(qhhoj_any_function)&::{name},")), "{name}");
    }
    assert_eq!(probe::baseline("grader.h"), "#include \"grader.h\"\n");
    let null = probe::null_contestant(&interface, &ROLES, "grader.h");
    let names: Vec<_> = defined_signatures(&null.replace(" { __builtin_trap(); }", " {"))
        .iter()
        .map(|s| declarations(s).remove(0).name)
        .collect();
    assert_eq!(names, vec!["solve"]);
}

#[test]
fn the_pair_for_the_reference_interface_has_the_documented_shape() {
    let header = "std::vector<int> solve(int n, const std::vector<int>& a);\nint ask(int);\nvoid note(int);\nconst std::vector<std::vector<int> >& view();\n";
    let interface = parse_header("g.h", header).unwrap();
    let roles = [Role::Contestant, Role::Callback, Role::Callback, Role::Callback];
    let jury = jury_glue(&interface, &roles, "grader.h");
    assert!(jury.contains(
        "std::vector<int> solve(int qhhoj_a0, const std::vector<int>& qhhoj_a1) {\n  qhhoj::Call qhhoj_call(0, qhhoj_serve);\n  qhhoj_call.put(qhhoj_a0);\n  qhhoj_call.put(qhhoj_a1);\n  return qhhoj_call.result<std::vector<int> >();\n}\n"
    ));
    assert!(jury.contains(
        "      const std::vector<std::vector<int> >* qhhoj_out = 0;\n      try {\n        qhhoj::Serving qhhoj_serving(3);\n        qhhoj_out = &::view();\n      } catch (...) {\n        qhhoj::callback_threw(3);\n      }\n      qhhoj::reply_cached(3, qhhoj_out);\n"
    ));
    let contestant = contestant_glue(&interface, &roles, "grader.h");
    assert!(contestant.contains(
        "    case 0: {\n      int qhhoj_a0 = qhhoj_request.get<int >();\n      std::vector<int> qhhoj_a1 = qhhoj_request.get<std::vector<int> >();\n      qhhoj_request.end();\n      qhhoj::reply(0, ::solve(qhhoj_a0, qhhoj_a1));\n      break;\n    }\n"
    ));
    assert!(contestant.ends_with("int main() { return qhhoj::contestant_main(qhhoj_serve); }\n"));
}

#[test]
fn a_scalar_callback_result_starts_at_zero_and_a_library_one_at_its_default() {
    let header = "#include <string>\nlong long big();\nbool flag();\nstd::string word();\nvoid solve();\n";
    let interface = parse_header("g.h", header).unwrap();
    let roles = [Role::Callback, Role::Callback, Role::Callback, Role::Contestant];
    let jury = jury_glue(&interface, &roles, "grader.h");
    assert!(jury.contains("      long long qhhoj_out = 0;\n"), "{jury}");
    assert!(jury.contains("      bool qhhoj_out = 0;\n"), "{jury}");
    assert!(jury.contains("      std::string qhhoj_out = std::string();\n"), "{jury}");
}

#[test]
fn generated_calls_and_addresses_name_the_global_declaration() {
    let header = "#include <algorithm>\nusing namespace std;\nint count(int x);\nint find(int x);\n";
    let interface = parse_header("g.h", header).unwrap();
    let roles = [Role::Callback, Role::Contestant];
    assert!(probe::probe(&interface, "grader.h").contains("(qhhoj_any_function)&::count,"));
    assert!(jury_glue(&interface, &roles, "grader.h").contains("qhhoj_out = ::count(qhhoj_a0);"));
    assert!(contestant_glue(&interface, &roles, "grader.h").contains("qhhoj::reply(1, ::find(qhhoj_a0));"));
}

#[test]
#[should_panic(expected = "one role per function")]
fn glue_for_roles_that_do_not_match_the_functions_is_a_programming_error() {
    let interface = parse_header("g.h", "int ask(int);\nvoid solve();\n").unwrap();
    contestant_glue(&interface, &[Role::Callback], "grader.h");
}
