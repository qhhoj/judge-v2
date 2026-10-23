use judge_interface::{Interface, Pass, Position, Reason, Ret, Type, parse_header};

fn parsed(text: &str) -> Interface {
    parse_header("grader.h", text).unwrap_or_else(|refusal| panic!("{refusal}"))
}

fn refused(text: &str) -> (Reason, Position, String) {
    let refusal = parse_header("grader.h", text).expect_err("accepted");
    (refusal.reason, refusal.at.expect("a header refusal has a position"), refusal.construct)
}

fn vector(item: Type) -> Type {
    Type::Vector(Box::new(item))
}

#[test]
fn a_plain_header_gives_functions_in_declaration_order() {
    let interface = parsed("int ask(int i);\nvoid note(int);\nvoid solve();\n");
    let names: Vec<_> = interface.functions.iter().map(|f| (f.id, f.name.as_str(), f.line)).collect();
    assert_eq!(names, vec![(0, "ask", 1), (1, "note", 2), (2, "solve", 3)]);
    assert_eq!(interface.functions[0].ret, Ret::Value(Type::Int));
    assert_eq!(interface.functions[0].params[0].name.as_deref(), Some("i"));
    assert_eq!(interface.functions[1].params[0].name, None);
}

#[test]
fn guards_crlf_no_final_newline_and_double_close_are_accepted() {
    let text = "#ifndef GRADER_H\r\n#define GRADER_H\r\n#include <vector>\r\n\
                const std::vector<std::vector<int>>& view();\r\n#endif";
    let view = &parsed(text).functions[0];
    assert_eq!(view.ret, Ret::ConstRef(vector(vector(Type::Int))));
    assert_eq!(parsed(text).includes, vec!["vector".to_string()]);
}

#[test]
fn a_declaration_may_span_lines_and_use_every_parameter_form() {
    let text = "#include <bits/stdc++.h>\nusing namespace std;\n\
                pair<int, int>\nsolve(int n,\n      const vector<int>& a, vector<bool> const& mask,\n      const string s, long long int k, long long t, double x, bool b);\n";
    let solve = &parsed(text).functions[0];
    let passes: Vec<Pass> = solve.params.iter().map(|p| p.pass).collect();
    use Pass::*;
    assert_eq!(passes, vec![Value, ConstRef, ConstRef, ConstValue, Value, Value, Value, Value]);
    assert_eq!(solve.params[4].ty, Type::LongLong);
    assert_eq!(solve.ret, Ret::Value(Type::Pair(Box::new(Type::Int), Box::new(Type::Int))));
}

#[test]
fn void_parameter_lists_extern_and_pragma_once_are_accepted() {
    let interface = parsed("#pragma once\nextern int ask(void);\nvoid go( );\nint open(int, int);\n");
    assert!(interface.functions.iter().take(2).all(|f| f.params.is_empty()));
    assert_eq!(interface.functions[2].name, "open", "a libc name is fine in the header");
}

