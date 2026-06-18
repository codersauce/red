//! Parser: consume tokens and produce an AST.
//!
//! This is a hand-written recursive-descent parser for the MVP syntax.

use husk_ast::{
    AssignOp, Attribute, BinaryOp, Block, CfgPredicate, ClosureParam, EnumVariant,
    EnumVariantFields, Expr, ExprKind, ExternProperty, File, FormatPlaceholder, FormatSegment,
    FormatSpec, FormatString, Ident, ImplBlock, ImplItem, ImplItemKind, ImplMethod, Item, ItemKind,
    Literal, LiteralKind, MatchArm, Param, Pattern, PatternKind, SelfReceiver, Span, Stmt,
    StmtKind, StructField, TraitDef, TraitItem, TraitItemKind, TraitMethod, TypeExpr, TypeExprKind,
    TypeParam,
};
use husk_lexer::{Keyword, Lexer, Token, TokenKind};

fn debug_log(msg: &str) {
    match std::env::var("HUSKC_DEBUG") {
        Ok(val) if val == "1" || val.eq_ignore_ascii_case("true") => {
            eprintln!("{msg}");
        }
        _ => {}
    }
}

// Removed DepthGuard - using thread_local instead

#[derive(Debug)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

/// State machine for parsing JavaScript content with proper handling of
/// strings, comments, and nested braces.
#[derive(Debug, Clone, Copy)]
enum JsParseState {
    Normal,
    InSingleQuoteString,
    InDoubleQuoteString,
    InTemplateString { template_depth: usize },
    InSingleLineComment,
    InMultiLineComment,
}

#[derive(Debug)]
pub struct ParseResult {
    pub file: Option<File>,
    pub errors: Vec<ParseError>,
    /// The tokens from lexing, useful for accessing trivia (comments).
    pub tokens: Vec<Token>,
}

/// Parse a source string into an AST `File` and a list of parse errors.
pub fn parse_str(source: &str) -> ParseResult {
    debug_log("[huskc-parser] lexing");
    let tokens: Vec<Token> = Lexer::new(source).collect();
    debug_log(&format!("[huskc-parser] lexed {} tokens", tokens.len()));
    // Parser borrows tokens - no cloning needed
    let mut parser = Parser::new(&tokens, source);
    let file = parser.parse_file();
    ParseResult {
        file: Some(file),
        errors: parser.errors,
        tokens,
    }
}

struct Parser<'src> {
    tokens: &'src [Token],
    pos: usize,
    pub errors: Vec<ParseError>,
    /// When false, struct literal expressions like `Name { ... }` are not allowed.
    /// This is set to false when parsing contexts where `{` has special meaning
    /// (e.g., match/if/while scrutinee).
    allow_struct_expr: bool,
    /// The original source text, used for extracting raw content (e.g., js blocks).
    source: &'src str,
}

impl<'src> Parser<'src> {
    fn new(tokens: &'src [Token], source: &'src str) -> Self {
        Self {
            tokens,
            pos: 0,
            errors: Vec::new(),
            allow_struct_expr: true,
            source,
        }
    }

    /// Parse an expression in a context where struct literals are not allowed
    /// (e.g., match/if/while scrutinee where `{` starts a block).
    fn parse_expr_no_struct(&mut self) -> Option<Expr> {
        let saved = self.allow_struct_expr;
        self.allow_struct_expr = false;
        let result = self.parse_expr();
        self.allow_struct_expr = saved;
        result
    }

    fn is_at_end(&self) -> bool {
        matches!(self.current().kind, TokenKind::Eof)
    }

    fn current(&self) -> &Token {
        self.tokens
            .get(self.pos)
            .unwrap_or(self.tokens.last().expect("tokens not empty"))
    }

    fn previous(&self) -> &Token {
        if self.pos == 0 {
            self.current()
        } else {
            &self.tokens[self.pos - 1]
        }
    }

    fn advance(&mut self) -> &Token {
        if !self.is_at_end() {
            self.pos += 1;
        }
        self.previous()
    }

    fn matches_keyword(&mut self, kw: Keyword) -> bool {
        match self.current().kind {
            TokenKind::Keyword(k) if k == kw => {
                self.advance();
                true
            }
            _ => false,
        }
    }

