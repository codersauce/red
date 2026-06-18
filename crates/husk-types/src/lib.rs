//! Core type system representations for Husk.
//!
//! This crate defines the internal type language used by the type checker.

/// Primitive types built into Husk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimitiveType {
    I32,
    I64,
    F64,
    Bool,
    String,
    Unit,
}

/// Identifier for a type variable used during type checking / inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeVarId(pub u32);

/// A type in Husk's core type language.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// A primitive type such as `i32`, `bool`, `String`, or `()`.
    Primitive(PrimitiveType),
    /// A named, possibly generic type such as `Option<T>` or `Result<T, E>`.
    Named {
        /// The name of the type constructor (e.g., "Option", "Result", "MyType").
        name: String,
        /// Type arguments applied to the constructor.
        args: Vec<Type>,
    },
    /// A function type: `(T1, T2, ...) -> R`.
    Function { params: Vec<Type>, ret: Box<Type> },
    /// A type variable introduced during type checking.
    Var(TypeVarId),
    /// An array type: `[T]`.
    Array(Box<Type>),
    /// A tuple type: `(T1, T2, ...)`.
    Tuple(Vec<Type>),
    /// An impl Trait type: `impl Iterator<T>` - used for return types
    /// Stores the underlying trait type for resolution
    ImplTrait {
        /// The trait type (e.g., `Iterator<i32>`)
        trait_ty: Box<Type>,
    },
}

impl Type {
    pub fn i32() -> Self {
        Type::Primitive(PrimitiveType::I32)
    }

    pub fn i64() -> Self {
        Type::Primitive(PrimitiveType::I64)
    }

    pub fn f64() -> Self {
        Type::Primitive(PrimitiveType::F64)
    }

    pub fn bool() -> Self {
        Type::Primitive(PrimitiveType::Bool)
    }

    pub fn string() -> Self {
        Type::Primitive(PrimitiveType::String)
    }

    pub fn unit() -> Self {
        Type::Primitive(PrimitiveType::Unit)
    }

    pub fn named(name: impl Into<String>, args: Vec<Type>) -> Self {
        Type::Named {
            name: name.into(),
            args,
        }
    }

    pub fn function(params: Vec<Type>, ret: Type) -> Self {
        Type::Function {
            params,
            ret: Box::new(ret),
        }
    }

    pub fn tuple(elements: Vec<Type>) -> Self {
        Type::Tuple(elements)
    }

    pub fn impl_trait(trait_ty: Type) -> Self {
        Type::ImplTrait {
            trait_ty: Box::new(trait_ty),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_constructors_work() {
        assert_eq!(Type::i32(), Type::Primitive(PrimitiveType::I32));
        assert_eq!(Type::i64(), Type::Primitive(PrimitiveType::I64));
        assert_eq!(Type::bool(), Type::Primitive(PrimitiveType::Bool));
        assert_eq!(Type::string(), Type::Primitive(PrimitiveType::String));
        assert_eq!(Type::unit(), Type::Primitive(PrimitiveType::Unit));
    }

    #[test]
    fn named_type_with_args() {
        let t = Type::named("Option", vec![Type::string()]);
        match t {
            Type::Named { name, args } => {
                assert_eq!(name, "Option");
                assert_eq!(args.len(), 1);
                assert_eq!(args[0], Type::string());
            }
            _ => panic!("expected named type"),
        }
    }

    #[test]
    fn function_type() {
        let t = Type::function(vec![Type::i32(), Type::bool()], Type::string());
        match t {
            Type::Function { params, ret } => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0], Type::i32());
                assert_eq!(params[1], Type::bool());
                assert_eq!(*ret, Type::string());
            }
            _ => panic!("expected function type"),
        }
    }

    #[test]
    fn tuple_type() {
        let t = Type::tuple(vec![Type::i32(), Type::string(), Type::bool()]);
        match t {
            Type::Tuple(elements) => {
                assert_eq!(elements.len(), 3);
                assert_eq!(elements[0], Type::i32());
                assert_eq!(elements[1], Type::string());
                assert_eq!(elements[2], Type::bool());
            }
            _ => panic!("expected tuple type"),
        }
    }

    #[test]
    fn impl_trait_type() {
        let inner = Type::named("Iterator", vec![Type::i32()]);
        let t = Type::impl_trait(inner.clone());
        match t {
            Type::ImplTrait { trait_ty } => {
                assert_eq!(*trait_ty, inner);
            }
            _ => panic!("expected impl trait type"),
        }
    }
}
