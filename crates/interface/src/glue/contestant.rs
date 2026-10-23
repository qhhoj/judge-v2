//! `qhhoj_contestant.cpp`: proxies for the callbacks the contestant calls, a dispatcher for the contestant
//! functions the jury calls, and `main`. Linked with the untouched submission (its `main` renamed).

use super::{arguments, banner, dispatch::dispatcher, proxy, with_role};
use crate::classify::Role;
use crate::model::{Function, Interface, Ret};

pub fn contestant_glue(interface: &Interface, roles: &[Role], header: &str) -> String {
    let mut out = banner(header);
    for callback in with_role(interface, roles, Role::Callback) {
        out += &format!("\n{}", proxy(callback, ""));
    }
    let served = with_role(interface, roles, Role::Contestant);
    out += &format!("\n{}", dispatcher(&served, serve_contestant_function));
    out + "\nint main() { return qhhoj::contestant_main(qhhoj_serve); }\n"
}

fn serve_contestant_function(function: &Function) -> String {
    let call = format!("{}({})", function.name, arguments(function));
    match function.ret {
        Ret::Void => format!("      {call};\n      qhhoj::reply({});\n", function.id),
        _ => format!("      qhhoj::reply({}, {call});\n", function.id),
    }
}
