//! Type system for BlumScript.

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    Void,
    Bool,
    Char,
    Byte,
    Int,
    UInt,
    Float,
    String,
    Ptr(Box<Type>),
    Array(Box<Type>, usize),
    Named(String),
}

impl Type {
    /// C type name used in generated code.
    pub fn c(&self) -> String {
        match self {
            Type::Void => "void".into(),
            Type::Bool => "bool".into(),
            Type::Char => "char".into(),
            Type::Byte => "uint8_t".into(),
            Type::Int => "int64_t".into(),
            Type::UInt => "uint64_t".into(),
            Type::Float => "double".into(),
            Type::String => "char*".into(),
            Type::Ptr(t) => format!("{}*", t.c()),
            Type::Array(t, n) => format!("{}[{}]", t.c(), n),
            Type::Named(n) => n.clone(),
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Type::Int | Type::UInt | Type::Float | Type::Char | Type::Byte
        )
    }

    pub fn is_intlike(&self) -> bool {
        matches!(self, Type::Int | Type::UInt | Type::Char | Type::Byte | Type::Bool)
    }

    pub fn is_pointer(&self) -> bool {
        matches!(self, Type::Ptr(_) | Type::String)
    }

    pub fn pointee(&self) -> Option<Type> {
        match self {
            Type::Ptr(t) => Some((**t).clone()),
            Type::String => Some(Type::Char),
            _ => None,
        }
    }

    /// Suffix used to pick the runtime printer.
    pub fn print_suffix(&self) -> &'static str {
        match self {
            Type::Int => "int",
            Type::UInt | Type::Byte => "uint",
            Type::Float => "float",
            Type::Bool => "bool",
            Type::Char => "char",
            Type::String => "str",
            Type::Ptr(_) => "ptr",
            Type::Named(_) => "ptr",
            Type::Array(_, _) => "ptr",
            Type::Void => "ptr",
        }
    }

    pub fn display(&self) -> String {
        match self {
            Type::Ptr(t) => format!("*{}", t.display()),
            Type::Array(t, n) => format!("{}[{}]", t.display(), n),
            other => other.c(),
        }
    }
}
