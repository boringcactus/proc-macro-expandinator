use syn::{
    parse_quote, Block, Expr, ExprCast, FnArg, Ident, Item, ItemFn, ItemUse, Local, Pat, PatType,
    ReturnType, Signature, Stmt, Type, UseGroup, UsePath, UseTree,
};

pub fn is_not_extern_crate_proc_macro(item: &Item) -> bool {
    if let Item::ExternCrate(item) = item {
        item.ident != "proc_macro"
    } else {
        true
    }
}

pub fn rewrite_use_proc_macro_to_use_proc_macro2(item: Item) -> Item {
    if let Item::Use(item) = item {
        Item::Use(ItemUse {
            tree: if let UseTree::Path(tree) = item.tree {
                UseTree::Path(UsePath {
                    ident: if tree.ident == "proc_macro" {
                        Ident::new("proc_macro2", proc_macro2::Span::call_site())
                    } else {
                        tree.ident
                    },
                    ..tree
                })
            } else {
                item.tree
            },
            ..item
        })
    } else {
        item
    }
}

fn rewrite_parse_macro_input_call(stmt: Stmt) -> Stmt {
    if let Stmt::Local(stmt) = stmt {
        Stmt::Local(Local {
            init: stmt.init.map(|(eq, expr)| {
                (
                    eq,
                    if let Expr::Macro(expr) = *expr {
                        if expr.mac.path.is_ident("parse_macro_input") || expr.mac.path == parse_quote!(syn::parse_macro_input) {
                            let (input, r#type) = if let Ok(cast) =
                                syn::parse2::<ExprCast>(expr.mac.tokens.clone())
                            {
                                (cast.expr, cast.ty)
                            } else {
                                if let Pat::Type(pat) = &stmt.pat {
                                    (syn::parse2(expr.mac.tokens).unwrap(), pat.ty.clone())
                                } else {
                                    panic!("weird parse_macro_input! call (neither `as` nor explicit output type) at {:?}", stmt.let_token.span);
                                }
                            };
                            Box::new(parse_quote! {
                                match syn::parse2::<#r#type>(#input) {
                                    Ok(syntax_tree) => syntax_tree,
                                    Err(err) => return err.to_compile_error(),
                                }
                            })
                        } else {
                            Box::new(Expr::Macro(expr))
                        }
                    } else {
                        expr
                    },
                )
            }),
            ..stmt
        })
    } else {
        stmt
    }
}

pub fn rewrite_parse_macro_input_calls(item: Item) -> Item {
    if let Item::Fn(item) = item {
        Item::Fn(ItemFn {
            block: Box::new(Block {
                brace_token: item.block.brace_token,
                stmts: item
                    .block
                    .stmts
                    .into_iter()
                    .map(rewrite_parse_macro_input_call)
                    .collect(),
            }),
            ..item
        })
    } else {
        item
    }
}

pub fn fix_proc_macro_error(item: Item) -> Item {
    if let Item::Fn(item) = item {
        Item::Fn(ItemFn {
            attrs: item
                .attrs
                .into_iter()
                .map(|attr| {
                    if attr.path.is_ident("proc_macro_error") {
                        parse_quote!(#[proc_macro_error(allow_not_macro)])
                    } else if attr.path == parse_quote!(proc_macro_error::proc_macro_error) {
                        parse_quote!(#[proc_macro_error::proc_macro_error(allow_not_macro)])
                    } else {
                        attr
                    }
                })
                .collect(),
            ..item
        })
    } else {
        item
    }
}

fn remove_parse_macro_input(tree: UseTree) -> Option<UseTree> {
    Some(match tree {
        UseTree::Name(ref name) => {
            if name.ident == "parse_macro_input" {
                return None;
            } else {
                tree
            }
        }
        UseTree::Group(group) => UseTree::Group(UseGroup {
            items: group
                .items
                .into_iter()
                .filter_map(remove_parse_macro_input)
                .collect(),
            ..group
        }),
        UseTree::Path(path) => UseTree::Path(path),
        tree => todo!("handle syn:: tree {:?}", tree),
    })
}

pub fn no_use_syn_parse_macro_input(item: Item) -> Option<Item> {
    Some(if let Item::Use(item) = item {
        Item::Use(ItemUse {
            tree: if let UseTree::Path(tree) = item.tree {
                if tree.ident == "syn" {
                    UseTree::Path(UsePath {
                        tree: Box::new(remove_parse_macro_input(*tree.tree)?),
                        ..tree
                    })
                } else {
                    UseTree::Path(tree)
                }
            } else {
                item.tree
            },
            ..item
        })
    } else {
        item
    })
}

fn rewrite_type(r#type: Type) -> Type {
    if r#type == parse_quote!(proc_macro::TokenStream) {
        parse_quote!(proc_macro2::TokenStream)
    } else {
        r#type
    }
}

fn rewrite_sig_types(sig: Signature) -> Signature {
    Signature {
        inputs: sig
            .inputs
            .into_iter()
            .map(|arg| {
                if let FnArg::Typed(arg) = arg {
                    FnArg::Typed(PatType {
                        ty: Box::new(rewrite_type(*arg.ty)),
                        ..arg
                    })
                } else {
                    arg
                }
            })
            .collect(),
        output: if let ReturnType::Type(arrow, r#type) = sig.output {
            ReturnType::Type(arrow, Box::new(rewrite_type(*r#type)))
        } else {
            sig.output
        },
        ..sig
    }
}

pub fn rewrite_proc_macro_fn_types_to_proc_macro2(item: Item) -> Item {
    if let Item::Fn(item) = item {
        Item::Fn(ItemFn {
            sig: rewrite_sig_types(item.sig),
            ..item
        })
    } else {
        item
    }
}
