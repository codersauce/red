//! Unified index for stdlib methods.
//!
//! This module provides a single source of truth for stdlib method information,
//! enabling both semantic analysis and code generation to query method signatures,
//! JS names, and type inference strategies without hardcoded match statements.

use std::collections::HashMap;

use husk_ast::{
    Attribute, File, ImplBlock, ItemKind, SelfReceiver, TraitDef, TraitItemKind, TypeExpr,
    TypeExprKind,
};

/// Lookup key for stdlib methods
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MethodKey {
    /// The trait or type name (e.g., "Iterator", "String", "[T]")
    pub trait_or_type: String,
    /// The method name (e.g., "map", "len")
    pub method_name: String,
}

/// How to infer types for methods with closure parameters
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum InferenceStrategy {
    /// No special inference needed
    #[default]
    Standard,

    /// Iterator adaptor: closure T -> U, returns Iterator<U>
    /// Used by: map
    MapLike,

    /// Iterator filter: closure &T -> bool, returns Iterator<T>
    /// Used by: filter
    FilterLike,

    /// Find element: closure &T -> bool, returns Option<T>
    /// Used by: find
    FindLike,

    /// Iterator consumer: closure T -> (), returns ()
    /// Used by: for_each
    ConsumerLike,

    /// Fold pattern: closure (Acc, T) -> Acc, returns Acc
    /// Used by: fold
    FoldLike,

    /// Returns same iterator type (no closure inference needed)
    /// Used by: take, skip, enumerate, chain, zip
    PassThrough,

    /// Filter + map combined: closure T -> Option<U>, returns Iterator<U>
    /// Used by: filter_map
    FilterMapLike,

    /// Predicate consumer: closure &T -> bool, returns bool
    /// Used by: all, any
    PredicateLike,

    /// Consumer returning count: returns i32
    /// Used by: count
    CountLike,

    /// Collect into array: returns [T]
    /// Used by: collect
    CollectLike,
}

/// Information about a stdlib method extracted from the AST
#[derive(Debug, Clone)]
pub struct StdlibMethodInfo {
    /// Method receiver (self, &self, &mut self, or None for static)
    pub receiver: Option<SelfReceiver>,

    /// Parameter types (name, type)
    pub params: Vec<(String, TypeExpr)>,

    /// Return type (None means unit/void)
    pub return_type: Option<TypeExpr>,

    /// The JS name from #[js_name = "..."] attribute
    pub js_name: Option<String>,

    /// Whether this is an `extern "js" fn` declaration
    pub is_extern_js: bool,

    /// Type inference strategy for closure parameters
    pub inference_strategy: InferenceStrategy,
}

/// The unified index for all stdlib methods
#[derive(Debug, Default)]
pub struct StdlibIndex {
    /// All indexed methods by (trait/type, method_name)
    methods: HashMap<MethodKey, StdlibMethodInfo>,

    /// Trait type parameters for generic substitution
    trait_type_params: HashMap<String, Vec<String>>,
}

impl StdlibIndex {
    /// Build index from parsed stdlib file
    pub fn from_file(file: &File) -> Self {
        let mut index = Self::default();

        for item in &file.items {
            match &item.kind {
                ItemKind::Trait(trait_def) => {
                    index.index_trait(trait_def);
                }
                ItemKind::Impl(impl_block) => {
                    index.index_impl(impl_block);
                }
                _ => {}
            }
        }

        index
    }

    fn index_trait(&mut self, trait_def: &TraitDef) {
        let trait_name = &trait_def.name.name;

        // Store type parameters for generic substitution
        let type_params: Vec<String> = trait_def
            .type_params
            .iter()
            .map(|p| p.name.name.clone())
            .collect();
        self.trait_type_params
            .insert(trait_name.clone(), type_params);

        for item in &trait_def.items {
            let TraitItemKind::Method(method) = &item.kind;

            let key = MethodKey {
                trait_or_type: trait_name.clone(),
                method_name: method.name.name.clone(),
            };

            let info = StdlibMethodInfo {
                receiver: method.receiver,
                params: method
                    .params
                    .iter()
                    .map(|p| (p.name.name.clone(), p.ty.clone()))
                    .collect(),
                return_type: method.ret_type.clone(),
                js_name: Self::extract_js_name(&method.attributes),
                is_extern_js: method.is_extern,
                inference_strategy: Self::determine_inference_strategy(
                    &method.name.name,
                    &method.attributes,
                    &method.ret_type,
                ),
            };

            self.methods.insert(key, info);
        }
    }