    fn matches_token(&mut self, kind: &TokenKind) -> bool {
        if std::mem::discriminant(&self.current().kind) == std::mem::discriminant(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn ast_span_from(&self, span: &husk_lexer::Span) -> Span {
        Span {
            range: span.range.clone(),
            file: None,
        }
    }

    /// Find the first token index whose span starts at or after `byte_pos`.
    /// Uses binary search since token spans are monotonically increasing.
    fn find_token_at_or_after(&self, byte_pos: usize) -> usize {
        let mut lo = self.pos;
        let mut hi = self.tokens.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.tokens[mid].span.range.start < byte_pos {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn error_at_token(&mut self, token: &Token, message: impl Into<String>) {
        let span = self.ast_span_from(&token.span);
        self.errors.push(ParseError {
            message: message.into(),
            span,
        });
    }

    fn error_here(&mut self, message: impl Into<String>) {
        let tok = self.current().clone();
        self.error_at_token(&tok, message);
    }

    /// Synchronize after an error by skipping tokens until we reach a likely
    /// item boundary.
    fn synchronize_item(&mut self) {
        while !self.is_at_end() {
            match &self.current().kind {
                TokenKind::Semicolon => {
                    self.advance();
                    return;
                }
                TokenKind::Keyword(
                    Keyword::Fn
                    | Keyword::Struct
                    | Keyword::Enum
                    | Keyword::Type
                    | Keyword::Extern
                    | Keyword::Use
                    | Keyword::Pub
                    | Keyword::Trait
                    | Keyword::Impl,
                ) => {
                    return;
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    fn parse_file(&mut self) -> File {
        let mut items = Vec::new();
        while !self.is_at_end() {
            if let Some(item) = self.parse_item() {
                items.push(item);
            } else {
                self.synchronize_item();
            }
        }
        File { items }
    }

    fn parse_item(&mut self) -> Option<Item> {
        // Parse any leading attributes (e.g., #[test], #[cfg(test)], #[ignore])
        let attributes = self.parse_attributes();

        // Capture start position before visibility for span calculation
        let item_start = self.current().span.range.start;

        let mut visibility = husk_ast::Visibility::Private;
        if self.matches_keyword(Keyword::Pub) {
            visibility = husk_ast::Visibility::Public;
        }

        let mut item = match self.current().kind {
            TokenKind::Keyword(Keyword::Use) => self.parse_use_item(),
            TokenKind::Keyword(Keyword::Fn) => self.parse_fn_item(),
            TokenKind::Keyword(Keyword::Struct) => self.parse_struct_item(),
            TokenKind::Keyword(Keyword::Enum) => self.parse_enum_item(),
            TokenKind::Keyword(Keyword::Type) => self.parse_type_alias_item(),
            TokenKind::Keyword(Keyword::Extern) => self.parse_extern_block_item(),
            TokenKind::Keyword(Keyword::Trait) => self.parse_trait_item(),
            TokenKind::Keyword(Keyword::Impl) => self.parse_impl_item(),
            TokenKind::Eof => None,
            _ => {
                self.error_here(
                    "expected item (`fn`, `struct`, `enum`, `type`, `extern`, `use`, `trait`, or `impl`)",
                );
                None
            }
        }?;

        // Update span to start from visibility keyword if present
        if visibility == husk_ast::Visibility::Public {
            item.span = Span {
                range: item_start..item.span.range.end,
                file: None,
            };
        }

        item.attributes = attributes;
        item.visibility = visibility;
        Some(item)
    }

    fn parse_fn_item(&mut self) -> Option<Item> {
        let fn_tok = self.advance().clone(); // consume `fn`
        let name = self.parse_ident("expected function name after `fn`")?;

        let type_params = self.parse_type_params_with_bounds();

        // Parameter list
        if !self.matches_token(&TokenKind::LParen) {
            self.error_here("expected `(` after function name");
            return None;
        }
        let params = self.parse_param_list();

        // Optional return type
        let ret_type = if self.matches_token(&TokenKind::Arrow) {
            Some(self.parse_type_expr()?)
        } else {
            None
        };

        let body_block = self.parse_block()?;
        let span = Span {
            range: fn_tok.span.range.start..body_block.span.range.end,
            file: None,
        };

        Some(Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Fn {
                name,
                type_params,
                params,
                ret_type,
                body: body_block.stmts,
            },
            span,
        })
    }

    fn parse_struct_item(&mut self) -> Option<Item> {
        let struct_tok = self.advance().clone(); // consume `struct`
        let name = self.parse_ident("expected struct name")?;
        let type_params = self.parse_type_params();

        if !self.matches_token(&TokenKind::LBrace) {
            self.error_here("expected `{` after struct name");
            return None;
        }

        let mut fields = Vec::new();
        while !self.is_at_end() && !self.matches_token(&TokenKind::RBrace) {
            let field_name = self.parse_ident("expected field name in struct")?;
            if !self.matches_token(&TokenKind::Colon) {
                self.error_here("expected `:` after field name");
                return None;
            }
            let ty = self.parse_type_expr()?;
            fields.push(StructField {
                name: field_name,
                ty,
            });

            // Optional trailing comma
            let _ = self.matches_token(&TokenKind::Comma);
        }

        let end = self.previous().span.range.end;
        let span = Span {
            range: struct_tok.span.range.start..end,
            file: None,
        };

        Some(Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Struct {
                name,
                type_params,
                fields,
            },
            span,
        })
    }

    fn parse_enum_item(&mut self) -> Option<Item> {
        let enum_tok = self.advance().clone(); // consume `enum`
        let name = self.parse_ident("expected enum name")?;
        let type_params = self.parse_type_params();

        if !self.matches_token(&TokenKind::LBrace) {
            self.error_here("expected `{` after enum name");
            return None;
        }

        let mut variants = Vec::new();
        while !self.is_at_end() && !self.matches_token(&TokenKind::RBrace) {
            let variant_name = self.parse_ident("expected enum variant name")?;

            let fields = if self.matches_token(&TokenKind::LParen) {
                // Tuple-like variant
                let mut tys = Vec::new();
                if !self.matches_token(&TokenKind::RParen) {
                    loop {
                        let ty = self.parse_type_expr()?;
                        tys.push(ty);
                        if self.matches_token(&TokenKind::RParen) {
                            break;
                        }
                        if !self.matches_token(&TokenKind::Comma) {
                            self.error_here("expected `,` or `)` in tuple variant");
                            return None;
                        }
                    }
                }
                EnumVariantFields::Tuple(tys)
            } else if self.matches_token(&TokenKind::LBrace) {
                // Struct-like variant
                let mut fields_vec = Vec::new();
                while !self.is_at_end() && !self.matches_token(&TokenKind::RBrace) {
                    let field_name = self.parse_ident("expected field name in enum variant")?;
                    if !self.matches_token(&TokenKind::Colon) {
                        self.error_here("expected `:` after field name in enum variant");
                        return None;
                    }
                    let ty = self.parse_type_expr()?;
                    fields_vec.push(StructField {
                        name: field_name,
                        ty,
                    });
                    let _ = self.matches_token(&TokenKind::Comma);
                }
                EnumVariantFields::Struct(fields_vec)
            } else {
                // Unit variant
                EnumVariantFields::Unit
            };

            variants.push(EnumVariant {
                name: variant_name,
                fields,
            });

            // Optional trailing comma between variants
            let _ = self.matches_token(&TokenKind::Comma);
        }

        let end = self.previous().span.range.end;
        let span = Span {
            range: enum_tok.span.range.start..end,
            file: None,
        };

        Some(Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Enum {
                name,
                type_params,
                variants,
            },
            span,
        })
    }

    fn parse_type_alias_item(&mut self) -> Option<Item> {
        let type_tok = self.advance().clone(); // consume `type`
        let name = self.parse_ident("expected type alias name")?;
        if !self.matches_token(&TokenKind::Eq) {
            self.error_here("expected `=` in type alias");
            return None;
        }
        let ty = self.parse_type_expr()?;
        if !self.matches_token(&TokenKind::Semicolon) {
            self.error_here("expected `;` after type alias");
            return None;
        }
        let end = self.previous().span.range.end;
        let span = Span {
            range: type_tok.span.range.start..end,
            file: None,
        };
        Some(Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::TypeAlias { name, ty },
            span,
        })
    }

    fn parse_extern_block_item(&mut self) -> Option<Item> {
        let extern_tok = self.advance().clone(); // consume `extern`
        // Expect "js" string literal for MVP
        let abi = if let TokenKind::StringLiteral(ref s) = self.current().kind {
            let s = s.clone();
            self.advance();
            s
        } else {
            self.error_here("expected string literal ABI after `extern` (e.g., \"js\")");
            "js".to_string()
        };

        if !self.matches_token(&TokenKind::LBrace) {
            self.error_here("expected `{` to start extern block");
            return None;
        }

        let mut items = Vec::new();
        while !self.is_at_end() && !self.matches_token(&TokenKind::RBrace) {
            // Parse any attributes before the item (e.g., #[js_name = "..."])
            let item_attributes = self.parse_attributes();

            // Check for `mod` declaration:
            // - `mod identifier;` - identifier is both package and binding
            // - `mod "package-name";` - string literal, derive binding from package
            // - `mod "package-name" as alias;` - string literal with explicit alias
            // - `mod global Name { ... }` - JS global object (no require/import)
            if self.matches_keyword(Keyword::Mod) {
                let mod_start = self.previous().span.range.start;

                // Check for `global` keyword (for JS builtins like Array, Math, JSON)
                let is_global = self.matches_keyword(Keyword::Global);

                // Parse package name (identifier or string literal)
                let (package, default_binding) =
                    if let TokenKind::StringLiteral(ref s) = self.current().kind {
                        let pkg = s.clone();
                        let binding_name = derive_binding_from_package(&pkg);
                        let tok = self.advance().clone();
                        (
                            pkg,
                            Ident {
                                name: binding_name,
                                span: Span {
                                    range: tok.span.range.clone(),
                                    file: None,
                                },
                            },
                        )
                    } else if let Some(id) =
                        self.parse_ident("expected module name or string literal after `mod`")
                    {
                        (id.name.clone(), id)
                    } else {
                        self.synchronize_item();
                        continue;
                    };

                // Check for optional `as alias`
                let binding = if self.matches_keyword(Keyword::As) {
                    match self.parse_ident("expected alias identifier after `as`") {
                        Some(alias) => alias,
                        None => {
                            self.synchronize_item();
                            continue;
                        }
                    }
                } else {
                    default_binding
                };

                // Check for block `{ ... }` with nested function declarations
                // or semicolon `;` for simple module import
                let mod_items = if self.matches_token(&TokenKind::LBrace) {
                    let mut nested = Vec::new();
                    while !self.is_at_end() && !self.matches_token(&TokenKind::RBrace) {
                        let attributes = self.parse_attributes();

                        if !self.matches_keyword(Keyword::Fn) {
                            self.error_here("expected `fn` inside mod block");
                            self.synchronize_item();
                            continue;
                        }
                        let fn_name = match self.parse_ident("expected function name in mod block")
                        {
                            Some(id) => id,
                            None => {
                                self.synchronize_item();
                                continue;
                            }
                        };

                        if !self.matches_token(&TokenKind::LParen) {
                            self.error_here("expected `(` after function name");
                            self.synchronize_item();
                            continue;
                        }
                        let fn_params = self.parse_param_list();

                        let fn_ret_type = if self.matches_token(&TokenKind::Arrow) {
                            self.parse_type_expr()
                        } else {
                            None
                        };

                        if !self.matches_token(&TokenKind::Semicolon) {
                            self.error_here("expected `;` after function declaration in mod block");
                            self.synchronize_item();
                            continue;
                        }

                        let fn_span = Span {
                            range: fn_name.span.range.start..self.previous().span.range.end,
                            file: None,
                        };
                        nested.push(husk_ast::ModItem {
                            attributes,
                            kind: husk_ast::ModItemKind::Fn {
                                name: fn_name,
                                params: fn_params,
                                ret_type: fn_ret_type,
                            },
                            span: fn_span,
                        });
                    }
                    nested
                } else if self.matches_token(&TokenKind::Semicolon) {
                    Vec::new()
                } else {
                    self.error_here("expected `{` or `;` after module declaration");
                    self.synchronize_item();
                    continue;
                };

                let span = Span {
                    range: mod_start..self.previous().span.range.end,
                    file: None,
                };
                items.push(husk_ast::ExternItem {
                    attributes: item_attributes,
                    kind: husk_ast::ExternItemKind::Mod {
                        package,
                        binding,
                        items: mod_items,
                        is_global,
                    },
                    span,
                });
                continue;
            }

            // Check for `struct` declaration (extern struct)
            if self.matches_keyword(Keyword::Struct) {
                let struct_start = self.previous().span.range.start;
                let name = match self.parse_ident("expected struct name after `struct`") {
                    Some(id) => id,
                    None => {
                        self.synchronize_item();
                        continue;
                    }
                };

                // Parse optional type parameters: struct JsArray<T>;
                let type_params = if self.matches_token(&TokenKind::Lt) {
                    let mut params = Vec::new();
                    loop {
                        if let Some(param) = self.parse_ident("expected type parameter") {
                            params.push(param);
                        }
                        if !self.matches_token(&TokenKind::Comma) {
                            break;
                        }
                    }
                    if !self.matches_token(&TokenKind::Gt) {
                        self.error_here("expected `>` after type parameters");
                    }
                    params
                } else {
                    Vec::new()
                };

                if !self.matches_token(&TokenKind::Semicolon) {
                    self.error_here("expected `;` after extern struct declaration");
                    self.synchronize_item();
                    continue;
                }

                let span = Span {
                    range: struct_start..self.previous().span.range.end,
                    file: None,
                };
                items.push(husk_ast::ExternItem {
                    attributes: item_attributes,
                    kind: husk_ast::ExternItemKind::Struct { name, type_params },
                    span,
                });
                continue;
            }

            // Check for `static` declaration (extern static variable)
            if self.matches_keyword(Keyword::Static) {
                let static_start = self.previous().span.range.start;
                let name = match self.parse_ident("expected variable name after `static`") {
                    Some(id) => id,
                    None => {
                        self.synchronize_item();
                        continue;
                    }
                };

                if !self.matches_token(&TokenKind::Colon) {
                    self.error_here("expected `:` after static variable name");
                    self.synchronize_item();
                    continue;
                }

                let ty = match self.parse_type_expr() {
                    Some(t) => t,
                    None => {
                        self.synchronize_item();
                        continue;
                    }
                };

                if !self.matches_token(&TokenKind::Semicolon) {
                    self.error_here("expected `;` after static declaration");
                    self.synchronize_item();
                    continue;
                }

                let span = Span {
                    range: static_start..self.previous().span.range.end,
                    file: None,
                };
                items.push(husk_ast::ExternItem {
                    attributes: item_attributes,
                    kind: husk_ast::ExternItemKind::Static { name, ty },
                    span,
                });
                continue;
            }

            // Check for `const` declaration (extern const value)
            if self.matches_keyword(Keyword::Const) {
                let const_start = self.previous().span.range.start;
                let name = match self.parse_ident("expected variable name after `const`") {
                    Some(id) => id,
                    None => {
                        self.synchronize_item();
                        continue;
                    }
                };

                if !self.matches_token(&TokenKind::Colon) {
                    self.error_here("expected `:` after const variable name");
                    self.synchronize_item();
                    continue;
                }

                let ty = match self.parse_type_expr() {
                    Some(t) => t,
                    None => {
                        self.synchronize_item();
                        continue;
                    }
                };

                if !self.matches_token(&TokenKind::Semicolon) {
                    self.error_here("expected `;` after const declaration");
                    self.synchronize_item();
                    continue;
                }

                let span = Span {
                    range: const_start..self.previous().span.range.end,
                    file: None,
                };
                items.push(husk_ast::ExternItem {
                    attributes: item_attributes,
                    kind: husk_ast::ExternItemKind::Const { name, ty },
                    span,
                });
                continue;
            }

            // Check for `impl` block
            if self.matches_keyword(Keyword::Impl) {
                let impl_start = self.previous().span.range.start;

                // Parse optional type parameters with bounds
                let type_params = self.parse_type_params_with_bounds();

                // Parse the type being implemented
                let self_ty = match self.parse_type_expr() {
                    Some(ty) => ty,
                    None => {
                        self.synchronize_item();
                        continue;
                    }
                };

                if !self.matches_token(&TokenKind::LBrace) {
                    self.error_here("expected `{` after impl type");
                    self.synchronize_item();
                    continue;
                }

                // Parse impl methods - treat them all as extern
                let mut impl_items = Vec::new();
                while !self.is_at_end() && !self.matches_token(&TokenKind::RBrace) {
                    let pos_before = self.pos;
                    if let Some(item) = self.parse_extern_impl_method() {
                        impl_items.push(item);
                    } else {
                        self.synchronize_item();
                        // Ensure progress to avoid infinite loop
                        if self.pos == pos_before && !self.is_at_end() {
                            self.advance();
                        }
                    }
                }

                let span = Span {
                    range: impl_start..self.previous().span.range.end,
                    file: None,
                };
                items.push(husk_ast::ExternItem {
                    attributes: item_attributes,
                    kind: husk_ast::ExternItemKind::Impl {
                        type_params,
                        self_ty,
                        items: impl_items,
                    },
                    span,
                });
                continue;
            }

            // Check for `fn` declaration
            let fn_start = self.current().span.range.start;
            if !self.matches_keyword(Keyword::Fn) {
                self.error_here("expected `fn`, `mod`, `struct`, `static`, `const`, or `impl` inside extern block");
                self.synchronize_item();
                continue;
            }
            let name = match self.parse_ident("expected function name in extern block") {
                Some(id) => id,
                None => {
                    self.synchronize_item();
                    continue;
                }
            };

            if !self.matches_token(&TokenKind::LParen) {
                self.error_here("expected `(` after extern function name");
                self.synchronize_item();
                continue;
            }
            let params = self.parse_param_list();

            let ret_type = if self.matches_token(&TokenKind::Arrow) {
                Some(self.parse_type_expr()?)
            } else {
                None
            };

            if !self.matches_token(&TokenKind::Semicolon) {
                self.error_here("expected `;` after extern function declaration");
                self.synchronize_item();
                continue;
            }

            let span = Span {
                range: fn_start..self.previous().span.range.end,
                file: None,
            };
            items.push(husk_ast::ExternItem {
                attributes: item_attributes,
                kind: husk_ast::ExternItemKind::Fn {
                    name,
                    params,
                    ret_type,
                },
                span,
            });
        }

        let end = self.previous().span.range.end;
        let span = Span {
            range: extern_tok.span.range.start..end,
            file: None,
        };

        Some(Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::ExternBlock { abi, items },
            span,
        })
    }

    /// Parse a trait definition: `trait Name: SuperTrait { fn method(&self); }`
    fn parse_trait_item(&mut self) -> Option<Item> {
        let trait_tok = self.advance().clone(); // consume `trait`
        let name = self.parse_ident("expected trait name after `trait`")?;

        // Parse optional type parameters with bounds
        let type_params = self.parse_type_params_with_bounds();

        // Parse optional supertraits: `trait Eq: PartialEq + Clone { ... }`
        let supertraits = if self.matches_token(&TokenKind::Colon) {
            self.parse_trait_bounds()
        } else {
            Vec::new()
        };

        if !self.matches_token(&TokenKind::LBrace) {
            self.error_here("expected `{` after trait name");
            return None;
        }

        let mut items = Vec::new();
        while !self.is_at_end() && !self.matches_token(&TokenKind::RBrace) {
            let pos_before = self.pos;
            if let Some(item) = self.parse_trait_method() {
                items.push(item);
            } else {
                self.synchronize_item();
                // Ensure progress is made to avoid infinite loop
                if self.pos == pos_before && !self.is_at_end() {
                    self.advance();
                }
            }
        }

        let end = self.previous().span.range.end;
        let span = Span {
            range: trait_tok.span.range.start..end,
            file: None,
        };

        Some(Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Trait(TraitDef {
                name,
                type_params,
                supertraits,
                items,
                span: span.clone(),
            }),
            span,
        })
    }

    /// Parse a method inside a trait: `fn method(&self) -> RetType;` or with default body
    fn parse_trait_method(&mut self) -> Option<TraitItem> {
        // First, parse any attributes
        let attributes = self.parse_attributes();

        // Capture start position for span (before extern or fn keyword)
        let item_start = self.current().span.range.start;

        // Check for extern "js" before fn (same as impl methods)
        let is_extern = if self.matches_keyword(Keyword::Extern) {
            // Expect "js" string literal
            if let TokenKind::StringLiteral(ref s) = self.current().kind {
                if s != "js" {
                    self.error_here("only \"js\" ABI is supported for extern methods");
                }
                self.advance();
            } else {
                self.error_here("expected string literal ABI after `extern`");
                return None;
            }
            true
        } else {
            false
        };

        if !self.matches_keyword(Keyword::Fn) {
            self.error_here("expected `fn` inside trait");
            return None;
        }
        let name = self.parse_ident("expected method name after `fn`")?;

        if !self.matches_token(&TokenKind::LParen) {
            self.error_here("expected `(` after method name");
            return None;
        }

        // Parse self receiver and regular parameters
        let (receiver, params) = self.parse_method_param_list();
        // Ensure we consumed the closing paren
        if matches!(self.current().kind, TokenKind::RParen) {
            self.advance(); // Consume it manually
        }

        // Optional return type
        let ret_type = if self.matches_token(&TokenKind::Arrow) {
            match self.parse_type_expr() {
                Some(ty) => Some(ty),
                None => {
                    return None;
                }
            }
        } else {
            None
        };

        // Either `;` (required method) or `{ ... }` (default implementation)
        let default_body = if self.matches_token(&TokenKind::Semicolon) {
            None
        } else if self.matches_token(&TokenKind::LBrace) {
            self.pos -= 1; // move back to parse_block
            let block = self.parse_block()?;
            Some(block.stmts)
        } else {
            self.error_here("expected `;` or `{` after method signature");
            return None;
        };

        let end = self.previous().span.range.end;
        Some(TraitItem {
            kind: TraitItemKind::Method(TraitMethod {
                attributes,
                name,
                receiver,
                params,
                ret_type,
                default_body,
                is_extern,
            }),
            span: Span {
                range: item_start..end,
                file: None,
            },
        })
    }

    /// Parse an impl block: `impl Trait for Type { ... }` or `impl Type { ... }`
    fn parse_impl_item(&mut self) -> Option<Item> {
        let impl_tok = self.advance().clone(); // consume `impl`

        // Parse optional type parameters with bounds
        let type_params = self.parse_type_params_with_bounds();

        // Parse the first type (could be trait or self_ty)
        let first_ty = self.parse_type_expr()?;

        // Check for `for` keyword to distinguish `impl Trait for Type` from `impl Type`
        let (trait_ref, self_ty) = if self.matches_keyword(Keyword::For) {
            let self_ty = self.parse_type_expr()?;
            (Some(first_ty), self_ty)
        } else {
            (None, first_ty)
        };

        if !self.matches_token(&TokenKind::LBrace) {
            self.error_here("expected `{` after impl header");
            return None;
        }

        let mut items = Vec::new();
        while !self.is_at_end() && !self.matches_token(&TokenKind::RBrace) {
            let pos_before = self.pos;
            if let Some(item) = self.parse_impl_method() {
                items.push(item);
            } else {
                self.synchronize_item();
                // Ensure progress is made to avoid infinite loop
                // (can happen when synchronize_item stops at a keyword like `type`
                // that parse_impl_method doesn't handle)
                if self.pos == pos_before && !self.is_at_end() {
                    self.advance();
                }
            }
        }

        let end = self.previous().span.range.end;
        let span = Span {
            range: impl_tok.span.range.start..end,
            file: None,
        };

        Some(Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Impl(ImplBlock {
                type_params,
                trait_ref,
                self_ty,
                items,
                span: span.clone(),
            }),
            span,
        })
    }

    /// Parse a single attribute like `#[getter]`, `#[js_name = "innerHTML"]`, or `#[cfg(test)]`.
    /// Returns None if no attribute is found (not an error).
    fn parse_attribute(&mut self) -> Option<Attribute> {
        if !self.matches_token(&TokenKind::Hash) {
            return None;
        }
        let start = self.previous().span.range.start;

        if !self.matches_token(&TokenKind::LBracket) {
            self.error_here("expected `[` after `#`");
            return None;
        }

        let name = self.parse_ident("expected attribute name")?;

        // Check for `(...)` syntax (cfg predicates or should_panic(expected = "..."))
        let (cfg_predicate, value) = if self.matches_token(&TokenKind::LParen) {
            if name.name == "cfg" {
                // Parse cfg predicate
                let pred = self.parse_cfg_predicate()?;
                if !self.matches_token(&TokenKind::RParen) {
                    self.error_here("expected `)` to close cfg predicate");
                    return None;
                }
                (Some(pred), None)
            } else if name.name == "should_panic" {
                // Parse optional expected = "message"
                let val = if matches!(&self.current().kind, TokenKind::Ident(s) if s == "expected")
                {
                    self.advance(); // consume `expected`
                    if !self.matches_token(&TokenKind::Eq) {
                        self.error_here("expected `=` after `expected`");
                        return None;
                    }
                    if let TokenKind::StringLiteral(ref s) = self.current().kind {
                        let val = s.clone();
                        self.advance();
                        Some(val)
                    } else {
                        self.error_here("expected string literal after `expected =`");
                        None
                    }
                } else {
                    None
                };
                if !self.matches_token(&TokenKind::RParen) {
                    self.error_here("expected `)` to close should_panic attribute");
                    return None;
                }
                (None, val)
            } else {
                // Unknown attribute with parentheses, skip contents
                let mut depth = 1;
                while depth > 0 && !self.is_at_end() {
                    if self.matches_token(&TokenKind::LParen) {
                        depth += 1;
                    } else if self.matches_token(&TokenKind::RParen) {
                        depth -= 1;
                    } else {
                        self.advance();
                    }
                }
                (None, None)
            }
        } else if self.matches_token(&TokenKind::Eq) {
            // Check for `= "value"` syntax
            if let TokenKind::StringLiteral(ref s) = self.current().kind {
                let val = s.clone();
                self.advance();
                (None, Some(val))
            } else {
                self.error_here("expected string literal after `=` in attribute");
                (None, None)
            }
        } else {
            (None, None)
        };

        if !self.matches_token(&TokenKind::RBracket) {
            self.error_here("expected `]` to close attribute");
            return None;
        }

        let end = self.previous().span.range.end;
        Some(Attribute {
            name,
            value,
            cfg_predicate,
            span: Span {
                range: start..end,
                file: None,
            },
        })
    }

    /// Parse a cfg predicate like `test`, `not(test)`, `all(test, debug)`, `any(node, bun)`.
    fn parse_cfg_predicate(&mut self) -> Option<CfgPredicate> {
        // Check for combinators: all(...), any(...), not(...)
        if let TokenKind::Ident(ref name) = self.current().kind.clone() {
            match name.as_str() {
                "all" => {
                    self.advance();
                    if !self.matches_token(&TokenKind::LParen) {
                        self.error_here("expected `(` after `all`");
                        return None;
                    }
                    let predicates = self.parse_cfg_predicate_list()?;
                    if !self.matches_token(&TokenKind::RParen) {
                        self.error_here("expected `)` to close `all(...)`");
                        return None;
                    }
                    return Some(CfgPredicate::All(predicates));
                }
                "any" => {
                    self.advance();
                    if !self.matches_token(&TokenKind::LParen) {
                        self.error_here("expected `(` after `any`");
                        return None;
                    }
                    let predicates = self.parse_cfg_predicate_list()?;
                    if !self.matches_token(&TokenKind::RParen) {
                        self.error_here("expected `)` to close `any(...)`");
                        return None;
                    }
                    return Some(CfgPredicate::Any(predicates));
                }
                "not" => {
                    self.advance();
                    if !self.matches_token(&TokenKind::LParen) {
                        self.error_here("expected `(` after `not`");
                        return None;
                    }
                    let inner = self.parse_cfg_predicate()?;
                    if !self.matches_token(&TokenKind::RParen) {
                        self.error_here("expected `)` to close `not(...)`");
                        return None;
                    }
                    return Some(CfgPredicate::Not(Box::new(inner)));
                }
                _ => {}
            }
        }

        // Simple flag or key-value
        let key = self.parse_ident("expected cfg predicate name")?;

        if self.matches_token(&TokenKind::Eq) {
            // Key-value: target = "esm"
            if let TokenKind::StringLiteral(ref s) = self.current().kind {
                let val = s.clone();
                self.advance();
                Some(CfgPredicate::KeyValue {
                    key: key.name,
                    value: val,
                })
            } else {
                self.error_here("expected string literal value in cfg predicate");
                None
            }
        } else {
            // Simple flag: test, debug
            Some(CfgPredicate::Flag(key.name))
        }
    }

    /// Parse a comma-separated list of cfg predicates.
    fn parse_cfg_predicate_list(&mut self) -> Option<Vec<CfgPredicate>> {
        let mut predicates = Vec::new();
        loop {
            // Check for empty or end
            if matches!(self.current().kind, TokenKind::RParen) {
                break;
            }
            predicates.push(self.parse_cfg_predicate()?);
            if !self.matches_token(&TokenKind::Comma) {
                break;
            }
        }
        Some(predicates)
    }

    /// Parse zero or more attributes: `#[getter] #[setter] #[js_name = "foo"]`
    fn parse_attributes(&mut self) -> Vec<Attribute> {
        let mut attrs = Vec::new();
        while let TokenKind::Hash = self.current().kind {
            if let Some(attr) = self.parse_attribute() {
                attrs.push(attr);
            } else {
                break;
            }
        }
        attrs
    }

    /// Parse a method or property inside an impl block.
    /// Methods: `fn method(&self) -> RetType { ... }` or `extern "js" fn method(&self) -> RetType;`
    /// Properties: `#[getter] extern "js" body: JsValue;`
    fn parse_impl_method(&mut self) -> Option<ImplItem> {
        // First, parse any attributes
        let attributes = self.parse_attributes();

        // Capture start position for span (before extern or fn keyword)
        let item_start = self.current().span.range.start;

        // Check for extern "js" (could be method or property)
        let is_extern = if self.matches_keyword(Keyword::Extern) {
            // Expect "js" string literal
            if let TokenKind::StringLiteral(ref s) = self.current().kind {
                if s != "js" {
                    self.error_here("only \"js\" ABI is supported for extern methods");
                }
                self.advance();
            } else {
                self.error_here("expected string literal ABI after `extern`");
            }
            true
        } else {
            false
        };

        // If extern and NOT followed by `fn`, this is a property declaration
        if is_extern && !matches!(self.current().kind, TokenKind::Keyword(Keyword::Fn)) {
            // Parse property: `name: Type;`
            return self.parse_extern_property(attributes);
        }

        if !self.matches_keyword(Keyword::Fn) {
            self.error_here("expected `fn` inside impl");
            return None;
        }
        let name = self.parse_ident("expected method name after `fn`")?;

        if !self.matches_token(&TokenKind::LParen) {
            self.error_here("expected `(` after method name");
            return None;
        }

        // Parse self receiver and regular parameters
        let (receiver, params) = self.parse_method_param_list();

        // Optional return type
        let ret_type = if self.matches_token(&TokenKind::Arrow) {
            Some(self.parse_type_expr()?)
        } else {
            None
        };

        if is_extern {
            // Extern methods end with semicolon, no body
            if !self.matches_token(&TokenKind::Semicolon) {
                self.error_here("expected `;` after extern method declaration");
                return None;
            }
            let end = self.previous().span.range.end;
            Some(ImplItem {
                kind: ImplItemKind::Method(ImplMethod {
                    attributes,
                    name,
                    receiver,
                    params,
                    ret_type,
                    body: Vec::new(), // Extern methods have no body
                    is_extern: true,
                }),
                span: Span {
                    range: item_start..end,
                    file: None,
                },
            })
        } else {
            // Regular method - body is required
            let body_block = self.parse_block()?;
            let end = body_block.span.range.end;
            Some(ImplItem {
                kind: ImplItemKind::Method(ImplMethod {
                    attributes,
                    name,
                    receiver,
                    params,
                    ret_type,
                    body: body_block.stmts,
                    is_extern: false,
                }),
                span: Span {
                    range: item_start..end,
                    file: None,
                },
            })
        }
    }

    /// Parse an extern property declaration: `name: Type;`
    /// Called after attributes and `extern "js"` have been parsed.
    fn parse_extern_property(&mut self, attributes: Vec<Attribute>) -> Option<ImplItem> {
        // Get start position from first attribute if present, otherwise use current position
        let start = attributes
            .first()
            .map(|a| a.span.range.start)
            .unwrap_or_else(|| self.current().span.range.start);

        // Parse property name
        let name = self.parse_ident("expected property name")?;

        // Expect `:`
        if !self.matches_token(&TokenKind::Colon) {
            self.error_here("expected `:` after property name");
            return None;
        }

        // Parse type
        let ty = self.parse_type_expr()?;

        // Expect `;`
        if !self.matches_token(&TokenKind::Semicolon) {
            self.error_here("expected `;` after extern property declaration");
            return None;
        }

        let end = self.previous().span.range.end;
        Some(ImplItem {
            kind: ImplItemKind::Property(ExternProperty {
                attributes,
                name,
                ty,
                span: Span {
                    range: start..end,
                    file: None,
                },
            }),
            span: Span {
                range: start..end,
                file: None,
            },
        })
    }

    /// Parse a method inside an extern impl block.
    /// All methods are implicitly extern and must end with `;` (no body).
    /// Format: `fn method(&self, param: Type) -> RetType;`
    fn parse_extern_impl_method(&mut self) -> Option<ImplItem> {
        // Parse any attributes
        let attributes = self.parse_attributes();

        let item_start = self.current().span.range.start;

        if !self.matches_keyword(Keyword::Fn) {
            self.error_here("expected `fn` inside extern impl block");
            return None;
        }

        let name = self.parse_ident("expected method name after `fn`")?;

        if !self.matches_token(&TokenKind::LParen) {
            self.error_here("expected `(` after method name");
            return None;
        }

        // Parse self receiver and regular parameters
        let (receiver, params) = self.parse_method_param_list();

        // Optional return type
        let ret_type = if self.matches_token(&TokenKind::Arrow) {
            Some(self.parse_type_expr()?)
        } else {
            None
        };

        // All methods in extern impl blocks must end with semicolon
        if !self.matches_token(&TokenKind::Semicolon) {
            self.error_here("expected `;` after method declaration in extern impl block");
            return None;
        }

        let end = self.previous().span.range.end;
        Some(ImplItem {
            kind: ImplItemKind::Method(ImplMethod {
                attributes,
                name,
                receiver,
                params,
                ret_type,
                body: Vec::new(), // Extern methods have no body
                is_extern: true,
            }),
            span: Span {
                range: item_start..end,
                file: None,
            },
        })
    }

    /// Parse method parameter list, handling `self`, `&self`, `&mut self` as first param
    fn parse_method_param_list(&mut self) -> (Option<SelfReceiver>, Vec<Param>) {
        let mut receiver = None;
        let mut params = Vec::new();

        if self.matches_token(&TokenKind::RParen) {
            return (receiver, params);
        }

        // Check for self receiver
        if self.matches_token(&TokenKind::Amp) {
            // `&self` or `&mut self`
            if self.matches_keyword(Keyword::Mut) {
                // `&mut self`
                if !self.check_ident("self") {
                    self.error_here("expected `self` after `&mut`");
                } else {
                    self.advance(); // consume `self`
                    receiver = Some(SelfReceiver::RefMut);
                }
            } else if self.check_ident("self") {
                // `&self`
                self.advance(); // consume `self`
                receiver = Some(SelfReceiver::Ref);
            } else {
                self.error_here("expected `self` or `mut self` after `&`");
            }
        } else if self.check_ident("self") {
            // `self` by value
            self.advance(); // consume `self`
            receiver = Some(SelfReceiver::Value);
        }

        // If we got a receiver, check for comma or end
        if receiver.is_some() {
            if self.matches_token(&TokenKind::RParen) {
                return (receiver, params);
            }
            if !self.matches_token(&TokenKind::Comma) {
                self.error_here("expected `,` or `)` after self parameter");
                return (receiver, params);
            }
        }

        // Parse remaining parameters
        if self.matches_token(&TokenKind::RParen) {
            return (receiver, params);
        }

        loop {
            // Parse optional parameter attributes like #[this]
            let attributes = self.parse_attributes();

            let name = match self.parse_ident("expected parameter name") {
                Some(n) => n,
                None => break,
            };

            if !self.matches_token(&TokenKind::Colon) {
                self.error_here("expected `:` after parameter name");
                break;
            }
            let ty = match self.parse_type_expr() {
                Some(t) => t,
                None => {
                    break;
                }
            };
            params.push(Param {
                attributes,
                name,
                ty,
            });

            if self.matches_token(&TokenKind::RParen) {
                break;
            }
            if !self.matches_token(&TokenKind::Comma) {
                self.error_here("expected `,` or `)` in parameter list");
                break;
            }
        }

        (receiver, params)
    }

    /// Check if current token is a specific identifier (without consuming)
    fn check_ident(&self, name: &str) -> bool {
        matches!(&self.current().kind, TokenKind::Ident(s) if s == name)
    }

    /// Parse type parameters with optional bounds: `<T, U: Foo + Bar>`
    fn parse_type_params_with_bounds(&mut self) -> Vec<TypeParam> {
        let mut params = Vec::new();
        if !self.matches_token(&TokenKind::Lt) {
            return params;
        }

        while let Some(name) = self.parse_ident("expected type parameter name") {
            // Parse optional bounds: `: Foo + Bar`
            let bounds = if self.matches_token(&TokenKind::Colon) {
                self.parse_trait_bounds()
            } else {
                Vec::new()
            };

            params.push(TypeParam { name, bounds });

            if self.matches_token(&TokenKind::Gt) {
                break;
            }
            if !self.matches_token(&TokenKind::Comma) {
                self.error_here("expected `,` or `>` in type parameter list");
                break;
            }
        }

        params
    }

    /// Parse trait bounds: `Foo + Bar + Baz`
    fn parse_trait_bounds(&mut self) -> Vec<TypeExpr> {
        let mut bounds = Vec::new();

        while let Some(ty) = self.parse_type_expr() {
            bounds.push(ty);
            if !self.matches_token(&TokenKind::Plus) {
                break;
            }
        }

        bounds
    }

    fn parse_type_params(&mut self) -> Vec<Ident> {
        let mut params = Vec::new();
        if !self.matches_token(&TokenKind::Lt) {
            return params;
        }
        while let Some(id) = self.parse_ident("expected type parameter name") {
            params.push(id);
            if self.matches_token(&TokenKind::Gt) {
                break;
            }
            if !self.matches_token(&TokenKind::Comma) {
                self.error_here("expected `,` or `>` in type parameter list");
                break;
            }
        }
        params
    }

    fn parse_param_list(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        if self.matches_token(&TokenKind::RParen) {
            return params;
        }

        loop {
            // Parse optional parameter attributes like #[this]
            let attributes = self.parse_attributes();

            let name = match self.parse_ident("expected parameter name") {
                Some(n) => n,
                None => break,
            };

            if !self.matches_token(&TokenKind::Colon) {
                self.error_here("expected `:` after parameter name");
                break;
            }
            let ty = match self.parse_type_expr() {
                Some(t) => t,
                None => break,
            };
            params.push(Param {
                attributes,
                name,
                ty,
            });

            if self.matches_token(&TokenKind::RParen) {
                break;
            }
            if !self.matches_token(&TokenKind::Comma) {
                self.error_here("expected `,` or `)` in parameter list");
                break;
            }
        }
        params
    }

    fn parse_type_expr(&mut self) -> Option<TypeExpr> {
        let tok = self.current().clone();
        match tok.kind {
            // Impl Trait type: `impl Iterator<T>`
            TokenKind::Keyword(Keyword::Impl) => {
                let start = tok.span.range.start;
                self.advance(); // consume `impl`
                let new_token = self.current();

                // Safety check: if we're still seeing impl, we have a problem
                if matches!(new_token.kind, TokenKind::Keyword(Keyword::Impl)) {
                    return None;
                }

                let trait_ty = match self.parse_type_expr() {
                    Some(ty) => ty,
                    None => {
                        return None;
                    }
                };
                let end = trait_ty.span.range.end;

                Some(TypeExpr {
                    kind: TypeExprKind::ImplTrait {
                        trait_ty: Box::new(trait_ty),
                    },
                    span: Span {
                        range: start..end,
                        file: None,
                    },
                })
            }
            // Function type: `fn(T, U) -> V`
            TokenKind::Keyword(Keyword::Fn) => {
                let start = tok.span.range.start;
                self.advance(); // consume `fn`

                if !self.matches_token(&TokenKind::LParen) {
                    self.error_here("expected `(` after `fn` in function type");
                    return None;
                }

                // Parse parameter types
                let mut params = Vec::new();
                if !self.matches_token(&TokenKind::RParen) {
                    loop {
                        // Skip `&` if present (reference types not fully supported, treat &T as T)
                        let _ = self.matches_token(&TokenKind::Amp);
                        match self.parse_type_expr() {
                            Some(param_ty) => {
                                params.push(param_ty);
                            }
                            None => {
                                break;
                            }
                        }
                        if self.matches_token(&TokenKind::RParen) {
                            break;
                        }
                        if !self.matches_token(&TokenKind::Comma) {
                            self.error_here("expected `,` or `)` in function type parameters");
                            break;
                        }
                    }
                }

                // Return type is optional: `-> Type` or implicit `()`
                let ret = if self.matches_token(&TokenKind::Arrow) {
                    match self.parse_type_expr() {
                        Some(ty) => ty,
                        None => {
                            // Default to unit type if return type parsing fails
                            TypeExpr {
                                kind: TypeExprKind::Named(Ident {
                                    name: "()".to_string(),
                                    span: Span {
                                        range: self.current().span.range.start
                                            ..self.current().span.range.start,
                                        file: None,
                                    },
                                }),
                                span: Span {
                                    range: self.current().span.range.start
                                        ..self.current().span.range.start,
                                    file: None,
                                },
                            }
                        }
                    }
                } else {
                    // No return type specified, default to unit type `()`
                    TypeExpr {
                        kind: TypeExprKind::Named(Ident {
                            name: "()".to_string(),
                            span: Span {
                                range: self.current().span.range.start
                                    ..self.current().span.range.start,
                                file: None,
                            },
                        }),
                        span: Span {
                            range: self.current().span.range.start..self.current().span.range.start,
                            file: None,
                        },
                    }
                };
                let end = ret.span.range.end;

                Some(TypeExpr {
                    kind: TypeExprKind::Function {
                        params,
                        ret: Box::new(ret),
                    },
                    span: Span {
                        range: start..end,
                        file: None,
                    },
                })
            }
            // Unit type `()` or tuple type `(T1, T2, ...)`
            TokenKind::LParen => {
                let start = tok.span.range.start;
                self.advance(); // consume `(`

                // Check for unit type `()`
                if self.matches_token(&TokenKind::RParen) {
                    let end = self.previous().span.range.end;
                    // Unit type is represented as a named type "()"
                    return Some(TypeExpr {
                        kind: TypeExprKind::Named(Ident {
                            name: "()".to_string(),
                            span: Span {
                                range: start..end,
                                file: None,
                            },
                        }),
                        span: Span {
                            range: start..end,
                            file: None,
                        },
                    });
                }

                // Parse tuple type: (T1, T2, ...) with optional trailing comma.
                let mut types = Vec::new();
                let mut had_trailing_comma = false;
                loop {
                    let ty = self.parse_type_expr()?;
                    types.push(ty);

                    if self.matches_token(&TokenKind::RParen) {
                        break;
                    }
                    if !self.matches_token(&TokenKind::Comma) {
                        self.error_here("expected `,` or `)` in tuple type");
                        return None;
                    }
                    // If we see `)` immediately after a comma, remember that there was a trailing comma.
                    had_trailing_comma = true;
                    if self.matches_token(&TokenKind::RParen) {
                        break;
                    }
                    // Reset if we continue parsing more elements
                    had_trailing_comma = false;
                }

                let end = self.previous().span.range.end;

                // Single element in parens is just grouping, not a tuple,
                // unless we saw a trailing comma: (T,) is a 1-tuple.
                if types.len() == 1 && !had_trailing_comma {
                    // Just return the inner type (parenthesized type)
                    return Some(types.pop().unwrap());
                }

                Some(TypeExpr {
                    kind: TypeExprKind::Tuple(types),
                    span: Span {
                        range: start..end,
                        file: None,
                    },
                })
            }
            // Array type: `[ElementType]`
            TokenKind::LBracket => {
                let start = tok.span.range.start;
                self.advance(); // consume '['

                let elem_type = self.parse_type_expr()?;

                if !self.matches_token(&TokenKind::RBracket) {
                    self.error_here("expected `]` after array element type");
                    return None;
                }

                let end = self.previous().span.range.end;

                Some(TypeExpr {
                    kind: TypeExprKind::Array(Box::new(elem_type)),
                    span: Span {
                        range: start..end,
                        file: None,
                    },
                })
            }
            // Self type (capital S) - used in trait definitions
            TokenKind::Keyword(Keyword::SelfType) => {
                self.advance();
                let span = self.ast_span_from(&tok.span);
                Some(TypeExpr {
                    kind: TypeExprKind::Named(Ident {
                        name: "Self".to_string(),
                        span: span.clone(),
                    }),
                    span,
                })
            }
            TokenKind::Ident(ref name) => {
                self.advance();
                let ident = Ident {
                    name: name.clone(),
                    span: self.ast_span_from(&tok.span),
                };
                // Generic application: Name<...>
                if self.matches_token(&TokenKind::Lt) {
                    let mut args = Vec::new();
                    let mut loop_count = 0;
                    loop {
                        loop_count += 1;
                        if loop_count > 50 {
                            self.error_here("too many generic type arguments (limit: 50)");
                            break;
                        }
                        match self.parse_type_expr() {
                            Some(arg) => {
                                args.push(arg);
                            }
                            None => {
                                // Advance to prevent infinite loop
                                if !self.is_at_end() {
                                    self.advance();
                                }
                                break;
                            }
                        }
                        if self.matches_token(&TokenKind::Gt) {
                            break;
                        }
                        if !self.matches_token(&TokenKind::Comma) {
                            self.error_here("expected `,` or `>` in generic type arguments");
                            break;
                        }
                    }
                    let span = Span {
                        range: ident.span.range.start..self.previous().span.range.end,
                        file: None,
                    };
                    Some(TypeExpr {
                        kind: TypeExprKind::Generic { name: ident, args },
                        span,
                    })
                } else {
                    let span = ident.span.clone();
                    Some(TypeExpr {
                        kind: TypeExprKind::Named(ident),
                        span,
                    })
                }
            }
            _ => {
                self.error_at_token(&tok, "expected type");
                None
            }
        }
    }

    fn parse_ident(&mut self, msg: &str) -> Option<Ident> {
        let tok = self.current().clone();
        if let TokenKind::Ident(ref name) = tok.kind {
            self.advance();
            Some(Ident {
                name: name.clone(),
                span: self.ast_span_from(&tok.span),
            })
        } else {
            self.error_here(msg);
            None
        }
    }

    /// After consuming an initial identifier, parse any following `::segment` path components.
    fn parse_path_segments(&mut self, first: Ident) -> Vec<Ident> {
        let mut path = vec![first];
        // Check for path segments `Foo::Bar`.
        while self.matches_token(&TokenKind::ColonColon) {
            let seg = match self.parse_ident("expected identifier after `::` in path") {
                Some(id) => id,
                None => break,
            };
            path.push(seg);
        }
        path
    }

    // ---------------- Statements and blocks ----------------

    fn parse_block(&mut self) -> Option<Block> {
        let start_tok = self.current().clone();
        if !self.matches_token(&TokenKind::LBrace) {
            self.error_here("expected `{` to start block");
            return None;
        }
        let start = start_tok.span.range.start;

        let mut stmts = Vec::new();
        while !self.is_at_end() && !self.matches_token(&TokenKind::RBrace) {
            if let Some(stmt) = self.parse_stmt() {
                stmts.push(stmt);
            } else {
                // Attempt to recover by skipping to next semicolon or closing brace
                while !self.is_at_end() {
                    if matches!(
                        self.current().kind,
                        TokenKind::Semicolon | TokenKind::RBrace
                    ) {
                        break;
                    }
                    self.advance();
                }
                let _ = self.matches_token(&TokenKind::Semicolon);
            }
        }

        let end = self.previous().span.range.end;
        Some(Block {
            stmts,
            span: Span {
                range: start..end,
                file: None,
            },
        })
    }

    fn parse_use_item(&mut self) -> Option<Item> {
        let use_tok = self.advance().clone(); // consume `use`
        let first = self.parse_ident("expected path after `use`")?;
        let path = self.parse_use_path_segments(first);

        // Check for variant import syntax: ::*, ::{A, B}, or continue as normal path
        let kind = if self.matches_token(&TokenKind::ColonColon) {
            if self.matches_token(&TokenKind::Star) {
                // Glob import: `use Enum::*;`
                husk_ast::UseKind::Glob
            } else if self.matches_token(&TokenKind::LBrace) {
                // Selective import: `use Enum::{A, B};`
                let variants = self.parse_use_variant_list()?;
                if !self.matches_token(&TokenKind::RBrace) {
                    self.error_here("expected `}` after variant list");
                }
                husk_ast::UseKind::Variants(variants)
            } else {
                // Could be single variant or continuing path - parse as identifier
                if let Some(variant) =
                    self.parse_ident("expected identifier, `*`, or `{` after `::`")
                {
                    husk_ast::UseKind::Variants(vec![variant])
                } else {
                    husk_ast::UseKind::Item
                }
            }
        } else {
            // Regular item import: `use crate::foo::Bar;`
            husk_ast::UseKind::Item
        };

        if !self.matches_token(&TokenKind::Semicolon) {
            self.error_here("expected `;` after use path");
        }
        let end = self.previous().span.range.end;
        Some(Item {
            attributes: Vec::new(),
            visibility: husk_ast::Visibility::Private,
            kind: ItemKind::Use { path, kind },
            span: Span {
                range: use_tok.span.range.start..end,
                file: None,
            },
        })
    }

    /// Parse path segments for use statements, stopping before `::*`, `::{`,
    /// and (for bare type paths) before a single-variant import like `Result::Ok;`.
    fn parse_use_path_segments(&mut self, first: Ident) -> Vec<Ident> {
        let mut path = vec![first];
        // Check for path segments `Foo::Bar`, but stop before:
        // - `::*`
        // - `::{`
        // - `Type::Variant;` when the path is a single bare type (e.g. `Result::Ok;`)
        while self.current().kind == TokenKind::ColonColon {
            // Peek ahead to see what follows `::`
            if let Some(next) = self.tokens.get(self.pos + 1) {
                match next.kind {
                    TokenKind::Star | TokenKind::LBrace => {
                        // Stop here - let the caller handle ::* or ::{
                        break;
                    }
                    TokenKind::Ident(_) => {
                        // For a bare type path like `Result::Ok;`, stop before the final
                        // `::Ok` so that `parse_use_item` can treat it as a variant import.
                        let is_bare_single_variant = path.len() == 1
                            && path[0].name != "crate"
                            && path[0].name != "self"
                            && path[0].name != "super"
                            && matches!(
                                self.tokens.get(self.pos + 2).map(|t| &t.kind),
                                Some(TokenKind::Semicolon)
                            );
                        if is_bare_single_variant {
                            break;
                        }

                        // Otherwise, continue parsing as a normal path segment.
                        self.advance(); // consume `::`
                        let seg = match self.parse_ident("expected identifier after `::` in path") {
                            Some(id) => id,
                            None => break,
                        };
                        path.push(seg);
                    }
                    _ => break,
                }
            } else {
                break;
            }
        }
        path
    }

    /// Parse a comma-separated list of variant identifiers: `A, B, C`
    fn parse_use_variant_list(&mut self) -> Option<Vec<Ident>> {
        let mut variants = Vec::new();

        // Parse first variant
        if let Some(ident) = self.parse_ident("expected variant name") {
            variants.push(ident);
        } else {
            return Some(variants); // Empty list
        }

        // Parse remaining variants separated by commas
        while self.matches_token(&TokenKind::Comma) {
            // Allow trailing comma
            if self.current().kind == TokenKind::RBrace {
                break;
            }
            if let Some(ident) = self.parse_ident("expected variant name after `,`") {
                variants.push(ident);
            } else {
                break;
            }
        }

        Some(variants)
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        match self.current().kind {
            TokenKind::Keyword(Keyword::Let) => self.parse_let_stmt(),
            TokenKind::Keyword(Keyword::Return) => self.parse_return_stmt(),
            TokenKind::Keyword(Keyword::If) => self.parse_if_stmt(),
            TokenKind::Keyword(Keyword::While) => self.parse_while_stmt(),
            TokenKind::Keyword(Keyword::Loop) => self.parse_loop_stmt(),
            TokenKind::Keyword(Keyword::For) => self.parse_for_in_stmt(),
            TokenKind::Keyword(Keyword::Break) => self.parse_break_stmt(),
            TokenKind::Keyword(Keyword::Continue) => self.parse_continue_stmt(),
            TokenKind::LBrace => {
                let block = self.parse_block()?;
                let span = block.span.clone();
                Some(Stmt {
                    kind: StmtKind::Block(block),
                    span,
                })
            }
            _ => self.parse_expr_stmt(),
        }
    }

    fn parse_let_stmt(&mut self) -> Option<Stmt> {
        let let_tok = self.advance().clone(); // consume `let`
        let mut mutable = false;
        if self.matches_keyword(Keyword::Mut) {
            mutable = true;
        }
        let pattern = self.parse_pattern()?;

        let ty = if self.matches_token(&TokenKind::Colon) {
            Some(self.parse_type_expr()?)
        } else {
            None
        };

        let value = if self.matches_token(&TokenKind::Eq) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        // Parse optional else block: `else { ... }`
        let else_block = if self.matches_keyword(Keyword::Else) {
            // Require initializer for let...else
            if value.is_none() {
                self.error_here("`let ... else` requires an initializer (`= expr`)");
                return None;
            }
            Some(self.parse_block()?)
        } else {
            None
        };

        if !self.matches_token(&TokenKind::Semicolon) {
            if else_block.is_some() {
                self.error_here("expected `;` after `let ... else` block");
            } else {
                self.error_here("expected `;` after let binding");
            }
            return None;
        }

        let end = self.previous().span.range.end;
        Some(Stmt {
            kind: StmtKind::Let {
                mutable,
                pattern,
                ty,
                value,
                else_block,
            },
            span: Span {
                range: let_tok.span.range.start..end,
                file: None,
            },
        })
    }

    fn parse_return_stmt(&mut self) -> Option<Stmt> {
        let ret_tok = self.advance().clone(); // consume `return`
        let value = if matches!(self.current().kind, TokenKind::Semicolon) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        if !self.matches_token(&TokenKind::Semicolon) {
            self.error_here("expected `;` after return");
        }
        let end = self.previous().span.range.end;
        Some(Stmt {
            kind: StmtKind::Return { value },
            span: Span {
                range: ret_tok.span.range.start..end,
                file: None,
            },
        })
    }

    fn parse_if_stmt(&mut self) -> Option<Stmt> {
        let if_tok = self.advance().clone(); // consume `if`

        // Check for `if let` syntax
        if self.matches_keyword(Keyword::Let) {
            return self.parse_if_let_stmt(if_tok);
        }

        // Use parse_expr_no_struct so that `if x { ... }` doesn't try to
        // parse `x { ... }` as a struct literal.
        let cond = self.parse_expr_no_struct()?;
        let then_branch = self.parse_block().unwrap_or(Block {
            stmts: Vec::new(),
            span: self.ast_span_from(&if_tok.span),
        });

        let else_branch = if self.matches_keyword(Keyword::Else) {
            if matches!(self.current().kind, TokenKind::Keyword(Keyword::If)) {
                // else if ...
                Some(Box::new(self.parse_if_stmt()?))
            } else {
                // else { ... }
                let block = self.parse_block().unwrap_or(Block {
                    stmts: Vec::new(),
                    span: self.ast_span_from(&if_tok.span),
                });
                Some(Box::new(Stmt {
                    kind: StmtKind::Block(block.clone()),
                    span: block.span.clone(),
                }))
            }
        } else {
            None
        };

        let end = self.previous().span.range.end;
        Some(Stmt {
            kind: StmtKind::If {
                cond,
                then_branch,
                else_branch,
            },
            span: Span {
                range: if_tok.span.range.start..end,
                file: None,
            },
        })
    }

    fn parse_if_let_stmt(&mut self, if_tok: Token) -> Option<Stmt> {
        let pattern = self.parse_pattern()?;

        if !self.matches_token(&TokenKind::Eq) {
            self.error_here("expected `=` after pattern in `if let`");
            return None;
        }

        // Use parse_expr_no_struct to avoid brace ambiguity
        let scrutinee = self.parse_expr_no_struct()?;
        let then_branch = self.parse_block()?;

        let else_branch = if self.matches_keyword(Keyword::Else) {
            if matches!(self.current().kind, TokenKind::Keyword(Keyword::If)) {
                // else if / else if let
                Some(Box::new(self.parse_if_stmt()?))
            } else {
                // else { ... }
                let block = self.parse_block()?;
                Some(Box::new(Stmt {
                    kind: StmtKind::Block(block.clone()),
                    span: block.span.clone(),
                }))
            }
        } else {
            None
        };

        let end = self.previous().span.range.end;
        Some(Stmt {
            kind: StmtKind::IfLet {
                pattern,
                scrutinee,
                then_branch,
                else_branch,
            },
            span: Span {
                range: if_tok.span.range.start..end,
                file: None,
            },
        })
    }

    fn parse_while_stmt(&mut self) -> Option<Stmt> {
        let while_tok = self.advance().clone(); // consume `while`
        // Use parse_expr_no_struct so that `while x { ... }` doesn't try to
        // parse `x { ... }` as a struct literal.
        let cond = self.parse_expr_no_struct()?;
        let body = self.parse_block().unwrap_or(Block {
            stmts: Vec::new(),
            span: self.ast_span_from(&while_tok.span),
        });

        let end = body.span.range.end;
        Some(Stmt {
            kind: StmtKind::While { cond, body },
            span: Span {
                range: while_tok.span.range.start..end,
                file: None,
            },
        })
    }

    fn parse_loop_stmt(&mut self) -> Option<Stmt> {
        let loop_tok = self.advance().clone(); // consume `loop`
        let body = self.parse_block().unwrap_or(Block {
            stmts: Vec::new(),
            span: self.ast_span_from(&loop_tok.span),
        });

        let end = body.span.range.end;
        Some(Stmt {
            kind: StmtKind::Loop { body },
            span: Span {
                range: loop_tok.span.range.start..end,
                file: None,
            },
        })
    }

    fn parse_for_in_stmt(&mut self) -> Option<Stmt> {
        let for_tok = self.advance().clone(); // consume `for`
        let binding = self.parse_ident("expected binding name after `for`")?;

        if !self.matches_keyword(Keyword::In) {
            self.error_here("expected `in` after for loop binding");
            return None;
        }

        // Use parse_expr_no_struct so that `for x in arr { ... }` doesn't try to
        // parse `arr { ... }` as a struct literal.
        let iterable = self.parse_expr_no_struct()?;
        let body = self.parse_block().unwrap_or(Block {
            stmts: Vec::new(),
            span: self.ast_span_from(&for_tok.span),
        });

        let end = body.span.range.end;
        Some(Stmt {
            kind: StmtKind::ForIn {
                binding,
                iterable,
                body,
            },
            span: Span {
                range: for_tok.span.range.start..end,
                file: None,
            },
        })
    }

    fn parse_break_stmt(&mut self) -> Option<Stmt> {
        let tok = self.advance().clone(); // consume `break`
        if !self.matches_token(&TokenKind::Semicolon) {
            self.error_here("expected `;` after `break`");
        }
        let end = self.previous().span.range.end;
        Some(Stmt {
            kind: StmtKind::Break,
            span: Span {
                range: tok.span.range.start..end,
                file: None,
            },
        })
    }

    fn parse_continue_stmt(&mut self) -> Option<Stmt> {
        let tok = self.advance().clone(); // consume `continue`
        if !self.matches_token(&TokenKind::Semicolon) {
            self.error_here("expected `;` after `continue`");
        }
        let end = self.previous().span.range.end;
        Some(Stmt {
            kind: StmtKind::Continue,
            span: Span {
                range: tok.span.range.start..end,
                file: None,
            },
        })
    }

    fn parse_expr_stmt(&mut self) -> Option<Stmt> {
        // parse_expr() already delegates to parse_assignment_expr() which handles
        // assignments, so assignments are already parsed as ExprKind::Assign by this point.
        // This function just wraps the expression in a statement.
        let expr = self.parse_expr()?;

        // Regular expression statement
        let mut span = expr.span.clone();
        let kind = if self.matches_token(&TokenKind::Semicolon) {
            span.range.end = self.previous().span.range.end;
            StmtKind::Semi(expr)
        } else {
            StmtKind::Expr(expr)
        };
        Some(Stmt { kind, span })
    }

    // ---------------- Expressions ----------------

    fn parse_expr(&mut self) -> Option<Expr> {
        self.parse_assignment_expr()
    }

    /// Parse assignment expressions (lowest precedence, right-associative).
    /// `target = value` or `target += value` etc.
    fn parse_assignment_expr(&mut self) -> Option<Expr> {
        let expr = self.parse_logical_or()?;

        // Check for assignment operators
        let assign_op = match self.current().kind {
            TokenKind::Eq => Some(AssignOp::Assign),
            TokenKind::PlusEq => Some(AssignOp::AddAssign),
            TokenKind::MinusEq => Some(AssignOp::SubAssign),
            TokenKind::PercentEq => Some(AssignOp::ModAssign),
            _ => None,
        };

        if let Some(op) = assign_op {
            self.advance(); // consume assignment operator
            // Right-associative: parse the right side as another assignment
            let value = self.parse_assignment_expr()?;
            let span = Span {
                range: expr.span.range.start..value.span.range.end,
                file: None,
            };
            Some(Expr {
                kind: ExprKind::Assign {
                    target: Box::new(expr),
                    op,
                    value: Box::new(value),
                },
                span,
            })
        } else {
            Some(expr)
        }
    }

    fn parse_logical_or(&mut self) -> Option<Expr> {
        let mut expr = self.parse_logical_and()?;
        loop {
            if self.matches_token(&TokenKind::OrOr) {
                let op_span = self.previous().span.clone();
                let right = self.parse_logical_and()?;
                let span = Span {
                    range: expr.span.range.start..right.span.range.end,
                    file: None,
                };
                expr = Expr {
                    kind: ExprKind::Binary {
                        op: BinaryOp::Or,
                        left: Box::new(expr),
                        right: Box::new(right),
                    },
                    span,
                };
                let _ = op_span; // suppress unused warning
            } else {
                break;
            }
        }
        Some(expr)
    }

    fn parse_logical_and(&mut self) -> Option<Expr> {
        let mut expr = self.parse_equality()?;
        loop {
            if self.matches_token(&TokenKind::AndAnd) {
                let right = self.parse_equality()?;
                let span = Span {
                    range: expr.span.range.start..right.span.range.end,
                    file: None,
                };
                expr = Expr {
                    kind: ExprKind::Binary {
                        op: BinaryOp::And,
                        left: Box::new(expr),
                        right: Box::new(right),
                    },
                    span,
                };
            } else {
                break;
            }
        }
        Some(expr)
    }

    fn parse_equality(&mut self) -> Option<Expr> {
        let mut expr = self.parse_range()?;
        loop {
            if self.matches_token(&TokenKind::EqEq) {
                let right = self.parse_range()?;
                let span = Span {
                    range: expr.span.range.start..right.span.range.end,
                    file: None,
                };
                expr = Expr {
                    kind: ExprKind::Binary {
                        op: BinaryOp::Eq,
                        left: Box::new(expr),
                        right: Box::new(right),
                    },
                    span,
                };
            } else if self.matches_token(&TokenKind::BangEq) {
                let right = self.parse_range()?;
                let span = Span {
                    range: expr.span.range.start..right.span.range.end,
                    file: None,
                };
                expr = Expr {
                    kind: ExprKind::Binary {
                        op: BinaryOp::NotEq,
                        left: Box::new(expr),
                        right: Box::new(right),
                    },
                    span,
                };
            } else {
                break;
            }
        }
        Some(expr)
    }

    fn parse_range(&mut self) -> Option<Expr> {
        let expr = self.parse_comparison()?;

        if self.matches_token(&TokenKind::DotDot) {
            let end = self.parse_comparison()?;
            let span = Span {
                range: expr.span.range.start..end.span.range.end,
                file: None,
            };
            return Some(Expr {
                kind: ExprKind::Range {
                    start: Some(Box::new(expr)),
                    end: Some(Box::new(end)),
                    inclusive: false,
                },
                span,
            });
        } else if self.matches_token(&TokenKind::DotDotEq) {
            let end = self.parse_comparison()?;
            let span = Span {
                range: expr.span.range.start..end.span.range.end,
                file: None,
            };
            return Some(Expr {
                kind: ExprKind::Range {
                    start: Some(Box::new(expr)),
                    end: Some(Box::new(end)),
                    inclusive: true,
                },
                span,
            });
        }

        Some(expr)
    }

    fn parse_comparison(&mut self) -> Option<Expr> {
        let mut expr = self.parse_additive()?;
        loop {
            let op = if self.matches_token(&TokenKind::Lt) {
                Some(BinaryOp::Lt)
            } else if self.matches_token(&TokenKind::Le) {
                Some(BinaryOp::Le)
            } else if self.matches_token(&TokenKind::Gt) {
                Some(BinaryOp::Gt)
            } else if self.matches_token(&TokenKind::Ge) {
                Some(BinaryOp::Ge)
            } else {
                None
            };

            if let Some(op) = op {
                let right = self.parse_additive()?;
                let span = Span {
                    range: expr.span.range.start..right.span.range.end,
                    file: None,
                };
                expr = Expr {
                    kind: ExprKind::Binary {
                        op,
                        left: Box::new(expr),
                        right: Box::new(right),
                    },
                    span,
                };
            } else {
                break;
            }
        }
        Some(expr)
    }

    /// Parse cast expressions: `expr as Type`
    /// Cast has higher precedence than all arithmetic operators (+, -, *, /, %)
    /// so `2 + 3 as f64` parses as `2 + (3 as f64)` and `2 * 3 as f64` as `2 * (3 as f64)`.
    fn parse_cast(&mut self) -> Option<Expr> {
        let mut expr = self.parse_unary()?;

        loop {
            if self.matches_keyword(Keyword::As) {
                let target_ty = self.parse_type_expr()?;
                let span = Span {
                    range: expr.span.range.start..target_ty.span.range.end,
                    file: None,
                };
                expr = Expr {
                    kind: ExprKind::Cast {
                        expr: Box::new(expr),
                        target_ty,
                    },
                    span,
                };
            } else {
                break;
            }
        }

        Some(expr)
    }

    fn parse_additive(&mut self) -> Option<Expr> {
        let mut expr = self.parse_multiplicative()?;
        loop {
            let op = if self.matches_token(&TokenKind::Plus) {
                Some(BinaryOp::Add)
            } else if self.matches_token(&TokenKind::Minus) {
                Some(BinaryOp::Sub)
            } else {
                None
            };

            if let Some(op) = op {
                let right = self.parse_multiplicative()?;
                let span = Span {
                    range: expr.span.range.start..right.span.range.end,
                    file: None,
                };
                expr = Expr {
                    kind: ExprKind::Binary {
                        op,
                        left: Box::new(expr),
                        right: Box::new(right),
                    },
                    span,
                };
            } else {
                break;
            }
        }
        Some(expr)
    }

    fn parse_multiplicative(&mut self) -> Option<Expr> {
        let mut expr = self.parse_cast()?;
        loop {
            let op = if self.matches_token(&TokenKind::Star) {
                Some(BinaryOp::Mul)
            } else if self.matches_token(&TokenKind::Slash) {
                Some(BinaryOp::Div)
            } else if self.matches_token(&TokenKind::Percent) {
                Some(BinaryOp::Mod)
            } else {
                None
            };

            if let Some(op) = op {
                let right = self.parse_cast()?;
                let span = Span {
                    range: expr.span.range.start..right.span.range.end,
                    file: None,
                };
                expr = Expr {
                    kind: ExprKind::Binary {
                        op,
                        left: Box::new(expr),
                        right: Box::new(right),
                    },
                    span,
                };
            } else {
                break;
            }
        }
        Some(expr)
    }

    fn parse_unary(&mut self) -> Option<Expr> {
        if self.matches_token(&TokenKind::Bang) {
            let op_tok = self.previous().clone();
            let expr = self.parse_unary()?;
            let span = Span {
                range: op_tok.span.range.start..expr.span.range.end,
                file: None,
            };
            return Some(Expr {
                kind: ExprKind::Unary {
                    op: husk_ast::UnaryOp::Not,
                    expr: Box::new(expr),
                },
                span,
            });
        }
        if self.matches_token(&TokenKind::Minus) {
            let op_tok = self.previous().clone();
            let expr = self.parse_unary()?;
            let span = Span {
                range: op_tok.span.range.start..expr.span.range.end,
                file: None,
            };
            return Some(Expr {
                kind: ExprKind::Unary {
                    op: husk_ast::UnaryOp::Neg,
                    expr: Box::new(expr),
                },
                span,
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Option<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.matches_token(&TokenKind::LParen) {
                // Function call - check if it's a println, print, or format with format string
                if let ExprKind::Ident(ref id) = expr.kind {
                    if id.name == "println" {
                        if let Some(format_expr) = self.try_parse_format_print(&expr, true) {
                            expr = format_expr;
                            continue;
                        }
                    } else if id.name == "print" {
                        if let Some(format_expr) = self.try_parse_format_print(&expr, false) {
                            expr = format_expr;
                            continue;
                        }
                    } else if id.name == "format"
                        && let Some(format_expr) = self.try_parse_format(&expr)
                    {
                        expr = format_expr;
                        continue;
                    }
                }

                // Regular function call (no turbofish in this path since we already matched LParen)
                let args = self.parse_argument_list();
                let span = Span {
                    range: expr.span.range.start..self.previous().span.range.end,
                    file: None,
                };
                expr = Expr {
                    kind: ExprKind::Call {
                        callee: Box::new(expr),
                        type_args: Vec::new(),
                        args,
                    },
                    span,
                };
            } else if self.matches_token(&TokenKind::Dot) {
                // Field access, method call, or tuple field access
                let name_tok = self.current().clone();

                // Check for tuple field access: expr.0, expr.1, etc.
                if let TokenKind::IntLiteral(ref s) = name_tok.kind {
                    self.advance();
                    let index = match s.parse::<usize>() {
                        Ok(idx) => idx,
                        Err(_) => {
                            self.error_at_token(
                                &name_tok,
                                "tuple field index must be a valid non-negative integer",
                            );
                            break;
                        }
                    };
                    let span = Span {
                        range: expr.span.range.start..name_tok.span.range.end,
                        file: None,
                    };
                    expr = Expr {
                        kind: ExprKind::TupleField {
                            base: Box::new(expr),
                            index,
                        },
                        span,
                    };
                    continue;
                }

                // Handle chained tuple access like .0.0 which lexes as FloatLiteral("0.0")
                // We split it into two tuple field accesses
                if let TokenKind::FloatLiteral(ref s) = name_tok.kind {
                    // Check if this looks like chained tuple access (e.g., "0.0", "0.1", "1.2")
                    if let Some((first, rest)) = s.split_once('.')
                        && let (Ok(idx1), Ok(idx2)) =
                            (first.parse::<usize>(), rest.parse::<usize>())
                    {
                        self.advance();
                        // First tuple access
                        let mid_span = Span {
                            range: expr.span.range.start..name_tok.span.range.end,
                            file: None,
                        };
                        let first_access = Expr {
                            kind: ExprKind::TupleField {
                                base: Box::new(expr),
                                index: idx1,
                            },
                            span: mid_span.clone(),
                        };
                        // Second tuple access
                        expr = Expr {
                            kind: ExprKind::TupleField {
                                base: Box::new(first_access),
                                index: idx2,
                            },
                            span: mid_span,
                        };
                        continue;
                    }
                }

                let ident = match &name_tok.kind {
                    TokenKind::Ident(s) => {
                        self.advance();
                        Ident {
                            name: s.clone(),
                            span: self.ast_span_from(&name_tok.span),
                        }
                    }
                    _ => {
                        self.error_here("expected identifier or tuple index after `.`");
                        break;
                    }
                };

                // Check for turbofish: expr.ident::<Type, ...>(args)
                let type_args = if self.matches_token(&TokenKind::ColonColon) {
                    self.parse_turbofish_type_args()
                } else {
                    Vec::new()
                };

                if self.matches_token(&TokenKind::LParen) {
                    // Method call: expr.ident(args...) or expr.ident::<T>(args...)
                    let args = self.parse_argument_list();
                    let span = Span {
                        range: expr.span.range.start..self.previous().span.range.end,
                        file: None,
                    };
                    expr = Expr {
                        kind: ExprKind::MethodCall {
                            receiver: Box::new(expr),
                            method: ident,
                            type_args,
                            args,
                        },
                        span,
                    };
                } else if !type_args.is_empty() {
                    // Turbofish without call - error or allow as partial application?
                    self.error_here("expected `(` after turbofish type arguments");
                    break;
                } else {
                    // Field access: expr.ident
                    let span = Span {
                        range: expr.span.range.start..ident.span.range.end,
                        file: None,
                    };
                    expr = Expr {
                        kind: ExprKind::Field {
                            base: Box::new(expr),
                            member: ident,
                        },
                        span,
                    };
                }
            } else if self.matches_token(&TokenKind::LBracket) {
                // Array indexing or slicing: expr[index] or expr[start..end] etc.
                let bracket_start = self.previous().span.range.start;

                // Check for slice syntax starting with `..` (no start bound)
                if self.matches_token(&TokenKind::DotDot) {
                    // Slice with no start: arr[..end] or arr[..]
                    let (end_expr, inclusive) =
                        if matches!(self.current().kind, TokenKind::RBracket) {
                            // arr[..] - full slice
                            (None, false)
                        } else {
                            // arr[..end]
                            (Some(Box::new(self.parse_comparison()?)), false)
                        };
                    if !self.matches_token(&TokenKind::RBracket) {
                        self.error_here("expected `]` after slice expression");
                        return None;
                    }
                    let span = Span {
                        range: expr.span.range.start..self.previous().span.range.end,
                        file: None,
                    };
                    let range_span = Span {
                        range: bracket_start..self.previous().span.range.end,
                        file: None,
                    };
                    expr = Expr {
                        kind: ExprKind::Index {
                            base: Box::new(expr),
                            index: Box::new(Expr {
                                kind: ExprKind::Range {
                                    start: None,
                                    end: end_expr,
                                    inclusive,
                                },
                                span: range_span,
                            }),
                        },
                        span,
                    };
                } else if self.matches_token(&TokenKind::DotDotEq) {
                    // Slice with no start but inclusive: arr[..=end]
                    let end_expr = Some(Box::new(self.parse_comparison()?));
                    if !self.matches_token(&TokenKind::RBracket) {
                        self.error_here("expected `]` after slice expression");
                        return None;
                    }
                    let span = Span {
                        range: expr.span.range.start..self.previous().span.range.end,
                        file: None,
                    };
                    let range_span = Span {
                        range: bracket_start..self.previous().span.range.end,
                        file: None,
                    };
                    expr = Expr {
                        kind: ExprKind::Index {
                            base: Box::new(expr),
                            index: Box::new(Expr {
                                kind: ExprKind::Range {
                                    start: None,
                                    end: end_expr,
                                    inclusive: true,
                                },
                                span: range_span,
                            }),
                        },
                        span,
                    };
                } else {
                    // Parse the first expression (could be simple index or start of range)
                    let first_expr = self.parse_comparison()?;

                    // Check if this is a range slice
                    if self.matches_token(&TokenKind::DotDot) {
                        // Slice: arr[start..end] or arr[start..]
                        let end_expr = if matches!(self.current().kind, TokenKind::RBracket) {
                            // arr[start..] - slice from start to end
                            None
                        } else {
                            // arr[start..end]
                            Some(Box::new(self.parse_comparison()?))
                        };
                        if !self.matches_token(&TokenKind::RBracket) {
                            self.error_here("expected `]` after slice expression");
                            return None;
                        }
                        let span = Span {
                            range: expr.span.range.start..self.previous().span.range.end,
                            file: None,
                        };
                        let range_span = Span {
                            range: first_expr.span.range.start..self.previous().span.range.end,
                            file: None,
                        };
                        expr = Expr {
                            kind: ExprKind::Index {
                                base: Box::new(expr),
                                index: Box::new(Expr {
                                    kind: ExprKind::Range {
                                        start: Some(Box::new(first_expr)),
                                        end: end_expr,
                                        inclusive: false,
                                    },
                                    span: range_span,
                                }),
                            },
                            span,
                        };
                    } else if self.matches_token(&TokenKind::DotDotEq) {
                        // Inclusive slice: arr[start..=end]
                        let end_expr = Some(Box::new(self.parse_comparison()?));
                        if !self.matches_token(&TokenKind::RBracket) {
                            self.error_here("expected `]` after slice expression");
                            return None;
                        }
                        let span = Span {
                            range: expr.span.range.start..self.previous().span.range.end,
                            file: None,
                        };
                        let range_span = Span {
                            range: first_expr.span.range.start..self.previous().span.range.end,
                            file: None,
                        };
                        expr = Expr {
                            kind: ExprKind::Index {
                                base: Box::new(expr),
                                index: Box::new(Expr {
                                    kind: ExprKind::Range {
                                        start: Some(Box::new(first_expr)),
                                        end: end_expr,
                                        inclusive: true,
                                    },
                                    span: range_span,
                                }),
                            },
                            span,
                        };
                    } else {
                        // Simple index: arr[index]
                        if !self.matches_token(&TokenKind::RBracket) {
                            self.error_here("expected `]` after array index");
                            return None;
                        }
                        let span = Span {
                            range: expr.span.range.start..self.previous().span.range.end,
                            file: None,
                        };
                        expr = Expr {
                            kind: ExprKind::Index {
                                base: Box::new(expr),
                                index: Box::new(first_expr),
                            },
                            span,
                        };
                    }
                }
            } else if self.matches_token(&TokenKind::Question) {
                // Try expression: expr?
                // Desugars to early return on Err/None
                let span = Span {
                    range: expr.span.range.start..self.previous().span.range.end,
                    file: None,
                };
                expr = Expr {
                    kind: ExprKind::Try {
                        expr: Box::new(expr),
                    },
                    span,
                };
            } else {
                break;
            }
        }
        Some(expr)
    }

    fn parse_argument_list(&mut self) -> Vec<Expr> {
        let mut args = Vec::new();
        if self.matches_token(&TokenKind::RParen) {
            return args;
        }
        while let Some(arg) = self.parse_expr() {
            args.push(arg);
            if self.matches_token(&TokenKind::RParen) {
                break;
            }
            if !self.matches_token(&TokenKind::Comma) {
                self.error_here("expected `,` or `)` in argument list");
                break;
            }
        }
        args
    }

    /// Parse turbofish type arguments: `::<Type, Type, ...>`.
    /// Called after `::` has been consumed. Expects `<` next.
    fn parse_turbofish_type_args(&mut self) -> Vec<TypeExpr> {
        if !self.matches_token(&TokenKind::Lt) {
            self.error_here("expected `<` after `::` in turbofish");
            return Vec::new();
        }

        let mut type_args = Vec::new();
        while let Some(ty) = self.parse_type_expr() {
            type_args.push(ty);
            if self.matches_token(&TokenKind::Gt) {
                break;
            }
            if !self.matches_token(&TokenKind::Comma) {
                self.error_here("expected `,` or `>` in turbofish type arguments");
                break;
            }
        }

        type_args
    }

    /// Try to parse a println/print call as a FormatPrint expression.
    /// Returns None if first argument is not a string literal with format placeholders,
    /// in which case it falls back to a regular Call.
    /// We're positioned right after the `(` of `println(...)` or `print(...)`.
    fn try_parse_format_print(&mut self, callee_expr: &Expr, newline: bool) -> Option<Expr> {
        let start = callee_expr.span.range.start;

        // Check if first token is a string literal
        let format_tok = self.current().clone();
        let format_str = match &format_tok.kind {
            TokenKind::StringLiteral(s) => s.clone(),
            _ => return None, // Not a string literal, fall back to regular call
        };

        // Parse the format string to check if it has placeholders
        let format_span = self.ast_span_from(&format_tok.span);
        let mut parsed_format = self.parse_format_string(&format_str, &format_span);

        // Advance past the format string
        self.advance();

        // Collect remaining arguments
        let mut args = Vec::new();
        if !self.matches_token(&TokenKind::RParen) {
            // There are more arguments after the format string
            if !self.matches_token(&TokenKind::Comma) {
                self.error_here("expected `,` or `)` after format string");
                return None;
            }
            while let Some(arg) = self.parse_expr() {
                args.push(arg);
                if self.matches_token(&TokenKind::RParen) {
                    break;
                }
                if !self.matches_token(&TokenKind::Comma) {
                    self.error_here("expected `,` or `)` in argument list");
                    break;
                }
            }
        }

        // Synthesize implicit arguments from named placeholders (Rust-style {var_name})
        self.synthesize_named_placeholder_args(&mut parsed_format, &mut args);

        let end = self.previous().span.range.end;

        Some(Expr {
            kind: ExprKind::FormatPrint {
                format: parsed_format,
                args,
                newline,
            },
            span: Span {
                range: start..end,
                file: None,
            },
        })
    }

    /// Try to parse a format call as a Format expression.
    /// Returns None if first argument is not a string literal with format placeholders,
    /// in which case it falls back to a regular Call.
    /// We're positioned right after the `(` of `format(...)`.
    fn try_parse_format(&mut self, callee_expr: &Expr) -> Option<Expr> {
        let start = callee_expr.span.range.start;

        // Check if first token is a string literal
        let format_tok = self.current().clone();
        let format_str = match &format_tok.kind {
            TokenKind::StringLiteral(s) => s.clone(),
            _ => return None, // Not a string literal, fall back to regular call
        };

        // Parse the format string to check if it has placeholders
        let format_span = self.ast_span_from(&format_tok.span);
        let mut parsed_format = self.parse_format_string(&format_str, &format_span);

        // Advance past the format string
        self.advance();

        // Collect remaining arguments
        let mut args = Vec::new();
        if !self.matches_token(&TokenKind::RParen) {
            // There are more arguments after the format string
            if !self.matches_token(&TokenKind::Comma) {
                self.error_here("expected `,` or `)` after format string");
                return None;
            }
            while let Some(arg) = self.parse_expr() {
                args.push(arg);
                if self.matches_token(&TokenKind::RParen) {
                    break;
                }
                if !self.matches_token(&TokenKind::Comma) {
                    self.error_here("expected `,` or `)` in argument list");
                    break;
                }
            }
        }

        // Synthesize implicit arguments from named placeholders (Rust-style {var_name})
        self.synthesize_named_placeholder_args(&mut parsed_format, &mut args);

        let end = self.previous().span.range.end;

        Some(Expr {
            kind: ExprKind::Format {
                format: parsed_format,
                args,
            },
            span: Span {
                range: start..end,
                file: None,
            },
        })
    }

    /// Parse a format string like "Hello, {}! Value: {:x}" into segments.
    fn parse_format_string(&mut self, s: &str, span: &Span) -> FormatString {
        let mut segments = Vec::new();
        let mut chars = s.chars().peekable();
        let mut current_literal = String::new();
        let mut char_offset = 0usize;

        while let Some(c) = chars.next() {
            if c == '{' {
                // Check for escaped brace {{
                if chars.peek() == Some(&'{') {
                    chars.next();
                    current_literal.push('{');
                    char_offset += 2;
                    continue;
                }

                // Start of placeholder - save any accumulated literal
                if !current_literal.is_empty() {
                    segments.push(FormatSegment::Literal(current_literal.clone()));
                    current_literal.clear();
                }

                // Parse the placeholder contents
                let placeholder_start = char_offset;
                let mut placeholder_content = String::new();
                char_offset += 1; // for the '{'

                while let Some(&next) = chars.peek() {
                    if next == '}' {
                        chars.next();
                        char_offset += 1;
                        break;
                    }
                    placeholder_content.push(chars.next().unwrap());
                    char_offset += 1;
                }

                let placeholder_span = Span {
                    range: span.range.start + placeholder_start..span.range.start + char_offset,
                    file: None,
                };
                let placeholder = self.parse_placeholder(&placeholder_content, &placeholder_span);
                segments.push(FormatSegment::Placeholder(placeholder));
            } else if c == '}' {
                // Check for escaped brace }}
                if chars.peek() == Some(&'}') {
                    chars.next();
                    current_literal.push('}');
                    char_offset += 2;
                    continue;
                }
                // Unmatched } - treat as error but continue
                current_literal.push('}');
                char_offset += 1;
            } else {
                current_literal.push(c);
                char_offset += 1;
            }
        }

        // Add any remaining literal
        if !current_literal.is_empty() {
            segments.push(FormatSegment::Literal(current_literal));
        }

        FormatString {
            span: span.clone(),
            segments,
        }
    }

    /// Synthesize implicit arguments from named placeholders (Rust-style {var_name}).
    ///
    /// When a placeholder has a name like `{var_name}` without a corresponding explicit
    /// argument, this creates an identifier expression for `var_name` and adds it to the
    /// args list, then assigns the correct position to the placeholder.
    ///
    /// For example, `println("{x} {y}")` becomes equivalent to `println("{} {}", x, y)`.
    fn synthesize_named_placeholder_args(&self, format: &mut FormatString, args: &mut Vec<Expr>) {
        // Track named placeholders we've seen and their assigned positions
        let mut named_positions: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        // Assign positions to named placeholders and synthesize args.
        // Use args.len() as the position to ensure the assigned position always matches
        // where the synthetic arg actually ends up in the vector, regardless of how many
        // explicit args were passed by the caller.
        for segment in &mut format.segments {
            if let FormatSegment::Placeholder(ph) = segment
                && let Some(name) = &ph.name
            {
                // Check if we've already seen this name
                if let Some(&pos) = named_positions.get(name) {
                    // Reuse the same position
                    ph.position = Some(pos);
                } else {
                    // Synthesize at current end of args vector
                    let pos = args.len();
                    named_positions.insert(name.clone(), pos);
                    ph.position = Some(pos);

                    // Create identifier expression for this variable
                    args.push(Expr {
                        kind: ExprKind::Ident(Ident {
                            name: name.clone(),
                            span: ph.span.clone(),
                        }),
                        span: ph.span.clone(),
                    });
                }
            }
        }
    }

    /// Parse placeholder content like "", "0", "name", ":?", ":x", "0:08x", etc.
    fn parse_placeholder(&self, content: &str, span: &Span) -> FormatPlaceholder {
        let mut position: Option<usize> = None;
        let mut name: Option<String> = None;
        let mut spec = FormatSpec::default();

        // Split on ':' to separate argument from format spec
        let (arg_part, spec_part) = match content.find(':') {
            Some(idx) => (&content[..idx], Some(&content[idx + 1..])),
            None => (content, None),
        };

        // Parse the argument part (position or name)
        if !arg_part.is_empty() {
            if let Ok(pos) = arg_part.parse::<usize>() {
                position = Some(pos);
            } else if arg_part.chars().all(|c| c.is_alphanumeric() || c == '_') {
                name = Some(arg_part.to_string());
            }
        }

        // Parse the format spec part
        if let Some(spec_str) = spec_part {
            spec = self.parse_format_spec(spec_str);
        }

        FormatPlaceholder {
            position,
            name,
            spec,
            span: span.clone(),
        }
    }

    /// Parse format spec like "?", "#?", "x", "08x", "<10", "*^20.5", etc.
    /// Format: [[fill]align][sign]['#']['0'][width]['.' precision][type]
    fn parse_format_spec(&self, s: &str) -> FormatSpec {
        let mut spec = FormatSpec::default();
        let mut chars = s.chars().peekable();

        // Parse fill and align
        // Fill is any character followed by an alignment (<, >, ^)
        // Or just alignment alone
        if let Some(&first) = chars.peek() {
            let mut lookahead = chars.clone();
            lookahead.next();
            if let Some(&second) = lookahead.peek() {
                if second == '<' || second == '>' || second == '^' {
                    spec.fill = Some(first);
                    spec.align = Some(second);
                    chars.next();
                    chars.next();
                } else if first == '<' || first == '>' || first == '^' {
                    spec.align = Some(first);
                    chars.next();
                }
            } else if first == '<' || first == '>' || first == '^' {
                spec.align = Some(first);
                chars.next();
            }
        }

        // Parse sign (+)
        if chars.peek() == Some(&'+') {
            spec.sign = true;
            chars.next();
        }

        // Parse alternate form (#)
        if chars.peek() == Some(&'#') {
            spec.alternate = true;
            chars.next();
        }

        // Parse zero padding (0) - must come before width
        if chars.peek() == Some(&'0') {
            // Peek ahead to see if there are more digits (width) or if this is just '0'
            let mut lookahead = chars.clone();
            lookahead.next();
            match lookahead.peek() {
                Some(&c) if c.is_ascii_digit() => {
                    // This is zero-padding prefix followed by width
                    spec.zero_pad = true;
                    chars.next();
                }
                Some(&'.') | Some(&'?') | Some(&'x') | Some(&'X') | Some(&'b') | Some(&'o')
                | None => {
                    // Just a width of 0, or followed by precision/type
                    // Don't consume, let width parsing handle it
                }
                _ => {
                    // Zero padding
                    spec.zero_pad = true;
                    chars.next();
                }
            }
        }

        // Parse width (digits)
        let mut width_str = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                width_str.push(c);
                chars.next();
            } else {
                break;
            }
        }
        if !width_str.is_empty() {
            spec.width = width_str.parse().ok();
        }

        // Parse precision (.N)
        if chars.peek() == Some(&'.') {
            chars.next();
            let mut prec_str = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    prec_str.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            if !prec_str.is_empty() {
                spec.precision = prec_str.parse().ok();
            }
        }

        // Parse type specifier (?, x, X, b, o, e, E)
        if let Some(&c) = chars.peek() {
            match c {
                '?' | 'x' | 'X' | 'b' | 'o' | 'e' | 'E' => {
                    spec.ty = Some(c);
                    chars.next();
                }
                _ => {}
            }
        }

        spec
    }

    fn parse_match_expr(&mut self) -> Option<Expr> {
        let match_tok = self.advance().clone(); // consume `match`
        // Use parse_expr_no_struct so that `match x { ... }` doesn't try to
        // parse `x { ... }` as a struct literal.
        let scrutinee = self.parse_expr_no_struct()?;
        if !self.matches_token(&TokenKind::LBrace) {
            self.error_here("expected `{` after `match` scrutinee");
            return None;
        }

        let mut arms = Vec::new();
        while !self.is_at_end() && !self.matches_token(&TokenKind::RBrace) {
            let pat = self.parse_pattern()?;
            if !self.matches_token(&TokenKind::FatArrow) {
                self.error_here("expected `=>` after match pattern");
                return None;
            }
            // Arm body: either a block `{ ... }` or an expression.
            let arm_expr = if self.matches_token(&TokenKind::LBrace) {
                // We already consumed `{`, so parse the block body.
                // parse_block expects `{` still current, so we need to rewind one step:
                // Instead, construct a block manually:
                // For simplicity, treat `{ expr }` as a block expression by re-parsing.
                self.pos -= 1; // move back to '{'
                let block = self.parse_block()?;
                Expr {
                    kind: ExprKind::Block(block.clone()),
                    span: block.span.clone(),
                }
            } else if matches!(
                self.current().kind,
                TokenKind::Keyword(Keyword::Break) | TokenKind::Keyword(Keyword::Continue)
            ) {
                // Handle break/continue in match arms by wrapping in a block
                let stmt_tok = self.current().clone();
                let is_break = matches!(self.current().kind, TokenKind::Keyword(Keyword::Break));
                self.advance(); // consume break or continue
                let stmt = if is_break {
                    Stmt {
                        kind: StmtKind::Break,
                        span: self.ast_span_from(&stmt_tok.span),
                    }
                } else {
                    Stmt {
                        kind: StmtKind::Continue,
                        span: self.ast_span_from(&stmt_tok.span),
                    }
                };
                let block = Block {
                    stmts: vec![stmt],
                    span: self.ast_span_from(&stmt_tok.span),
                };
                Expr {
                    kind: ExprKind::Block(block.clone()),
                    span: block.span.clone(),
                }
            } else {
                self.parse_expr()?
            };

            arms.push(MatchArm {
                pattern: pat,
                expr: arm_expr,
            });

            // Optional trailing comma between arms.
            let _ = self.matches_token(&TokenKind::Comma);
        }

        let end = self.previous().span.range.end;
        Some(Expr {
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
            span: Span {
                range: match_tok.span.range.start..end,
                file: None,
            },
        })
    }

    fn parse_pattern(&mut self) -> Option<Pattern> {
        let tok = self.current().clone();
        let kind = match &tok.kind {
            // Tuple pattern: (a, b, c)
            TokenKind::LParen => {
                let start = tok.span.range.start;
                self.advance(); // consume `(`

                // Empty tuple pattern: ()
                if self.matches_token(&TokenKind::RParen) {
                    let end = self.previous().span.range.end;
                    return Some(Pattern {
                        kind: PatternKind::Tuple { fields: Vec::new() },
                        span: Span {
                            range: start..end,
                            file: None,
                        },
                    });
                }

                // Parse tuple pattern fields with optional trailing comma
                let mut fields = Vec::new();
                let mut had_trailing_comma = false;
                loop {
                    let field = self.parse_pattern()?;
                    fields.push(field);

                    if self.matches_token(&TokenKind::RParen) {
                        break;
                    }
                    if !self.matches_token(&TokenKind::Comma) {
                        self.error_here("expected `,` or `)` in tuple pattern");
                        return None;
                    }
                    // If we see `)` immediately after a comma, remember that there was a trailing comma.
                    had_trailing_comma = true;
                    if self.matches_token(&TokenKind::RParen) {
                        break;
                    }
                    // Reset if we continue parsing more elements
                    had_trailing_comma = false;
                }

                let end = self.previous().span.range.end;

                // Single element in parens is just grouping, not a tuple,
                // unless we saw a trailing comma: (x,) is a 1-tuple pattern.
                if fields.len() == 1 && !had_trailing_comma {
                    // Just return the inner pattern (parenthesized pattern)
                    return Some(fields.pop().unwrap());
                }

                return Some(Pattern {
                    kind: PatternKind::Tuple { fields },
                    span: Span {
                        range: start..end,
                        file: None,
                    },
                });
            }
            TokenKind::Ident(name) => {
                // Could be binding (`x`) or enum path (`Enum::Variant`).
                let first = Ident {
                    name: name.clone(),
                    span: self.ast_span_from(&tok.span),
                };
                self.advance();

                // Check for wildcard `_`
                if first.name == "_" {
                    return Some(Pattern {
                        kind: PatternKind::Wildcard,
                        span: self.ast_span_from(&tok.span),
                    });
                }

                let mut path = self.parse_path_segments(first);

                if self.matches_token(&TokenKind::LParen) {
                    // Enum tuple pattern: Enum::Variant(x, y) or imported variant Some(x)
                    let mut fields = Vec::new();
                    while !self.is_at_end() && self.current().kind != TokenKind::RParen {
                        fields.push(self.parse_pattern()?);
                        if !self.matches_token(&TokenKind::Comma) {
                            break;
                        }
                    }
                    if !self.matches_token(&TokenKind::RParen) {
                        self.error_here("expected `)` after enum tuple pattern fields");
                        return None;
                    }
                    PatternKind::EnumTuple { path, fields }
                } else if path.len() == 1 {
                    // Single identifier without parens: treat as binding pattern.
                    PatternKind::Binding(path.pop().unwrap())
                } else {
                    // Enum unit pattern: Enum::Variant
                    PatternKind::EnumUnit { path }
                }
            }
            _ => {
                self.error_here("unexpected token in pattern");
                return None;
            }
        };

        Some(Pattern {
            kind,
            span: self.ast_span_from(&tok.span),
        })
    }
    /// Parse a closure expression: `|x, y| expr` or `|x: i32| -> i32 { expr }` or `|| expr`
    fn parse_closure_expr(&mut self) -> Option<Expr> {
        let start_tok = self.current().clone();
        let start = start_tok.span.range.start;

        // Handle `||` (empty param list) or `|` (start of param list)
        let params = if self.matches_token(&TokenKind::OrOr) {
            // `|| expr` - no parameters
            Vec::new()
        } else if self.matches_token(&TokenKind::Pipe) {
            // `|x, y| expr` - parse parameter list
            let params = self.parse_closure_param_list();
            if !self.matches_token(&TokenKind::Pipe) {
                self.error_here("expected `|` after closure parameters");
                return None;
            }
            params
        } else {
            self.error_here("expected `|` or `||` to start closure");
            return None;
        };

        // Optional return type: `-> Type`
        let ret_type = if self.matches_token(&TokenKind::Arrow) {
            Some(self.parse_type_expr()?)
        } else {
            None
        };

        // Body: either a block `{ ... }` or a single expression
        let body = if matches!(self.current().kind, TokenKind::LBrace) {
            let block = self.parse_block()?;
            Expr {
                kind: ExprKind::Block(block.clone()),
                span: block.span.clone(),
            }
        } else {
            self.parse_expr()?
        };

        let end = body.span.range.end;

        Some(Expr {
            kind: ExprKind::Closure {
                params,
                ret_type,
                body: Box::new(body),
            },
            span: Span {
                range: start..end,
                file: None,
            },
        })
    }

    /// Parse closure parameter list: `x, y: i32, z`
    fn parse_closure_param_list(&mut self) -> Vec<ClosureParam> {
        let mut params = Vec::new();

        // Handle empty param list (just `||`)
        if matches!(self.current().kind, TokenKind::Pipe) {
            return params;
        }

        while let Some(name) = self.parse_ident("expected parameter name in closure") {
            // Optional type annotation: `: Type`
            let ty = if self.matches_token(&TokenKind::Colon) {
                self.parse_type_expr()
            } else {
                None
            };

            params.push(ClosureParam { name, ty });

            // Check for closing pipe or comma
            if matches!(self.current().kind, TokenKind::Pipe) {
                break;
            }
            if !self.matches_token(&TokenKind::Comma) {
                break;
            }
        }

        params
    }

    /// Parse an `if` expression (expression position). Requires `else` so it has a value.
    fn parse_if_expr(&mut self) -> Option<Expr> {
        let if_tok = self.advance().clone(); // consume `if`

        // Condition shouldn't parse struct literals
        let cond = self.parse_expr_no_struct()?;

        // then branch: block or single expression
        let then_branch = if matches!(self.current().kind, TokenKind::LBrace) {
            let block = self.parse_block()?;
            Expr {
                kind: ExprKind::Block(block.clone()),
                span: block.span.clone(),
            }
        } else {
            self.parse_expr()?
        };

        if !self.matches_keyword(Keyword::Else) {
            self.error_here("`if` expressions require an `else` branch");
            return None;
        }

        let else_branch = if matches!(self.current().kind, TokenKind::LBrace) {
            let block = self.parse_block()?;
            Expr {
                kind: ExprKind::Block(block.clone()),
                span: block.span.clone(),
            }
        } else {
            self.parse_expr()?
        };

        let end = else_branch.span.range.end;
        Some(Expr {
            kind: ExprKind::If {
                cond: Box::new(cond),
                then_branch: Box::new(then_branch),
                else_branch: Box::new(else_branch),
            },
            span: Span {
                range: if_tok.span.range.start..end,
                file: None,
            },
        })
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        let tok = self.current().clone();
        match tok.kind {
            // Closure expressions: `|x, y| expr` or `|| expr`
            TokenKind::Pipe | TokenKind::OrOr => self.parse_closure_expr(),
            TokenKind::Keyword(Keyword::If) => self.parse_if_expr(),
            TokenKind::IntLiteral(ref s) => {
                self.advance();
                let value = s.parse::<i64>().unwrap_or(0);
                Some(Expr {
                    kind: ExprKind::Literal(Literal {
                        kind: LiteralKind::Int(value),
                        span: self.ast_span_from(&tok.span),
                    }),
                    span: self.ast_span_from(&tok.span),
                })
            }
            TokenKind::FloatLiteral(ref s) => {
                self.advance();
                let value = s.parse::<f64>().unwrap_or(0.0);
                Some(Expr {
                    kind: ExprKind::Literal(Literal {
                        kind: LiteralKind::Float(value),
                        span: self.ast_span_from(&tok.span),
                    }),
                    span: self.ast_span_from(&tok.span),
                })
            }
            TokenKind::StringLiteral(ref s) => {
                self.advance();
                Some(Expr {
                    kind: ExprKind::Literal(Literal {
                        kind: LiteralKind::String(s.clone()),
                        span: self.ast_span_from(&tok.span),
                    }),
                    span: self.ast_span_from(&tok.span),
                })
            }
            TokenKind::Keyword(Keyword::True) => {
                self.advance();
                Some(Expr {
                    kind: ExprKind::Literal(Literal {
                        kind: LiteralKind::Bool(true),
                        span: self.ast_span_from(&tok.span),
                    }),
                    span: self.ast_span_from(&tok.span),
                })
            }
            TokenKind::Keyword(Keyword::False) => {
                self.advance();
                Some(Expr {
                    kind: ExprKind::Literal(Literal {
                        kind: LiteralKind::Bool(false),
                        span: self.ast_span_from(&tok.span),
                    }),
                    span: self.ast_span_from(&tok.span),
                })
            }
            TokenKind::Ident(ref name) => {
                // Could be a simple identifier, a path like `Enum::Variant`,
                // or a struct literal like `Point { x: 1, y: 2 }`.
                let first = Ident {
                    name: name.clone(),
                    span: self.ast_span_from(&tok.span),
                };
                self.advance();
                let path = self.parse_path_segments(first.clone());

                // Check for struct literal: `Name { ... }` or `Path::Name { ... }`
                // Only parse as struct literal if allowed in this context.
                if self.allow_struct_expr && self.current().kind == TokenKind::LBrace {
                    return self.parse_struct_expr(path);
                }

                if path.len() == 1 {
                    Some(Expr {
                        kind: ExprKind::Ident(first.clone()),
                        span: first.span,
                    })
                } else {
                    let start = path
                        .first()
                        .map(|id| id.span.range.start)
                        .unwrap_or(tok.span.range.start);
                    let end = path
                        .last()
                        .map(|id| id.span.range.end)
                        .unwrap_or(tok.span.range.end);
                    Some(Expr {
                        kind: ExprKind::Path { segments: path },
                        span: Span {
                            range: start..end,
                            file: None,
                        },
                    })
                }
            }
            TokenKind::LParen => {
                let start = tok.span.range.start;
                self.advance(); // consume `(`

                // Check for empty tuple / unit value `()`
                if self.matches_token(&TokenKind::RParen) {
                    let end = self.previous().span.range.end;
                    return Some(Expr {
                        kind: ExprKind::Tuple {
                            elements: Vec::new(),
                        },
                        span: Span {
                            range: start..end,
                            file: None,
                        },
                    });
                }

                // Parse the first expression
                let first_expr = self.parse_expr()?;

                // If we see `)`, it's just a grouped expression
                if self.matches_token(&TokenKind::RParen) {
                    return Some(first_expr);
                }

                // If we see `,`, it's a tuple
                if !self.matches_token(&TokenKind::Comma) {
                    self.error_here("expected `)` or `,` after expression");
                    return None;
                }

                // Single element tuple with trailing comma: (expr,)
                if self.matches_token(&TokenKind::RParen) {
                    let end = self.previous().span.range.end;
                    return Some(Expr {
                        kind: ExprKind::Tuple {
                            elements: vec![first_expr],
                        },
                        span: Span {
                            range: start..end,
                            file: None,
                        },
                    });
                }

                // Multi-element tuple: (expr1, expr2, ...)
                let mut elements = vec![first_expr];
                loop {
                    let expr = self.parse_expr()?;
                    elements.push(expr);

                    if self.matches_token(&TokenKind::RParen) {
                        break;
                    }
                    if !self.matches_token(&TokenKind::Comma) {
                        self.error_here("expected `,` or `)` in tuple expression");
                        return None;
                    }
                    // Allow trailing comma: (a, b,)
                    if self.matches_token(&TokenKind::RParen) {
                        break;
                    }
                }

                let end = self.previous().span.range.end;
                Some(Expr {
                    kind: ExprKind::Tuple { elements },
                    span: Span {
                        range: start..end,
                        file: None,
                    },
                })
            }
            TokenKind::LBracket => self.parse_array_expr(),
            TokenKind::Keyword(Keyword::Match) => self.parse_match_expr(),
            TokenKind::Keyword(Keyword::Js) => self.parse_js_literal(),
            _ => {
                self.error_at_token(&tok, "expected expression");
                None
            }
        }
    }

    /// Parse a `js { ... }` literal expression for embedding raw JavaScript.
    ///
    /// Handles:
    /// - Nested braces in object literals: `js { { a: { b: 1 } } }`
    /// - Braces in strings: `js { "text } here" }`
    /// - Braces in comments: `js { /* } */ code }`
    /// - Template literals: `` js { `${x}` } ``
    fn parse_js_literal(&mut self) -> Option<Expr> {
        let start_tok = self.advance().clone(); // consume `js`

        if !self.matches_token(&TokenKind::LBrace) {
            self.error_here("expected `{` after `js`");
            return None;
        }

        // The opening brace was just consumed. Its position marks the start of content.
        let content_start = self.previous().span.range.end;

        // Extract the raw content between braces using a state machine
        let code = self.extract_js_block_content(content_start)?;

        let end = self.previous().span.range.end;
        Some(Expr {
            kind: ExprKind::JsLiteral { code },
            span: Span::new(start_tok.span.range.start, end),
        })
    }

    /// Extract JavaScript content between braces, tracking depth and handling
    /// strings/comments to avoid false matches on braces within them.
    fn extract_js_block_content(&mut self, content_start: usize) -> Option<String> {
        use JsParseState::*;

        let mut depth = 1usize; // Already consumed opening {
        let mut state = Normal;
        let mut pos = content_start;
        let source_bytes = self.source.as_bytes();
        let source_len = source_bytes.len();

        while depth > 0 && pos < source_len {
            let ch = source_bytes[pos] as char;
            let prev_ch = if pos > 0 {
                source_bytes[pos - 1] as char
            } else {
                '\0'
            };
            let next_ch = if pos + 1 < source_len {
                source_bytes[pos + 1] as char
            } else {
                '\0'
            };

            match state {
                Normal => match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            // Don't include the closing brace
                            break;
                        }
                    }
                    '"' => state = InDoubleQuoteString,
                    '\'' => state = InSingleQuoteString,
                    '`' => state = InTemplateString { template_depth: 0 },
                    '/' if next_ch == '/' => {
                        state = InSingleLineComment;
                        pos += 1; // skip second /
                    }
                    '/' if next_ch == '*' => {
                        state = InMultiLineComment;
                        pos += 1; // skip *
                    }
                    _ => {}
                },
                InDoubleQuoteString => {
                    if ch == '"' && prev_ch != '\\' {
                        state = Normal;
                    }
                }
                InSingleQuoteString => {
                    if ch == '\'' && prev_ch != '\\' {
                        state = Normal;
                    }
                }
                InTemplateString { template_depth } => match ch {
                    '`' if prev_ch != '\\' => {
                        if template_depth == 0 {
                            state = Normal;
                        }
                    }
                    '$' if next_ch == '{' => {
                        pos += 1; // skip {
                        state = InTemplateString {
                            template_depth: template_depth + 1,
                        };
                    }
                    '{' if template_depth > 0 => {
                        state = InTemplateString {
                            template_depth: template_depth + 1,
                        };
                    }
                    '}' if template_depth > 0 => {
                        state = InTemplateString {
                            template_depth: template_depth - 1,
                        };
                    }
                    _ => {}
                },
                InSingleLineComment => {
                    if ch == '\n' {
                        state = Normal;
                    }
                }
                InMultiLineComment => {
                    if ch == '*' && next_ch == '/' {
                        pos += 1; // skip /
                        state = Normal;
                    }
                }
            }
            pos += 1;
        }

        if depth != 0 {
            self.error_here("unclosed `js` block");
            return None;
        }

        // Extract the content (excluding the closing brace)
        let content = &self.source[content_start..pos];
        let trimmed = content.trim().to_string();

        // Advance the token position past the closing brace
        // Use binary search to find the token at or after pos (O(log n) instead of O(n))
        self.pos = self.find_token_at_or_after(pos);

        // Consume the closing brace token
        if self.current().kind == TokenKind::RBrace {
            self.advance();
        }

        Some(trimmed)
    }

    fn parse_array_expr(&mut self) -> Option<Expr> {
        let start = self.current().span.range.start;
        self.advance(); // consume '['

        let mut elements = Vec::new();

        // Handle empty array or elements
        if self.current().kind != TokenKind::RBracket {
            loop {
                elements.push(self.parse_expr()?);

                if !self.matches_token(&TokenKind::Comma) {
                    break;
                }

                // Allow trailing comma
                if self.current().kind == TokenKind::RBracket {
                    break;
                }
            }
        }

        let end = self.current().span.range.end;
        if !self.matches_token(&TokenKind::RBracket) {
            self.error_here("expected `]` after array elements");
            return None;
        }

        Some(Expr {
            kind: ExprKind::Array { elements },
            span: Span {
                range: start..end,
                file: None,
            },
        })
    }

    /// Parse a struct instantiation expression: `Name { field: value, ... }`.
    /// The `name` path has already been parsed; we're positioned at `{`.
    fn parse_struct_expr(&mut self, name: Vec<Ident>) -> Option<Expr> {
        let start = name
            .first()
            .map(|id| id.span.range.start)
            .unwrap_or(self.current().span.range.start);

        // Consume the `{`
        self.advance();

        let mut fields = Vec::new();

        // Parse field initializers: `field: expr, ...`
        while self.current().kind != TokenKind::RBrace && self.current().kind != TokenKind::Eof {
            let field_name = self.parse_ident("expected field name")?;

            if !self.matches_token(&TokenKind::Colon) {
                self.error_here("expected `:` after field name");
                return None;
            }

            let value = self.parse_expr()?;

            fields.push(husk_ast::FieldInit {
                name: field_name,
                value,
            });

            // Optional trailing comma
            if !self.matches_token(&TokenKind::Comma) {
                break;
            }
        }

        if !self.matches_token(&TokenKind::RBrace) {
            self.error_here("expected `}` after struct fields");
            return None;
        }

        let end = self.previous().span.range.end;

        Some(Expr {
            kind: ExprKind::Struct { name, fields },
            span: Span {
                range: start..end,
                file: None,
            },
        })
    }
}

