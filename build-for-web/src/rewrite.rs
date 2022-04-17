use syn::punctuated::Pair;

pub fn is_not_extern_crate_proc_macro(item: &syn::Item) -> bool {
    if let syn::Item::ExternCrate(item) = item {
        item.ident != "proc_macro"
    } else {
        true
    }
}

pub fn rewrite_use_proc_macro_to_use_proc_macro2(item: syn::Item) -> syn::Item {
    if let syn::Item::Use(item) = item {
        syn::Item::Use(syn::ItemUse {
            tree: if let syn::UseTree::Path(tree) = item.tree {
                syn::UseTree::Path(syn::UsePath {
                    ident: if tree.ident == "proc_macro" {
                        syn::Ident::new("proc_macro2", proc_macro2::Span::call_site())
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

fn rewrite_parse_macro_input_call(stmt: syn::Stmt) -> syn::Stmt {
    if let syn::Stmt::Local(stmt) = stmt {
        syn::Stmt::Local(syn::Local {
            init: stmt.init.map(|(eq, expr)| {
                (
                    eq,
                    if let syn::Expr::Macro(expr) = *expr {
                        if expr.mac.path.is_ident("parse_macro_input") {
                            let expected_type = if let Ok(cast) =
                                syn::parse2::<syn::ExprCast>(expr.mac.tokens.clone())
                            {
                                cast.ty
                            } else {
                                assert_eq!(
                                    syn::parse2::<syn::Ident>(expr.mac.tokens)
                                        .unwrap()
                                        .to_string(),
                                    "input"
                                );
                                if let syn::Pat::Type(pat) = &stmt.pat {
                                    pat.ty.clone()
                                } else {
                                    panic!("weird parse_macro_input! call (neither `as` nor explicit output type) at {:?}", stmt.let_token.span);
                                }
                            };
                            Box::new(syn::parse_quote! {
                                match syn::parse2::<#expected_type>(input) {
                                    Ok(syntax_tree) => syntax_tree,
                                    Err(err) => return err.to_compile_error(),
                                }
                            })
                        } else {
                            Box::new(syn::Expr::Macro(expr))
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

pub fn rewrite_parse_macro_input_calls(item: syn::Item) -> syn::Item {
    if let syn::Item::Fn(item) = item {
        syn::Item::Fn(syn::ItemFn {
            block: Box::new(syn::Block {
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

pub fn fix_proc_macro_error(item: syn::Item) -> syn::Item {
    if let syn::Item::Fn(item) = item {
        syn::Item::Fn(syn::ItemFn {
            attrs: item
                .attrs
                .into_iter()
                .map(|attr| {
                    if attr.path.is_ident("proc_macro_error") {
                        syn::parse_quote!(#[proc_macro_error(allow_not_macro)])
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

fn remove_parse_macro_input(tree: syn::UseTree) -> Option<syn::UseTree> {
    Some(match tree {
        syn::UseTree::Name(ref name) => {
            if name.ident == "parse_macro_input" {
                return None;
            } else {
                tree
            }
        }
        syn::UseTree::Group(group) => syn::UseTree::Group(syn::UseGroup {
            items: group
                .items
                .into_pairs()
                .filter_map(|pair| {
                    Some(match pair {
                        Pair::Punctuated(tree, comma) => {
                            Pair::Punctuated(remove_parse_macro_input(tree)?, comma)
                        }
                        Pair::End(tree) => Pair::End(remove_parse_macro_input(tree)?),
                    })
                })
                .collect(),
            ..group
        }),
        tree => todo!("handle syn:: tree {:?}", tree),
    })
}

pub fn no_use_syn_parse_macro_input(item: syn::Item) -> Option<syn::Item> {
    Some(if let syn::Item::Use(item) = item {
        syn::Item::Use(syn::ItemUse {
            tree: if let syn::UseTree::Path(tree) = item.tree {
                if tree.ident == "syn" {
                    syn::UseTree::Path(syn::UsePath {
                        tree: Box::new(remove_parse_macro_input(*tree.tree)?),
                        ..tree
                    })
                } else {
                    syn::UseTree::Path(tree)
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
