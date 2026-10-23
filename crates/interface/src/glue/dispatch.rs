//! The `qhhoj_serve` dispatcher both sides share: one `case` per served function.

use crate::model::Function;

/// `body` writes what follows the argument decode of one case.
pub(crate) fn dispatcher(served: &[&Function], body: fn(&Function) -> String) -> String {
    let mut out = String::from(
        "static void qhhoj_serve(qhhoj::Request& qhhoj_request) {\n  switch (qhhoj_request.fn()) {\n",
    );
    for function in served {
        out += &format!("    case {}: {{\n", function.id);
        out += &decode_arguments(function);
        out += "      qhhoj_request.end();\n";
        out += &body(function);
        out += "      break;\n    }\n";
    }
    out + "    default:\n      qhhoj::unknown_function(qhhoj_request);\n  }\n}\n"
}

fn decode_arguments(function: &Function) -> String {
    function
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| format!("      {ty} qhhoj_a{i} = qhhoj_request.get<{ty} >();\n", ty = p.ty.cpp()))
        .collect()
}