/// Derive a valid Husk identifier from an npm package name.
///
/// This function converts npm package names into valid Husk identifiers by:
/// - Stripping scopes (e.g., `@scope/pkg` -> `pkg`)
/// - Replacing hyphens with underscores (e.g., `lodash-es` -> `lodash_es`)
/// - Prefixing with `_` if the result starts with a digit
/// - Falling back to `"pkg"` if the result would be empty
///
/// # Examples
/// - `express` -> `express`
/// - `lodash-es` -> `lodash_es`
/// - `@scope/pkg` -> `pkg`
/// - `@scope/my-pkg` -> `my_pkg`
/// - `3d-viewer` -> `_3d_viewer`
pub fn derive_binding_from_package(package: &str) -> String {
    // If scoped package (@scope/name), use only the name part
    let name = if let Some(slash_pos) = package.rfind('/') {
        &package[slash_pos + 1..]
    } else {
        package
    };

    // Replace hyphens with underscores to make it a valid identifier
    let mut result = String::new();
    for ch in name.chars() {
        if ch == '-' {
            result.push('_');
        } else if ch.is_alphanumeric() || ch == '_' {
            result.push(ch);
        }
        // Skip other characters (like @, /)
    }

    // Ensure the identifier doesn't start with a digit
    if result
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        result.insert(0, '_');
    }

    // If empty, use a default
    if result.is_empty() {
        result = "pkg".to_string();
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_binding_from_simple_package() {
        assert_eq!(derive_binding_from_package("lodash"), "lodash");
        assert_eq!(derive_binding_from_package("express"), "express");
    }

    #[test]
    fn derive_binding_from_hyphenated_package() {
        assert_eq!(derive_binding_from_package("lodash-es"), "lodash_es");
        assert_eq!(derive_binding_from_package("my-cool-pkg"), "my_cool_pkg");
    }

    #[test]
    fn derive_binding_from_scoped_package() {
        assert_eq!(derive_binding_from_package("@scope/pkg"), "pkg");
        assert_eq!(derive_binding_from_package("@myorg/my-lib"), "my_lib");
    }

    #[test]
    fn derive_binding_handles_numeric_prefix() {
        assert_eq!(derive_binding_from_package("3d-viewer"), "_3d_viewer");
    }

    #[test]
    fn parses_mod_declaration_with_identifier() {
        let src = r#"extern "js" { mod express; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        assert_eq!(file.items.len(), 1);
        if let ItemKind::ExternBlock { items, .. } = &file.items[0].kind {
            assert_eq!(items.len(), 1);
            if let husk_ast::ExternItemKind::Mod {
                package,
                binding,
                items,
                ..
            } = &items[0].kind
            {
                assert_eq!(package, "express");
                assert_eq!(binding.name, "express");
                assert!(items.is_empty());
            } else {
                panic!("expected Mod item");
            }
        } else {
            panic!("expected ExternBlock");
        }
    }

    #[test]
    fn parses_mod_declaration_with_string_literal() {
        let src = r#"extern "js" { mod "lodash-es"; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::ExternBlock { items, .. } = &file.items[0].kind {
            if let husk_ast::ExternItemKind::Mod {
                package,
                binding,
                items,
                ..
            } = &items[0].kind
            {
                assert_eq!(package, "lodash-es");
                assert_eq!(binding.name, "lodash_es");
                assert!(items.is_empty());
            } else {
                panic!("expected Mod item");
            }
        } else {
            panic!("expected ExternBlock");
        }
    }

    #[test]
    fn parses_mod_declaration_with_alias() {
        let src = r#"extern "js" { mod "@myorg/my-lib" as mylib; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::ExternBlock { items, .. } = &file.items[0].kind {
            if let husk_ast::ExternItemKind::Mod {
                package,
                binding,
                items,
                ..
            } = &items[0].kind
            {
                assert_eq!(package, "@myorg/my-lib");
                assert_eq!(binding.name, "mylib");
                assert!(items.is_empty());
            } else {
                panic!("expected Mod item");
            }
        } else {
            panic!("expected ExternBlock");
        }
    }

    #[test]
    fn parse_mod_declaration_with_default() {
        let src = r#"
            extern "js" { 
                mod express { 
                    #[default]
                    fn main() -> JsValue;
                } 
            }
        "#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let ItemKind::ExternBlock { items, .. } = &result.file.unwrap().items[0].kind else {
            panic!("expected ExternBlock");
        };
        let husk_ast::ExternItemKind::Mod {
            package,
            binding,
            items: mod_items,
            ..
        } = &items[0].kind
        else {
            panic!("expected Mod item");
        };

        let item = &mod_items[0];
        assert_eq!(item.attributes[0].name.name, "default");
        assert_eq!(package, "express");
        assert_eq!(binding.name, "express");
    }

    #[test]
    fn parse_mod_declaration_with_default_on_mod() {
        // Test #[default] on the mod itself (not on functions)
        // This indicates the module uses default import, all functions are methods
        let src = r#"
            extern "js" {
                #[default]
                mod validator {
                    fn isEmail(s: String) -> bool;
                }
            }
        "#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let ItemKind::ExternBlock { items, .. } = &result.file.unwrap().items[0].kind else {
            panic!("expected ExternBlock");
        };

        // Check that the extern item (mod) has the #[default] attribute
        let extern_item = &items[0];
        assert_eq!(
            extern_item.attributes.len(),
            1,
            "extern item should have 1 attribute"
        );
        assert_eq!(extern_item.attributes[0].name.name, "default");

        let husk_ast::ExternItemKind::Mod {
            package,
            binding,
            items: mod_items,
            ..
        } = &extern_item.kind
        else {
            panic!("expected Mod item");
        };

        assert_eq!(package, "validator");
        assert_eq!(binding.name, "validator");
        assert_eq!(mod_items.len(), 1);

        // The function should NOT have any attributes
        let fn_item = &mod_items[0];
        assert!(
            fn_item.attributes.is_empty(),
            "function should have no attributes"
        );
    }

    #[test]
    fn parses_mod_block_with_functions() {
        let src = r#"extern "js" {
            mod nanoid {
                fn nanoid() -> String;
            }
        }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::ExternBlock { items, .. } = &file.items[0].kind {
            assert_eq!(items.len(), 1);
            if let husk_ast::ExternItemKind::Mod {
                package,
                binding,
                items,
                ..
            } = &items[0].kind
            {
                assert_eq!(package, "nanoid");
                assert_eq!(binding.name, "nanoid");
                assert_eq!(items.len(), 1);
                // ModItemKind has only Fn variant (MVP scope)
                let husk_ast::ModItemKind::Fn {
                    name,
                    params,
                    ret_type,
                } = &items[0].kind;
                assert_eq!(name.name, "nanoid");
                assert!(params.is_empty());
                assert!(ret_type.is_some());
            } else {
                panic!("expected Mod item");
            }
        } else {
            panic!("expected ExternBlock");
        }
    }

    #[test]
    fn parses_format_basic() {
        let src = r#"fn main() { let s = format("hello"); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                assert!(
                    matches!(val.kind, ExprKind::Format { .. }),
                    "expected Format expression, got {:?}",
                    val.kind
                );
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_format_with_placeholder() {
        let src = r#"fn main() { let s = format("hello {}", name); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Format { format, args } = &val.kind {
                    assert_eq!(args.len(), 1);
                    // Check that we have a placeholder segment
                    let has_placeholder = format
                        .segments
                        .iter()
                        .any(|s| matches!(s, husk_ast::FormatSegment::Placeholder(_)));
                    assert!(has_placeholder, "expected at least one placeholder");
                } else {
                    panic!("expected Format expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_format_with_multiple_args() {
        let src = r#"fn main() { let s = format("{} + {} = {}", a, b, c); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Format { args, .. } = &val.kind {
                    assert_eq!(args.len(), 3, "expected 3 arguments");
                } else {
                    panic!("expected Format expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_format_with_format_specifiers() {
        let src = r#"fn main() { let s = format("{:x} {:?}", num, val); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Format { format, args } = &val.kind {
                    assert_eq!(args.len(), 2);
                    // Check format specifiers
                    let placeholders: Vec<_> = format
                        .segments
                        .iter()
                        .filter_map(|s| {
                            if let husk_ast::FormatSegment::Placeholder(ph) = s {
                                Some(ph)
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(placeholders.len(), 2);
                    assert_eq!(placeholders[0].spec.ty, Some('x'));
                    assert_eq!(placeholders[1].spec.ty, Some('?'));
                } else {
                    panic!("expected Format expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_loop_statement() {
        let src = r#"fn main() { loop { break; } }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            assert_eq!(body.len(), 1);
            if let StmtKind::Loop { body: loop_body } = &body[0].kind {
                assert_eq!(loop_body.stmts.len(), 1);
                assert!(matches!(loop_body.stmts[0].kind, StmtKind::Break));
            } else {
                panic!("expected Loop statement, got {:?}", body[0].kind);
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_loop_with_continue() {
        let src = r#"fn main() { loop { continue; } }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Loop { body: loop_body } = &body[0].kind {
                assert_eq!(loop_body.stmts.len(), 1);
                assert!(matches!(loop_body.stmts[0].kind, StmtKind::Continue));
            } else {
                panic!("expected Loop statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_loop_with_if_break() {
        let src = r#"fn main() {
            loop {
                if true {
                    break;
                }
            }
        }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            assert_eq!(body.len(), 1);
            if let StmtKind::Loop { body: loop_body } = &body[0].kind {
                assert_eq!(loop_body.stmts.len(), 1);
                assert!(matches!(loop_body.stmts[0].kind, StmtKind::If { .. }));
            } else {
                panic!("expected Loop statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_format_with_named_placeholder() {
        // Rust-style {var_name} syntax - the parser should synthesize an implicit arg
        let src = r#"fn main() { let s = format("{x}"); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Format { format, args } = &val.kind {
                    // Should have synthesized 1 implicit arg for `x`
                    assert_eq!(args.len(), 1, "expected 1 synthesized arg");
                    // Verify the arg is an identifier named "x"
                    if let ExprKind::Ident(ident) = &args[0].kind {
                        assert_eq!(ident.name, "x");
                    } else {
                        panic!("expected Ident expression, got {:?}", args[0].kind);
                    }
                    // Verify the placeholder has a position assigned
                    let placeholders: Vec<_> = format
                        .segments
                        .iter()
                        .filter_map(|s| {
                            if let husk_ast::FormatSegment::Placeholder(ph) = s {
                                Some(ph)
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(placeholders.len(), 1);
                    assert_eq!(placeholders[0].name, Some("x".to_string()));
                    assert_eq!(placeholders[0].position, Some(0));
                } else {
                    panic!("expected Format expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_format_with_multiple_named_placeholders() {
        let src = r#"fn main() { let s = format("{x} + {y} = {z}"); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Format { format, args } = &val.kind {
                    // Should have synthesized 3 implicit args
                    assert_eq!(args.len(), 3, "expected 3 synthesized args");
                    // Verify args are identifiers x, y, z in order
                    let names: Vec<_> = args
                        .iter()
                        .filter_map(|a| {
                            if let ExprKind::Ident(ident) = &a.kind {
                                Some(ident.name.as_str())
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(names, vec!["x", "y", "z"]);
                    // Verify placeholders have positions 0, 1, 2
                    let placeholders: Vec<_> = format
                        .segments
                        .iter()
                        .filter_map(|s| {
                            if let husk_ast::FormatSegment::Placeholder(ph) = s {
                                Some(ph)
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(placeholders[0].position, Some(0));
                    assert_eq!(placeholders[1].position, Some(1));
                    assert_eq!(placeholders[2].position, Some(2));
                } else {
                    panic!("expected Format expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_format_with_repeated_named_placeholder() {
        // Same variable used twice should only generate one arg
        let src = r#"fn main() { let s = format("{x} and {x} again"); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Format { format, args } = &val.kind {
                    // Should have synthesized 1 arg for `x` (not 2)
                    assert_eq!(
                        args.len(),
                        1,
                        "expected 1 synthesized arg for repeated {{x}}"
                    );
                    // Both placeholders should point to position 0
                    let placeholders: Vec<_> = format
                        .segments
                        .iter()
                        .filter_map(|s| {
                            if let husk_ast::FormatSegment::Placeholder(ph) = s {
                                Some(ph)
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(placeholders.len(), 2);
                    assert_eq!(placeholders[0].position, Some(0));
                    assert_eq!(placeholders[1].position, Some(0));
                } else {
                    panic!("expected Format expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_format_with_named_and_format_spec() {
        let src = r#"fn main() { let s = format("{x:08x}"); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Format { format, args } = &val.kind {
                    assert_eq!(args.len(), 1);
                    let placeholders: Vec<_> = format
                        .segments
                        .iter()
                        .filter_map(|s| {
                            if let husk_ast::FormatSegment::Placeholder(ph) = s {
                                Some(ph)
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(placeholders[0].name, Some("x".to_string()));
                    assert_eq!(placeholders[0].position, Some(0));
                    assert_eq!(placeholders[0].spec.width, Some(8));
                    assert!(placeholders[0].spec.zero_pad);
                    assert_eq!(placeholders[0].spec.ty, Some('x'));
                } else {
                    panic!("expected Format expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_println_with_named_placeholder() {
        let src = r#"fn main() { println("{name}"); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            // println statements with semicolons are wrapped in Semi
            let expr = match &body[0].kind {
                husk_ast::StmtKind::Expr(e) => e,
                husk_ast::StmtKind::Semi(e) => e,
                other => panic!("expected Expr or Semi statement, got {:?}", other),
            };
            if let ExprKind::FormatPrint { format, args, .. } = &expr.kind {
                assert_eq!(args.len(), 1);
                if let ExprKind::Ident(ident) = &args[0].kind {
                    assert_eq!(ident.name, "name");
                } else {
                    panic!("expected Ident expression");
                }
                let placeholders: Vec<_> = format
                    .segments
                    .iter()
                    .filter_map(|s| {
                        if let husk_ast::FormatSegment::Placeholder(ph) = s {
                            Some(ph)
                        } else {
                            None
                        }
                    })
                    .collect();
                assert_eq!(placeholders[0].name, Some("name".to_string()));
                assert_eq!(placeholders[0].position, Some(0));
            } else {
                panic!("expected FormatPrint expression");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_format_mixed_implicit_and_named() {
        // Mix of implicit {} and named {var} placeholders
        let src = r#"fn main() { let s = format("{} is {x} and {} is {y}", a, b); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Format { format, args } = &val.kind {
                    // 2 explicit args (a, b) + 2 synthesized (x, y) = 4 total
                    assert_eq!(
                        args.len(),
                        4,
                        "expected 4 args (2 explicit + 2 synthesized)"
                    );
                    // Check synthesized args are at the end
                    if let ExprKind::Ident(ident) = &args[2].kind {
                        assert_eq!(ident.name, "x");
                    }
                    if let ExprKind::Ident(ident) = &args[3].kind {
                        assert_eq!(ident.name, "y");
                    }
                    // Check placeholder positions
                    let placeholders: Vec<_> = format
                        .segments
                        .iter()
                        .filter_map(|s| {
                            if let husk_ast::FormatSegment::Placeholder(ph) = s {
                                Some(ph)
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(placeholders.len(), 4);
                    // First {} -> position None (implicit 0)
                    assert_eq!(placeholders[0].position, None);
                    // {x} -> position 2 (synthesized)
                    assert_eq!(placeholders[1].position, Some(2));
                    // Second {} -> position None (implicit 1)
                    assert_eq!(placeholders[2].position, None);
                    // {y} -> position 3 (synthesized)
                    assert_eq!(placeholders[3].position, Some(3));
                } else {
                    panic!("expected Format expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_format_named_with_extra_explicit_args() {
        // Edge case: more explicit args than implicit {} placeholders.
        // The named placeholder {x} should bind to the synthesized arg at position 2,
        // not accidentally bind to the extra explicit arg `b` at position 1.
        let src = r#"fn main() { let s = format("{} {x}", a, b); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Format { format, args } = &val.kind {
                    // 2 explicit args (a, b) + 1 synthesized (x) = 3 total
                    assert_eq!(
                        args.len(),
                        3,
                        "expected 3 args (2 explicit + 1 synthesized)"
                    );
                    // Verify synthesized arg is at the end and is 'x'
                    if let ExprKind::Ident(ident) = &args[2].kind {
                        assert_eq!(ident.name, "x");
                    } else {
                        panic!("expected Ident 'x' at position 2, got {:?}", args[2].kind);
                    }
                    // Check placeholder positions
                    let placeholders: Vec<_> = format
                        .segments
                        .iter()
                        .filter_map(|s| {
                            if let husk_ast::FormatSegment::Placeholder(ph) = s {
                                Some(ph)
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(placeholders.len(), 2);
                    // First {} -> position None (implicit, will use arg 0 at runtime)
                    assert_eq!(placeholders[0].position, None);
                    // {x} -> position 2 (synthesized, matching args[2])
                    assert_eq!(placeholders[1].position, Some(2));
                    assert_eq!(placeholders[1].name, Some("x".to_string()));
                } else {
                    panic!("expected Format expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_format_only_named_no_implicit() {
        // Only named placeholders, no implicit {} - positions should match args indices
        let src = r#"fn main() { let s = format("{a} {b} {c}"); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Format { format, args } = &val.kind {
                    assert_eq!(args.len(), 3);
                    // Verify args are a, b, c at positions 0, 1, 2
                    let names: Vec<_> = args
                        .iter()
                        .filter_map(|a| {
                            if let ExprKind::Ident(ident) = &a.kind {
                                Some(ident.name.as_str())
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(names, vec!["a", "b", "c"]);
                    // Verify placeholders have positions 0, 1, 2
                    let placeholders: Vec<_> = format
                        .segments
                        .iter()
                        .filter_map(|s| {
                            if let husk_ast::FormatSegment::Placeholder(ph) = s {
                                Some(ph)
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(placeholders[0].position, Some(0));
                    assert_eq!(placeholders[1].position, Some(1));
                    assert_eq!(placeholders[2].position, Some(2));
                } else {
                    panic!("expected Format expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_basic_cast_expression() {
        let src = r#"fn main() { let x = 42 as f64; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Cast { expr, target_ty } = &val.kind {
                    // Check inner expression is 42
                    if let ExprKind::Literal(lit) = &expr.kind {
                        assert!(matches!(lit.kind, husk_ast::LiteralKind::Int(42)));
                    } else {
                        panic!("expected Literal, got {:?}", expr.kind);
                    }
                    // Check target type is f64
                    if let husk_ast::TypeExprKind::Named(ident) = &target_ty.kind {
                        assert_eq!(ident.name, "f64");
                    } else {
                        panic!("expected Named type 'f64', got {:?}", target_ty.kind);
                    }
                } else {
                    panic!("expected Cast expression, got {:?}", val.kind);
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_chained_cast_expression() {
        let src = r#"fn main() { let x = true as i32 as f64; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                // Outer cast: (true as i32) as f64
                if let ExprKind::Cast {
                    expr: outer_expr,
                    target_ty: outer_ty,
                } = &val.kind
                {
                    // Check outer target is f64
                    if let husk_ast::TypeExprKind::Named(ident) = &outer_ty.kind {
                        assert_eq!(ident.name, "f64");
                    } else {
                        panic!("expected outer type 'f64'");
                    }
                    // Inner cast: true as i32
                    if let ExprKind::Cast {
                        expr: inner_expr,
                        target_ty: inner_ty,
                    } = &outer_expr.kind
                    {
                        // Check inner target is i32
                        if let husk_ast::TypeExprKind::Named(ident) = &inner_ty.kind {
                            assert_eq!(ident.name, "i32");
                        } else {
                            panic!("expected inner type 'i32'");
                        }
                        // Check inner expression is true
                        if let ExprKind::Literal(lit) = &inner_expr.kind {
                            assert!(matches!(lit.kind, husk_ast::LiteralKind::Bool(true)));
                        } else {
                            panic!("expected Literal 'true'");
                        }
                    } else {
                        panic!("expected inner Cast expression");
                    }
                } else {
                    panic!("expected Cast expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_cast_with_arithmetic_precedence() {
        // Cast should have higher precedence than addition, so "2 + 3 as f64" should parse as "2 + (3 as f64)"
        let src = r#"fn main() { let x = 2 + 3 as f64; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                // Should be: Binary(Add, 2, Cast(3, f64))
                if let ExprKind::Binary { op, left, right } = &val.kind {
                    assert!(matches!(op, husk_ast::BinaryOp::Add));
                    // Left should be 2
                    if let ExprKind::Literal(lit) = &left.kind {
                        assert!(matches!(lit.kind, husk_ast::LiteralKind::Int(2)));
                    } else {
                        panic!("expected Literal 2 on left, got {:?}", left.kind);
                    }
                    // Right should be Cast(3, f64)
                    if let ExprKind::Cast { expr, target_ty } = &right.kind {
                        if let ExprKind::Literal(lit) = &expr.kind {
                            assert!(matches!(lit.kind, husk_ast::LiteralKind::Int(3)));
                        } else {
                            panic!("expected Literal 3 in cast");
                        }
                        if let husk_ast::TypeExprKind::Named(ident) = &target_ty.kind {
                            assert_eq!(ident.name, "f64");
                        } else {
                            panic!("expected type f64");
                        }
                    } else {
                        panic!("expected Cast on right, got {:?}", right.kind);
                    }
                } else {
                    panic!("expected Binary expression, got {:?}", val.kind);
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_cast_with_comparison_precedence() {
        // Cast should have higher precedence than comparison, so "x < y as i32" should parse as "x < (y as i32)"
        let src = r#"fn main() { let result = x < y as i32; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                // Should be: Binary(Lt, x, Cast(y, i32))
                if let ExprKind::Binary { op, left, right } = &val.kind {
                    assert!(matches!(op, husk_ast::BinaryOp::Lt));
                    // Left should be x
                    if let ExprKind::Ident(ident) = &left.kind {
                        assert_eq!(ident.name, "x");
                    } else {
                        panic!("expected Ident x on left");
                    }
                    // Right should be Cast(y, i32)
                    if let ExprKind::Cast { expr, target_ty } = &right.kind {
                        if let ExprKind::Ident(ident) = &expr.kind {
                            assert_eq!(ident.name, "y");
                        } else {
                            panic!("expected Ident y in cast");
                        }
                        if let husk_ast::TypeExprKind::Named(ident) = &target_ty.kind {
                            assert_eq!(ident.name, "i32");
                        } else {
                            panic!("expected type i32");
                        }
                    } else {
                        panic!("expected Cast on right");
                    }
                } else {
                    panic!("expected Binary expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_cast_with_multiplication_precedence() {
        // Cast should have higher precedence than multiplication, so "2 * 3 as f64" should parse as "2 * (3 as f64)"
        let src = r#"fn main() { let x = 2 * 3 as f64; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                // Should be: Binary(Mul, 2, Cast(3, f64))
                if let ExprKind::Binary { op, left, right } = &val.kind {
                    assert!(matches!(op, husk_ast::BinaryOp::Mul));
                    // Left should be 2
                    if let ExprKind::Literal(lit) = &left.kind {
                        assert!(matches!(lit.kind, husk_ast::LiteralKind::Int(2)));
                    } else {
                        panic!("expected Literal 2 on left");
                    }
                    // Right should be Cast(3, f64)
                    if let ExprKind::Cast { expr, target_ty } = &right.kind {
                        if let ExprKind::Literal(lit) = &expr.kind {
                            assert!(matches!(lit.kind, husk_ast::LiteralKind::Int(3)));
                        } else {
                            panic!("expected Literal 3 in cast");
                        }
                        if let husk_ast::TypeExprKind::Named(ident) = &target_ty.kind {
                            assert_eq!(ident.name, "f64");
                        } else {
                            panic!("expected type f64");
                        }
                    } else {
                        panic!("expected Cast on right");
                    }
                } else {
                    panic!("expected Binary expression");
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_cast_with_unary_precedence() {
        // Unary should have higher precedence than cast, so "-1 as f64" should parse as "(-1) as f64"
        let src = r#"fn main() { let x = -1 as f64; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                // Should be: Cast(Unary(Neg, 1), f64)
                if let ExprKind::Cast { expr, target_ty } = &val.kind {
                    if let ExprKind::Unary { op, expr: inner } = &expr.kind {
                        assert!(matches!(op, husk_ast::UnaryOp::Neg));
                        if let ExprKind::Literal(lit) = &inner.kind {
                            assert!(matches!(lit.kind, husk_ast::LiteralKind::Int(1)));
                        } else {
                            panic!("expected Literal 1 in unary");
                        }
                    } else {
                        panic!("expected Unary expression in cast, got {:?}", expr.kind);
                    }
                    if let husk_ast::TypeExprKind::Named(ident) = &target_ty.kind {
                        assert_eq!(ident.name, "f64");
                    } else {
                        panic!("expected type f64");
                    }
                } else {
                    panic!("expected Cast expression, got {:?}", val.kind);
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_cast_with_call_precedence() {
        // Function calls should have higher precedence than cast, so "foo() as i32" should parse as "(foo()) as i32"
        let src = r#"fn main() { let x = foo() as i32; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                // Should be: Cast(Call(foo, []), i32)
                if let ExprKind::Cast { expr, target_ty } = &val.kind {
                    if let ExprKind::Call {
                        callee,
                        type_args: _,
                        args,
                    } = &expr.kind
                    {
                        if let ExprKind::Ident(ident) = &callee.kind {
                            assert_eq!(ident.name, "foo");
                        } else {
                            panic!("expected Ident foo as callee");
                        }
                        assert!(args.is_empty());
                    } else {
                        panic!("expected Call expression in cast, got {:?}", expr.kind);
                    }
                    if let husk_ast::TypeExprKind::Named(ident) = &target_ty.kind {
                        assert_eq!(ident.name, "i32");
                    } else {
                        panic!("expected type i32");
                    }
                } else {
                    panic!("expected Cast expression, got {:?}", val.kind);
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_cast_with_index_precedence() {
        // Indexing should have higher precedence than cast, so "arr[0] as f64" should parse as "(arr[0]) as f64"
        let src = r#"fn main() { let x = arr[0] as f64; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                // Should be: Cast(Index(arr, 0), f64)
                if let ExprKind::Cast { expr, target_ty } = &val.kind {
                    if let ExprKind::Index { base: arr, index } = &expr.kind {
                        if let ExprKind::Ident(ident) = &arr.kind {
                            assert_eq!(ident.name, "arr");
                        } else {
                            panic!("expected Ident arr");
                        }
                        if let ExprKind::Literal(lit) = &index.kind {
                            assert!(matches!(lit.kind, husk_ast::LiteralKind::Int(0)));
                        } else {
                            panic!("expected Literal 0 as index");
                        }
                    } else {
                        panic!("expected Index expression in cast, got {:?}", expr.kind);
                    }
                    if let husk_ast::TypeExprKind::Named(ident) = &target_ty.kind {
                        assert_eq!(ident.name, "f64");
                    } else {
                        panic!("expected type f64");
                    }
                } else {
                    panic!("expected Cast expression, got {:?}", val.kind);
                }
            } else {
                panic!("expected Let statement with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_enum_tuple_pattern_single_binding() {
        // Option::Some(x) should parse as EnumTuple pattern with one binding
        let src = r#"
fn test(opt: Option<i32>) -> i32 {
    match opt {
        Option::Some(x) => x,
        Option::None => 0,
    }
}
"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Expr(expr) = &body[0].kind {
                if let ExprKind::Match { arms, .. } = &expr.kind {
                    // First arm should be Option::Some(x)
                    if let PatternKind::EnumTuple { path, fields } = &arms[0].pattern.kind {
                        assert_eq!(path.len(), 2);
                        assert_eq!(path[0].name, "Option");
                        assert_eq!(path[1].name, "Some");
                        assert_eq!(fields.len(), 1);
                        if let PatternKind::Binding(id) = &fields[0].kind {
                            assert_eq!(id.name, "x");
                        } else {
                            panic!("expected Binding pattern in tuple field");
                        }
                    } else {
                        panic!("expected EnumTuple pattern, got {:?}", arms[0].pattern.kind);
                    }
                } else {
                    panic!("expected Match expression");
                }
            } else {
                panic!("expected Expr statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_enum_tuple_pattern_result_variants() {
        // Result::Ok(val) and Result::Err(msg) patterns
        let src = r#"
fn test(r: Result<i32, String>) -> i32 {
    match r {
        Result::Ok(val) => val,
        Result::Err(msg) => 0,
    }
}
"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Expr(expr) = &body[0].kind {
                if let ExprKind::Match { arms, .. } = &expr.kind {
                    // First arm should be Result::Ok(val)
                    if let PatternKind::EnumTuple { path, fields } = &arms[0].pattern.kind {
                        assert_eq!(path[1].name, "Ok");
                        assert_eq!(fields.len(), 1);
                        if let PatternKind::Binding(id) = &fields[0].kind {
                            assert_eq!(id.name, "val");
                        } else {
                            panic!("expected Binding pattern");
                        }
                    } else {
                        panic!("expected EnumTuple pattern");
                    }
                    // Second arm should be Result::Err(msg)
                    if let PatternKind::EnumTuple { path, fields } = &arms[1].pattern.kind {
                        assert_eq!(path[1].name, "Err");
                        assert_eq!(fields.len(), 1);
                        if let PatternKind::Binding(id) = &fields[0].kind {
                            assert_eq!(id.name, "msg");
                        } else {
                            panic!("expected Binding pattern");
                        }
                    } else {
                        panic!("expected EnumTuple pattern");
                    }
                } else {
                    panic!("expected Match expression");
                }
            } else {
                panic!("expected Expr statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_break_in_match_arm() {
        // break in a match arm should be wrapped in a block expression
        let src = r#"
fn test(opt: Option<i32>) {
    loop {
        match opt {
            Option::Some(x) => x,
            Option::None => break,
        }
    }
}
"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Loop { body: loop_body } = &body[0].kind {
                if let StmtKind::Expr(match_expr) = &loop_body.stmts[0].kind {
                    if let ExprKind::Match { arms, .. } = &match_expr.kind {
                        // Second arm should have break wrapped in a block
                        if let ExprKind::Block(block) = &arms[1].expr.kind {
                            assert_eq!(block.stmts.len(), 1);
                            assert!(matches!(block.stmts[0].kind, StmtKind::Break));
                        } else {
                            panic!(
                                "expected Block expression for break arm, got {:?}",
                                arms[1].expr.kind
                            );
                        }
                    } else {
                        panic!("expected Match expression");
                    }
                } else {
                    panic!("expected Expr statement in loop");
                }
            } else {
                panic!("expected Loop statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_continue_in_match_arm() {
        // continue in a match arm should be wrapped in a block expression
        let src = r#"
fn test() {
    loop {
        match x {
            _ => continue,
        }
    }
}
"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Loop { body: loop_body } = &body[0].kind {
                if let StmtKind::Expr(match_expr) = &loop_body.stmts[0].kind {
                    if let ExprKind::Match { arms, .. } = &match_expr.kind {
                        if let ExprKind::Block(block) = &arms[0].expr.kind {
                            assert_eq!(block.stmts.len(), 1);
                            assert!(matches!(block.stmts[0].kind, StmtKind::Continue));
                        } else {
                            panic!("expected Block expression for continue arm");
                        }
                    } else {
                        panic!("expected Match expression");
                    }
                } else {
                    panic!("expected Expr statement in loop");
                }
            } else {
                panic!("expected Loop statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_turbofish_method_call() {
        // Method call with turbofish: "123".parse::<i32>()
        let src = r#"fn main() { let n = "123".parse::<i32>(); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::MethodCall {
                    receiver,
                    method,
                    type_args,
                    args,
                } = &val.kind
                {
                    // Receiver should be "123" literal
                    assert!(matches!(
                        &receiver.kind,
                        ExprKind::Literal(husk_ast::Literal {
                            kind: husk_ast::LiteralKind::String(s),
                            ..
                        }) if s == "123"
                    ));
                    // Method should be parse
                    assert_eq!(method.name, "parse");
                    // Should have one type arg: i32
                    assert_eq!(type_args.len(), 1);
                    if let husk_ast::TypeExprKind::Named(ident) = &type_args[0].kind {
                        assert_eq!(ident.name, "i32");
                    } else {
                        panic!("expected Named type i32");
                    }
                    // Should have no args
                    assert!(args.is_empty());
                } else {
                    panic!("expected MethodCall, got {:?}", val.kind);
                }
            } else {
                panic!("expected Let with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_turbofish_with_multiple_type_args() {
        // Method call with multiple turbofish types
        let src = r#"fn main() { let x = foo.bar::<i32, String, f64>(); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let husk_ast::StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::MethodCall { type_args, .. } = &val.kind {
                    assert_eq!(type_args.len(), 3);
                    let names: Vec<_> = type_args
                        .iter()
                        .filter_map(|ty| {
                            if let husk_ast::TypeExprKind::Named(ident) = &ty.kind {
                                Some(ident.name.as_str())
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(names, vec!["i32", "String", "f64"]);
                } else {
                    panic!("expected MethodCall");
                }
            } else {
                panic!("expected Let with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    // =========================================================================
    // Tuple parsing tests
    // =========================================================================

    #[test]
    fn parses_tuple_type() {
        let src = r#"fn main() { let x: (i32, String) = (1, "hello"); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Let { ty: Some(ty), .. } = &body[0].kind {
                if let husk_ast::TypeExprKind::Tuple(types) = &ty.kind {
                    assert_eq!(types.len(), 2);
                } else {
                    panic!("expected Tuple type, got {:?}", ty.kind);
                }
            } else {
                panic!("expected Let with type annotation");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_single_element_tuple_type_with_trailing_comma() {
        // (i32,) should be a single-element tuple, not just i32
        let src = r#"fn main() { let x: (i32,) = (42,); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Let { ty: Some(ty), .. } = &body[0].kind {
                if let husk_ast::TypeExprKind::Tuple(types) = &ty.kind {
                    assert_eq!(types.len(), 1, "single-element tuple should have 1 type");
                } else {
                    panic!("expected Tuple type for (i32,), got {:?}", ty.kind);
                }
            } else {
                panic!("expected Let with type annotation");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_parenthesized_type_as_grouping() {
        // (i32) without trailing comma should be just i32, not a tuple
        let src = r#"fn main() { let x: (i32) = 5; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Let { ty: Some(ty), .. } = &body[0].kind {
                // Should be Named(i32), not Tuple
                if let husk_ast::TypeExprKind::Named(ident) = &ty.kind {
                    assert_eq!(ident.name, "i32");
                } else {
                    panic!("expected Named type i32 for grouping, got {:?}", ty.kind);
                }
            } else {
                panic!("expected Let with type annotation");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_tuple_literal() {
        let src = r#"fn main() { let x = (1, "hello", true); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::Tuple { elements } = &val.kind {
                    assert_eq!(elements.len(), 3);
                } else {
                    panic!("expected Tuple expression, got {:?}", val.kind);
                }
            } else {
                panic!("expected Let with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_tuple_field_access() {
        let src = r#"fn main() { let x = pair.0; let y = pair.1; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                if let ExprKind::TupleField { index, .. } = &val.kind {
                    assert_eq!(*index, 0);
                } else {
                    panic!("expected TupleField expression, got {:?}", val.kind);
                }
            } else {
                panic!("expected Let with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_chained_tuple_field_access() {
        // nested.0.0 should parse as two chained TupleField accesses
        let src = r#"fn main() { let x = nested.0.0; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Let {
                value: Some(val), ..
            } = &body[0].kind
            {
                // Outer should be TupleField with index 0
                if let ExprKind::TupleField {
                    base,
                    index: outer_idx,
                } = &val.kind
                {
                    assert_eq!(*outer_idx, 0);
                    // Inner should also be TupleField with index 0
                    if let ExprKind::TupleField {
                        index: inner_idx, ..
                    } = &base.kind
                    {
                        assert_eq!(*inner_idx, 0);
                    } else {
                        panic!("expected inner TupleField, got {:?}", base.kind);
                    }
                } else {
                    panic!("expected TupleField expression, got {:?}", val.kind);
                }
            } else {
                panic!("expected Let with value");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_tuple_pattern_in_let() {
        let src = r#"fn main() { let (x, y) = pair; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Let { pattern, .. } = &body[0].kind {
                if let husk_ast::PatternKind::Tuple { fields } = &pattern.kind {
                    assert_eq!(fields.len(), 2);
                } else {
                    panic!("expected Tuple pattern, got {:?}", pattern.kind);
                }
            } else {
                panic!("expected Let statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_tuple_pattern_with_wildcard() {
        let src = r#"fn main() { let (x, _) = pair; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Let { pattern, .. } = &body[0].kind {
                if let husk_ast::PatternKind::Tuple { fields } = &pattern.kind {
                    assert_eq!(fields.len(), 2);
                    assert!(matches!(fields[1].kind, husk_ast::PatternKind::Wildcard));
                } else {
                    panic!("expected Tuple pattern, got {:?}", pattern.kind);
                }
            } else {
                panic!("expected Let statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_parenthesized_pattern_as_grouping() {
        // (x) without trailing comma should be just x, not a tuple pattern
        let src = r#"fn main() { let (x) = 5; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Let { pattern, .. } = &body[0].kind {
                // Should be a Binding pattern, not a Tuple
                if let husk_ast::PatternKind::Binding(ident) = &pattern.kind {
                    assert_eq!(ident.name, "x");
                } else {
                    panic!(
                        "expected Binding pattern for grouping, got {:?}",
                        pattern.kind
                    );
                }
            } else {
                panic!("expected Let statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_single_element_tuple_pattern_with_trailing_comma() {
        // (x,) with trailing comma should be a single-element tuple pattern
        let src = r#"fn main() { let (x,) = single; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Let { pattern, .. } = &body[0].kind {
                if let husk_ast::PatternKind::Tuple { fields } = &pattern.kind {
                    assert_eq!(
                        fields.len(),
                        1,
                        "single-element tuple pattern should have 1 field"
                    );
                } else {
                    panic!("expected Tuple pattern for (x,), got {:?}", pattern.kind);
                }
            } else {
                panic!("expected Let statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_if_let_basic() {
        let src = r#"fn main() { if let Some(x) = opt { foo(); } }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::IfLet {
                pattern,
                then_branch,
                else_branch,
                ..
            } = &body[0].kind
            {
                assert!(matches!(
                    pattern.kind,
                    husk_ast::PatternKind::EnumTuple { .. }
                ));
                assert_eq!(then_branch.stmts.len(), 1);
                assert!(else_branch.is_none());
            } else {
                panic!("expected IfLet statement, got {:?}", body[0].kind);
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_if_let_with_else() {
        let src = r#"fn main() { if let Some(x) = opt { foo(); } else { bar(); } }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::IfLet { else_branch, .. } = &body[0].kind {
                assert!(else_branch.is_some(), "expected else branch");
            } else {
                panic!("expected IfLet statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_if_let_else_if_let_chain() {
        let src = r#"fn main() { if let Some(x) = a { } else if let None = b { } }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::IfLet { else_branch, .. } = &body[0].kind {
                // else branch should contain another IfLet
                let else_stmt = else_branch.as_ref().expect("expected else branch");
                assert!(
                    matches!(else_stmt.kind, StmtKind::IfLet { .. }),
                    "expected nested IfLet in else branch"
                );
            } else {
                panic!("expected IfLet statement");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_let_else() {
        let src = r#"fn main() { let Some(x) = opt else { return; }; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Fn { body, .. } = &file.items[0].kind {
            if let StmtKind::Let {
                pattern,
                else_block,
                ..
            } = &body[0].kind
            {
                assert!(matches!(
                    pattern.kind,
                    husk_ast::PatternKind::EnumTuple { .. }
                ));
                assert!(else_block.is_some(), "expected else block in let-else");
            } else {
                panic!("expected Let statement with else_block");
            }
        } else {
            panic!("expected Fn item");
        }
    }

    #[test]
    fn parses_extern_const() {
        let src = r#"extern "js" { const VERSION: String; }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        assert_eq!(file.items.len(), 1);
        if let ItemKind::ExternBlock { items, .. } = &file.items[0].kind {
            assert_eq!(items.len(), 1);
            if let husk_ast::ExternItemKind::Const { name, ty } = &items[0].kind {
                assert_eq!(name.name, "VERSION");
                assert!(matches!(&ty.kind, husk_ast::TypeExprKind::Named(n) if n.name == "String"));
            } else {
                panic!("expected Const item, got {:?}", items[0].kind);
            }
        } else {
            panic!("expected ExternBlock");
        }
    }

    #[test]
    fn parses_extern_const_multiple() {
        let src = r#"extern "js" {
            const API_URL: String;
            const MAX_RETRIES: i32;
            fn fetch_data() -> String;
        }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::ExternBlock { items, .. } = &file.items[0].kind {
            assert_eq!(items.len(), 3);
            assert!(
                matches!(&items[0].kind, husk_ast::ExternItemKind::Const { name, .. } if name.name == "API_URL")
            );
            assert!(
                matches!(&items[1].kind, husk_ast::ExternItemKind::Const { name, .. } if name.name == "MAX_RETRIES")
            );
            assert!(
                matches!(&items[2].kind, husk_ast::ExternItemKind::Fn { name, .. } if name.name == "fetch_data")
            );
        } else {
            panic!("expected ExternBlock");
        }
    }

    #[test]
    fn parses_extern_static_and_const_together() {
        let src = r#"extern "js" {
            static __dirname: String;
            const VERSION: String;
        }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::ExternBlock { items, .. } = &file.items[0].kind {
            assert_eq!(items.len(), 2);
            assert!(
                matches!(&items[0].kind, husk_ast::ExternItemKind::Static { name, .. } if name.name == "__dirname")
            );
            assert!(
                matches!(&items[1].kind, husk_ast::ExternItemKind::Const { name, .. } if name.name == "VERSION")
            );
        } else {
            panic!("expected ExternBlock");
        }
    }

    #[test]
    fn parses_param_with_this_attribute() {
        let src = r#"extern "js" { fn call(#[this] context: JsValue, arg: String); }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::ExternBlock { items, .. } = &file.items[0].kind {
            if let husk_ast::ExternItemKind::Fn { params, .. } = &items[0].kind {
                assert_eq!(params.len(), 2);
                // First param should have #[this] attribute
                assert_eq!(params[0].attributes.len(), 1);
                assert_eq!(params[0].attributes[0].name.name, "this");
                assert_eq!(params[0].name.name, "context");
                // Second param should have no attributes
                assert!(params[1].attributes.is_empty());
                assert_eq!(params[1].name.name, "arg");
            } else {
                panic!("expected Fn item");
            }
        } else {
            panic!("expected ExternBlock");
        }
    }

    #[test]
    fn parses_method_with_this_attribute() {
        let src = r#"
            impl MyClass {
                extern "js" fn call(&self, #[this] context: JsValue) -> String;
            }
        "#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::Impl(impl_block) = &file.items[0].kind {
            if let husk_ast::ImplItemKind::Method(method) = &impl_block.items[0].kind {
                // Method has receiver + one param with #[this]
                assert!(method.receiver.is_some());
                assert_eq!(method.params.len(), 1);
                assert_eq!(method.params[0].attributes.len(), 1);
                assert_eq!(method.params[0].attributes[0].name.name, "this");
            } else {
                panic!("expected Method");
            }
        } else {
            panic!("expected Impl");
        }
    }

    #[test]
    fn parses_extern_fn_with_js_name_attribute() {
        let src = r#"extern "js" {
            #[js_name = "e"]
            fn express() -> Application;
        }"#;
        let result = parse_str(src);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let file = result.file.unwrap();
        if let ItemKind::ExternBlock { items, .. } = &file.items[0].kind {
            assert_eq!(items.len(), 1);
            // Check the function name
            if let husk_ast::ExternItemKind::Fn { name, .. } = &items[0].kind {
                assert_eq!(name.name, "express");
            } else {
                panic!("expected Fn item");
            }
            // Check the #[js_name] attribute
            assert_eq!(items[0].attributes.len(), 1);
            assert_eq!(items[0].attributes[0].name.name, "js_name");
            assert_eq!(items[0].attributes[0].value, Some("e".to_string()));
            // Check js_name() helper method
            assert_eq!(items[0].js_name(), Some("e"));
        } else {
            panic!("expected ExternBlock");
        }
    }
}