    fn index_impl(&mut self, impl_block: &ImplBlock) {
        // Get the type name this impl is for
        let type_name = Self::type_expr_to_name(&impl_block.self_ty);

        for item in &impl_block.items {
            if let husk_ast::ImplItemKind::Method(method) = &item.kind {
                let key = MethodKey {
                    trait_or_type: type_name.clone(),
                    method_name: method.name.name.clone(),
                };

                let info = StdlibMethodInfo {
                    receiver: method.receiver,
                    params: method
                        .params
                        .iter()
                        .map(|p| (p.name.name.clone(), p.ty.clone()))
                        .collect(),
                    return_type: method.ret_type.clone(),
                    js_name: Self::extract_js_name(&method.attributes),
                    is_extern_js: method.is_extern,
                    inference_strategy: InferenceStrategy::Standard,
                };

                self.methods.insert(key, info);
            }
        }
    }

    fn type_expr_to_name(ty: &TypeExpr) -> String {
        match &ty.kind {
            TypeExprKind::Named(ident) => ident.name.clone(),
            TypeExprKind::Generic { name, .. } => name.name.clone(),
            TypeExprKind::Array(_) => "[T]".to_string(),
            TypeExprKind::Tuple(_) => "(T)".to_string(),
            _ => "Unknown".to_string(),
        }
    }

    fn extract_js_name(attrs: &[Attribute]) -> Option<String> {
        attrs
            .iter()
            .find(|a| a.name.name == "js_name")
            .and_then(|a| a.value.clone())
    }

    fn determine_inference_strategy(
        method_name: &str,
        attrs: &[Attribute],
        return_type: &Option<TypeExpr>,
    ) -> InferenceStrategy {
        // Check for explicit #[infer_closure(...)] attribute first
        if let Some(attr) = attrs.iter().find(|a| a.name.name == "infer_closure") {
            return Self::parse_inference_attr(attr);
        }

        // Fall back to method name matching for backward compatibility
        match method_name {
            "map" => InferenceStrategy::MapLike,
            "filter" => InferenceStrategy::FilterLike,
            "find" => InferenceStrategy::FindLike,
            "for_each" => InferenceStrategy::ConsumerLike,
            "fold" => InferenceStrategy::FoldLike,
            "take" | "skip" | "enumerate" => InferenceStrategy::PassThrough,
            "filter_map" => InferenceStrategy::FilterMapLike,
            "all" | "any" => InferenceStrategy::PredicateLike,
            "count" => InferenceStrategy::CountLike,
            "collect" => InferenceStrategy::CollectLike,
            "zip" | "chain" => InferenceStrategy::PassThrough,
            _ => {
                // Check if return type indicates an iterator
                if let Some(ret_ty) = return_type
                    && Self::is_iterator_type(ret_ty)
                {
                    return InferenceStrategy::PassThrough;
                }
                InferenceStrategy::Standard
            }
        }
    }

    fn parse_inference_attr(attr: &Attribute) -> InferenceStrategy {
        match attr.value.as_deref() {
            Some("map_like") => InferenceStrategy::MapLike,
            Some("filter_like") => InferenceStrategy::FilterLike,
            Some("find_like") => InferenceStrategy::FindLike,
            Some("consumer_like") => InferenceStrategy::ConsumerLike,
            Some("fold_like") => InferenceStrategy::FoldLike,
            Some("pass_through") => InferenceStrategy::PassThrough,
            Some("filter_map_like") => InferenceStrategy::FilterMapLike,
            Some("predicate_like") => InferenceStrategy::PredicateLike,
            Some("count_like") => InferenceStrategy::CountLike,
            Some("collect_like") => InferenceStrategy::CollectLike,
            _ => InferenceStrategy::Standard,
        }
    }

    /// Check if a type expression represents an iterator type
    fn is_iterator_type(ty: &TypeExpr) -> bool {
        match &ty.kind {
            TypeExprKind::ImplTrait { trait_ty } => {
                // Check if the trait is Iterator
                match &trait_ty.kind {
                    TypeExprKind::Named(ident) => ident.name == "Iterator",
                    TypeExprKind::Generic { name, .. } => name.name == "Iterator",
                    _ => false,
                }
            }
            TypeExprKind::Named(ident) => ident.name == "Iterator",
            TypeExprKind::Generic { name, .. } => name.name == "Iterator",
            _ => false,
        }
    }

    // ========================================================================
    // Query API
    // ========================================================================

    /// Look up a method by trait/type and name
    pub fn get_method(&self, trait_or_type: &str, method_name: &str) -> Option<&StdlibMethodInfo> {
        let key = MethodKey {
            trait_or_type: trait_or_type.to_string(),
            method_name: method_name.to_string(),
        };
        self.methods.get(&key)
    }

    /// Get the JS name for a method (for codegen)
    pub fn get_js_name(&self, trait_or_type: &str, method_name: &str) -> Option<&str> {
        self.get_method(trait_or_type, method_name)
            .and_then(|info| info.js_name.as_deref())
    }

    /// Check if a method returns an iterator (for codegen chaining)
    pub fn returns_iterator(&self, trait_or_type: &str, method_name: &str) -> bool {
        self.get_method(trait_or_type, method_name)
            .map(|info| {
                if let Some(ret) = &info.return_type {
                    Self::is_iterator_type(ret)
                } else {
                    false
                }
            })
            .unwrap_or(false)
    }