#[test]
#[allow(clippy::too_many_lines)] // one data table, one assertion loop
fn every_unsupported_construct_is_refused_at_its_declaration() {
    use Reason::*;
    let at = |line, column| Position { line, column };
    let table: Vec<(&str, Reason, Position)> = vec![
        ("int* pick(int);", Pointer, at(1, 1)),
        ("void f(int a[]);", Array, at(1, 1)),
        ("void f(int& x);", Reference, at(1, 1)),
        ("void f(std::vector<int>&& v);", Reference, at(1, 1)),
        ("char f(int);", Scalar("char".into()), at(1, 1)),
        ("unsigned g();", Scalar("unsigned".into()), at(1, 1)),
        ("long h();", Scalar("long".into()), at(1, 1)),
        ("long int h();", Scalar("long int".into()), at(1, 1)),
        ("long double h();", Scalar("long double".into()), at(1, 1)),
        ("size_t n();", Scalar("size_t".into()), at(1, 1)),
        ("std::map<int, int> m();", Template("std::map".into()), at(1, 1)),
        ("std::tuple<int> t();", Template("std::tuple".into()), at(1, 1)),
        ("vector<int> f();", Unqualified("vector".into()), at(1, 1)),
        ("struct P { int x; };", Aggregate("struct".into()), at(1, 1)),
        ("enum E { A };", Aggregate("enum".into()), at(1, 1)),
        ("typedef int T;", Alias, at(1, 1)),
        ("using T = int;", Alias, at(1, 1)),
        ("namespace a { int f(); }", Namespace, at(1, 1)),
        ("int f(int x = 1);", DefaultArgument, at(1, 1)),
        ("int f(...);", Variadic, at(1, 1)),
        ("int f(int) { return 0; }", Body, at(1, 1)),
        ("inline int f();", Specifier("inline".into()), at(1, 1)),
        ("static int f();", Specifier("static".into()), at(1, 1)),
        ("int f() noexcept;", Specifier("noexcept".into()), at(1, 1)),
        ("template <class T> T f();", Template("template".into()), at(1, 1)),
        ("[[nodiscard]] int f();", Attribute, at(1, 1)),
        ("__attribute__((pure)) int f();", Attribute, at(1, 1)),
        ("extern \"C\" int f();", ExternC, at(1, 1)),
        ("int x;", Variable, at(1, 1)),
        ("extern int x;", Variable, at(1, 1)),
        ("const int f();", ConstValueReturn, at(1, 1)),
        ("void f(int const x);", Unexpected("const".into()), at(1, 1)),
        ("void f(void x);", Scalar("void".into()), at(1, 1)),
        ("int main();", ReservedName("main".into()), at(1, 1)),
        ("int qhhoj_call();", ReservedName("qhhoj_call".into()), at(1, 1)),
        ("void f(int QHHOJ_x);", ReservedName("QHHOJ_x".into()), at(1, 1)),
        ("int f();\n  int f(long long);", Overload("f".into()), at(2, 3)),
        ("#include \"other.h\"", QuotedInclude, at(1, 1)),
        ("#define N 10", Directive("define".into()), at(1, 1)),
        ("#ifdef X\n#endif", Directive("ifdef".into()), at(1, 1)),
        ("#if 1\n#endif", Directive("if".into()), at(1, 1)),
        ("#ifndef G\n#define G 1\n#endif", GuardMismatch, at(2, 1)),
        ("#ifndef G\n#define H\n#endif", GuardMismatch, at(2, 1)),
        ("int f();\n#ifndef G\n#define G\n#endif", Directive("ifndef".into()), at(2, 1)),
        ("#ifndef G\n#define G\nint f();", GuardNotClosed, at(1, 1)),
        ("int f(int", UnexpectedEnd, at(1, 1)),
    ];
    for (text, reason, position) in table {
        let (got, got_at, _) = refused(text);
        assert_eq!((got, got_at), (reason, position), "{text}");
    }
}

#[test]
fn a_refusal_quotes_the_whole_declaration_on_one_line() {
    let (_, at, construct) = refused("void ok();\nint*\n  pick(int);\n");
    assert_eq!(at, Position { line: 2, column: 1 });
    assert_eq!(construct, "int* pick(int)");
    let message = parse_header("grader.h", "int* pick(int);").unwrap_err().to_string();
    assert!(
        message
            .starts_with("grader.h:1:1: 'int* pick(int)': pointer types are not supported. Supported: int,"),
        "{message}"
    );
}

#[test]
fn limits_on_functions_parameters_and_nesting() {
    let many: String = (0..65).map(|i| format!("void f{i}();\n")).collect();
    assert_eq!(refused(&many).0, Reason::TooManyFunctions);
    assert_eq!(refused(&many).1, Position { line: 65, column: 1 });
    let params = vec!["int"; 17].join(", ");
    assert_eq!(refused(&format!("void f({params});")).0, Reason::TooManyParameters);
    let deep = format!("{}int{} f();", "std::vector<".repeat(8), ">".repeat(8));
    assert_eq!(refused(&deep).0, Reason::TooDeep);
    let fits = format!("{}int{} f();", "std::vector<".repeat(7), " >".repeat(7));
    assert_eq!(parsed(&fits).functions[0].ret, Ret::Value((0..7).fold(Type::Int, |t, _| vector(t))));
}

#[test]
fn spelling_a_parsed_declaration_and_parsing_it_again_gives_the_same_function() {
    let text = "using namespace std;\nconst vector<pair<long long, string>>& view();\nvoid solve(vector<vector<bool>> g, const double d);\n";
    for function in parsed(text).functions {
        let params: Vec<String> =
            function.params.iter().enumerate().map(|(i, p)| p.cpp(&format!("a{i}"))).collect();
        let again = parsed(&format!("{} {}({});", function.ret.cpp(), function.name, params.join(", ")));
        let mut back = again.functions[0].clone();
        back.id = function.id;
        back.line = function.line;
        for (param, original) in back.params.iter_mut().zip(&function.params) {
            param.name = original.name.clone();
        }
        assert_eq!(back, function);
    }
}
