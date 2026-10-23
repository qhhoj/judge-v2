//! `qhhoj_jury.cpp`: proxies for the contestant functions the grader calls, and a dispatcher that runs the
//! grader's callbacks under the `Serving` guard. Linked with the untouched entry, which keeps its `main`.

use super::{arguments, banner, dispatch::dispatcher, proxy, with_role};
use crate::classify::Role;
use crate::model::{Function, Interface, Ret};

pub fn jury_glue(interface: &Interface, roles: &[Role], header: &str) -> String {
    let mut out = banner(header);
    out += "\nstatic void qhhoj_serve(qhhoj::Request& qhhoj_request);\n";
    for function in with_role(interface, roles, Role::Contestant) {
        out += &format!("\n{}", proxy(function, ", qhhoj_serve"));
    }
    let served = with_role(interface, roles, Role::Callback);
    out + "\n" + &dispatcher(&served, serve_callback)
}

/// The callback runs inside `try` with the runtime in its serving state for that function; a throw is recorded and aborts. The guard lives
/// inside `try`, so its destructor restores the page state before `callback_threw` and before the RETURN is posted.
fn serve_callback(function: &Function) -> String {
    let id = function.id;
    let call = format!("{}({})", function.name, arguments(function));
    let (declare, run, reply) = match &function.ret {
        Ret::Void => (String::new(), format!("{call};"), format!("qhhoj::reply({id});")),
        Ret::Value(ty) => (
            format!("      {t} qhhoj_out = {t}();\n", t = ty.cpp()),
            format!("qhhoj_out = {call};"),
            format!("qhhoj::reply({id}, qhhoj_out);"),
        ),
        Ret::ConstRef(ty) => (
            format!("      const {}* qhhoj_out = 0;\n", ty.cpp()),
            format!("qhhoj_out = &{call};"),
            format!("qhhoj::reply_cached({id}, qhhoj_out);"),
        ),
    };
    format!(
        "{declare}      try {{\n        qhhoj::Serving qhhoj_serving({id});\n        {run}\n      }} catch (...) {{\n        \
         qhhoj::callback_threw({id});\n      }}\n      {reply}\n"
    )
}