    /// Get inference strategy for type checking
    pub fn get_inference_strategy(
        &self,
        trait_or_type: &str,
        method_name: &str,
    ) -> InferenceStrategy {
        self.get_method(trait_or_type, method_name)
            .map(|info| info.inference_strategy.clone())
            .unwrap_or_default()
    }

    /// Get type parameters for a trait (e.g., ["T"] for Iterator<T>)
    pub fn get_trait_type_params(&self, trait_name: &str) -> Option<&[String]> {
        self.trait_type_params.get(trait_name).map(|v| v.as_slice())
    }

    /// Check if a method exists in the index
    pub fn has_method(&self, trait_or_type: &str, method_name: &str) -> bool {
        self.get_method(trait_or_type, method_name).is_some()
    }

    /// Get all method names for a trait/type (useful for completions)
    pub fn get_methods_for_type(&self, trait_or_type: &str) -> Vec<&str> {
        self.methods
            .iter()
            .filter(|(k, _)| k.trait_or_type == trait_or_type)
            .map(|(k, _)| k.method_name.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use husk_parser::parse_str;

    fn parse_stdlib() -> StdlibIndex {
        let source = include_str!("stdlib/core.hk");
        let parsed = parse_str(source);
        assert!(
            parsed.errors.is_empty(),
            "Parse errors: {:?}",
            parsed.errors
        );
        StdlibIndex::from_file(&parsed.file.unwrap())
    }

    #[test]
    fn test_iterator_methods_indexed() {
        let index = parse_stdlib();

        // All iterator methods should be indexed
        assert!(index.has_method("Iterator", "map"));
        assert!(index.has_method("Iterator", "filter"));
        assert!(index.has_method("Iterator", "collect"));
        assert!(index.has_method("Iterator", "count"));
        assert!(index.has_method("Iterator", "zip"));
        assert!(index.has_method("Iterator", "chain"));
        assert!(index.has_method("Iterator", "all"));
        assert!(index.has_method("Iterator", "any"));
        assert!(index.has_method("Iterator", "filter_map"));
    }

    #[test]
    fn test_js_names_extracted() {
        let index = parse_stdlib();

        assert_eq!(
            index.get_js_name("Iterator", "map"),
            Some("__husk_iterator_map")
        );
        assert_eq!(
            index.get_js_name("Iterator", "count"),
            Some("__husk_iterator_count")
        );
        assert_eq!(
            index.get_js_name("Iterator", "filter"),
            Some("__husk_iterator_filter")
        );
    }

    #[test]
    fn test_inference_strategies() {
        let index = parse_stdlib();

        assert_eq!(
            index.get_inference_strategy("Iterator", "map"),
            InferenceStrategy::MapLike
        );
        assert_eq!(
            index.get_inference_strategy("Iterator", "filter"),
            InferenceStrategy::FilterLike
        );
        assert_eq!(
            index.get_inference_strategy("Iterator", "count"),
            InferenceStrategy::CountLike
        );
        assert_eq!(
            index.get_inference_strategy("Iterator", "all"),
            InferenceStrategy::PredicateLike
        );
        assert_eq!(
            index.get_inference_strategy("Iterator", "any"),
            InferenceStrategy::PredicateLike
        );
    }

    #[test]
    fn test_returns_iterator() {
        let index = parse_stdlib();

        // Methods that return iterators
        assert!(index.returns_iterator("Iterator", "map"));
        assert!(index.returns_iterator("Iterator", "filter"));
        assert!(index.returns_iterator("Iterator", "take"));
        assert!(index.returns_iterator("Iterator", "skip"));
        assert!(index.returns_iterator("Iterator", "enumerate"));
        assert!(index.returns_iterator("Iterator", "zip"));
        assert!(index.returns_iterator("Iterator", "chain"));
        assert!(index.returns_iterator("Iterator", "filter_map"));

        // Methods that don't return iterators
        assert!(!index.returns_iterator("Iterator", "collect"));
        assert!(!index.returns_iterator("Iterator", "count"));
        assert!(!index.returns_iterator("Iterator", "all"));
        assert!(!index.returns_iterator("Iterator", "any"));
    }

    #[test]
    fn test_string_methods_indexed() {
        let index = parse_stdlib();

        assert!(index.has_method("String", "len"));
        assert!(index.has_method("String", "trim"));
        assert!(index.has_method("String", "split"));

        // Check JS name override for len -> length
        assert_eq!(index.get_js_name("String", "len"), Some("length"));
    }

    #[test]
    fn test_trait_type_params() {
        let index = parse_stdlib();

        let params = index.get_trait_type_params("Iterator");
        assert_eq!(params, Some(&["T".to_string()][..]));
    }
}
