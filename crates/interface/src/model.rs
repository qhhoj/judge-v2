//! The interface model: what a header declares, in declaration order. `cpp` spells a type for generated
//! C++03 code (`> >`), `short` for the interface log.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    /// `#include <...>` names, in order (the glue includes the header itself, never these).
    pub includes: Vec<String>,
    pub functions: Vec<Function>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// Declaration order, from 0: the function number on the wire.
    pub id: u16,
    pub name: String,
    pub ret: Ret,
    pub params: Vec<Param>,
    /// Line of the declaration's first token, for problem-level refusals.
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ret {
    Void,
    Value(Type),
    /// `const T&`: callbacks only; the contestant keeps a refreshed copy.
    ConstRef(Type),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub ty: Type,
    pub pass: Pass,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Value,
    ConstValue,
    ConstRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Int,
    LongLong,
    Double,
    Bool,
    String,
    Vector(Box<Type>),
    Pair(Box<Type>, Box<Type>),
}

impl Type {
    pub fn cpp(&self) -> String {
        match self {
            Type::Vector(item) => close_cpp(format!("std::vector<{}", item.cpp())),
            Type::Pair(first, second) => close_cpp(format!("std::pair<{}, {}", first.cpp(), second.cpp())),
            scalar => scalar.scalar_name("std::string"),
        }
    }

    pub fn short(&self) -> String {
        match self {
            Type::Vector(item) => format!("vector<{}>", item.short()),
            Type::Pair(first, second) => format!("pair<{}, {}>", first.short(), second.short()),
            scalar => scalar.scalar_name("string"),
        }
    }

    /// 1 for a scalar; one more per template level.
    pub fn depth(&self) -> usize {
        match self {
            Type::Vector(item) => 1 + item.depth(),
            Type::Pair(first, second) => 1 + first.depth().max(second.depth()),
            _ => 1,
        }
    }

    fn scalar_name(&self, string: &str) -> String {
        let name = match self {
            Type::Int => "int",
            Type::LongLong => "long long",
            Type::Double => "double",
            Type::Bool => "bool",
            _ => string,
        };
        name.to_string()
    }
}

/// C++03 needs `> >`: a closing `>` right after another one is spaced.
fn close_cpp(open: String) -> String {
    if open.ends_with('>') { format!("{open} >") } else { format!("{open}>") }
}

impl Ret {
    pub fn cpp(&self) -> String {
        match self {
            Ret::Void => "void".into(),
            Ret::Value(ty) => ty.cpp(),
            Ret::ConstRef(ty) => format!("const {}&", ty.cpp()),
        }
    }

    pub fn short(&self) -> String {
        match self {
            Ret::Void => "void".into(),
            Ret::Value(ty) => ty.short(),
            Ret::ConstRef(ty) => format!("const {}&", ty.short()),
        }
    }
}

impl Param {
    pub fn cpp(&self, name: &str) -> String {
        match self.pass {
            Pass::Value => format!("{} {name}", self.ty.cpp()),
            Pass::ConstValue => format!("const {} {name}", self.ty.cpp()),
            Pass::ConstRef => format!("const {}& {name}", self.ty.cpp()),
        }
    }

    pub fn short(&self) -> String {
        match self.pass {
            Pass::ConstRef => format!("const {}&", self.ty.short()),
            Pass::Value | Pass::ConstValue => self.ty.short(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_spell_as_cplusplus03_and_as_short_log_text() {
        let nested = Type::Vector(Box::new(Type::Vector(Box::new(Type::Int))));
        assert_eq!(nested.cpp(), "std::vector<std::vector<int> >");
        assert_eq!(nested.short(), "vector<vector<int>>");
        let pair = Type::Pair(Box::new(Type::LongLong), Box::new(Type::String));
        assert_eq!(pair.cpp(), "std::pair<long long, std::string>");
        assert_eq!(pair.short(), "pair<long long, string>");
        assert_eq!(nested.depth(), 3);
        assert_eq!(Type::Bool.depth(), 1);
    }

    #[test]
    fn parameters_and_returns_spell_their_passing() {
        let vector = Type::Vector(Box::new(Type::Bool));
        let by_ref = Param { ty: vector.clone(), pass: Pass::ConstRef, name: Some("mask".into()) };
        assert_eq!(by_ref.cpp("qhhoj_a0"), "const std::vector<bool>& qhhoj_a0");
        assert_eq!(by_ref.short(), "const vector<bool>&");
        let by_const = Param { ty: Type::Int, pass: Pass::ConstValue, name: None };
        assert_eq!(by_const.cpp("qhhoj_a1"), "const int qhhoj_a1");
        assert_eq!(Ret::ConstRef(vector).cpp(), "const std::vector<bool>&");
        assert_eq!(Ret::Void.short(), "void");
    }
}
