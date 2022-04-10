use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Result};
use crates_index::Version;
use semver::VersionReq;

fn latest_matching_version(crate_name: &str, crate_version_req: &VersionReq) -> Result<Version> {
    let crates_index = crates_index::Index::new_cargo_default()?;
    let crate_ = crates_index
        .crate_(crate_name)
        .ok_or_else(|| anyhow!("failed to find latest crate version"))?;
    let matching_versions = crate_
        .versions()
        .iter()
        .filter(|v| crate_version_req.matches(&v.version().parse().unwrap()));
    let result = matching_versions.last();
    result
        .cloned()
        .ok_or_else(|| anyhow!("no matching crate versions"))
}

fn extract_crate_tarball(crate_version: &Version) -> Result<tempfile::TempDir> {
    let crates_index = crates_index::Index::new_cargo_default()?;
    let index_path = crates_index.path();
    let index_name = index_path
        .file_name()
        .ok_or_else(|| anyhow!("failed to find index path"))?;
    let cache_folder = index_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("cache")
        .join(index_name);
    let cache_filename = format!("{}-{}.crate", crate_version.name(), crate_version.version());
    let cache_path = cache_folder.join(cache_filename);
    let reader: Box<dyn io::Read> = if cache_path.exists() {
        Box::new(fs::File::open(cache_path)?)
    } else {
        let download_url = crate_version
            .download_url(&crates_index.index_config()?)
            .ok_or_else(|| anyhow!("failed to get crate download URL"))?;

        let download_response = reqwest::blocking::get(download_url)?;
        assert!(
            download_response.status().is_success(),
            "failed to download crate"
        );
        Box::new(download_response)
    };
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(reader));
    let crate_extract_path = tempfile::TempDir::new()?;
    archive.unpack(&crate_extract_path)?;
    Ok(crate_extract_path)
}

fn get_subfolder(path: impl AsRef<Path>) -> Result<PathBuf> {
    let subfolder = fs::read_dir(path.as_ref())?
        .next()
        .ok_or_else(|| anyhow!("failed to extract crate"))??
        .path();
    Ok(subfolder)
}

fn replace_text(
    folder: impl AsRef<Path>,
    file: impl AsRef<Path>,
    replace: impl FnOnce(&str) -> String,
) -> Result<()> {
    let full_path = folder.as_ref().join(file);
    let original_text = fs::read_to_string(&full_path)?;
    let final_text = replace(&original_text);
    fs::write(&full_path, final_text)?;
    Ok(())
}

fn replace_syn(
    folder: impl AsRef<Path>,
    file: impl AsRef<Path>,
    replace: impl FnOnce(syn::File) -> syn::File,
) -> Result<()> {
    replace_text(folder, file, |original_text| {
        let original_file = syn::parse_file(original_text).expect("failed to parse file");
        let new_file = replace(original_file);
        prettyplease::unparse(&new_file)
    })
}

fn build_crate_for_web(crate_version: &Version) {
    let name = crate_version.name();
    let version = crate_version.version();
    let crate_extract_path =
        extract_crate_tarball(&crate_version).expect("failed to extract crate");
    let crate_root = get_subfolder(&crate_extract_path).expect("failed to extract crate");
    replace_text(&crate_root, "Cargo.toml", |cargo_toml| {
        cargo_toml.replace(
            "proc-macro = true",
            r#"crate-type = ["cdylib"]
[dependencies.wasm-bindgen]
version = "0.2.80"
[dependencies.prettyplease]
version = "0.1.9"
            "#,
        )
    })
    .expect("couldn't patch Cargo.toml");
    // TODO be smart about this
    let mut functions: HashMap<String, String> = HashMap::new();
    replace_syn(&crate_root, "src/lib.rs", |lib_rs| {
        let syn::File {
            shebang,
            attrs,
            items,
        } = lib_rs;
        syn::File {
            shebang,
            attrs,
            items: items
                .into_iter()
                .filter(|item| {
                    if let syn::Item::ExternCrate(item) = item {
                        item.ident != "proc_macro"
                    } else {
                        true
                    }
                })
                .map(|item| {
                    if let syn::Item::Use(item) = item {
                        syn::Item::Use(syn::ItemUse {
                            tree: if let syn::UseTree::Path(tree) = item.tree {
                                syn::UseTree::Path(syn::UsePath {
                                    ident: if tree.ident == "proc_macro" {
                                        syn::Ident::new(
                                            "proc_macro2",
                                            proc_macro2::Span::call_site(),
                                        )
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
                })
                .map(|item| {
                    if let syn::Item::Fn(item) = item {
                        syn::Item::Fn(syn::ItemFn {
                            block: Box::new(syn::Block {
                                brace_token: item.block.brace_token,
                                stmts: item
                                    .block
                                    .stmts
                                    .into_iter()
                                    .map(|stmt| {
                                        if let syn::Stmt::Local(stmt) = stmt {
                                            syn::Stmt::Local(syn::Local {
                                                init: stmt.init.map(|(eq, expr)| {
                                                    (
                                                        eq,
                                                        if let syn::Expr::Macro(expr) = *expr {
                                                            if expr.mac.path.is_ident("parse_macro_input") {
                                                                assert_eq!(syn::parse2::<syn::ExprCast>(expr.mac.tokens).unwrap(), syn::parse_quote!(input as DeriveInput));
                                                                Box::new(syn::parse_quote! {
                                                                    match syn::parse2::<DeriveInput>(input) {
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
                                    })
                                    .collect(),
                            }),
                            ..item
                        })
                    } else {
                        item
                    }
                })
                .flat_map(|item| {
                    if let syn::Item::Fn(item) = item {
                        let orig_fn_name = item.sig.ident.clone();
                        let mut derive: Option<syn::Ident> = None;
                        let old_attr_count = item.attrs.len();
                        let new_attrs: Vec<_> = item.attrs
                            .into_iter()
                            .filter(|attr| if attr.path.is_ident("proc_macro_derive") {
                                let derive_options = syn::parse2::<syn::ExprTuple>(attr.tokens.clone()).unwrap();
                                let derive_type = derive_options.elems.first().unwrap();
                                if let syn::Expr::Path(path) = derive_type {
                                    derive = path.path.get_ident().cloned();
                                }
                                false
                            } else {
                                true
                            })
                            .collect();
                        let new_attr_count = new_attrs.len();
                        let item = syn::Item::Fn(syn::ItemFn {
                            attrs: new_attrs,
                            ..item
                        });
                        if new_attr_count < old_attr_count {
                            if let Some(derive) = derive {
                                let fn_name = quote::format_ident!("expand_{}", orig_fn_name);
                                functions.insert(format!("#[derive({})]", derive), fn_name.to_string());

                                vec![item, syn::parse_quote! {
                                    #[wasm_bindgen::prelude::wasm_bindgen]
                                    pub fn #fn_name(input: String) -> String {
                                        let output = #orig_fn_name(input.parse().unwrap());
                                        prettyplease::unparse(&syn::parse2(output).unwrap())
                                    }
                                }]
                            } else {
                                vec![item]
                            }
                        } else {
                            vec![item]
                        }
                    } else {
                        vec![item]
                    }
                })
                .collect(),
        }
    })
    .expect("couldn't patch src/lib.rs");
    let cargo_path = env::var_os("CARGO").expect("couldn't run cargo");
    let cargo_build = Command::new(&cargo_path)
        .args(["build", "--target", "wasm32-unknown-unknown"])
        .current_dir(&crate_root)
        .status()
        .expect("couldn't run cargo build");
    assert!(cargo_build.success(), "couldn't run cargo build");
    let cargo_install = Command::new(&cargo_path)
        .args(["install", "wasm-bindgen-cli", "--version", "0.2.80"])
        .current_dir(&crate_root)
        .status()
        .expect("couldn't install wasm-bindgen-cli");
    assert!(cargo_install.success(), "couldn't run cargo install");
    let wasm_path = crate_root
        .join("target/wasm32-unknown-unknown/debug")
        .join(format!("{}.wasm", &name));
    let wasm_bindgen = Command::new("wasm-bindgen")
        .arg(wasm_path)
        .args(["--out-dir", "out", "--out-name"])
        .arg(format!("{}-{}", &name, &version.replace(".", "-")))
        .arg("--no-typescript")
        .status()
        .expect("couldn't wasm-bindgen");
    assert!(wasm_bindgen.success(), "couldn't bindgen the wasm");
    fs::write(
        format!("out/{}-{}.json", &name, &version.replace(".", "-")),
        serde_json::to_string(&functions).expect("couldn't write the metadata"),
    )
    .expect("couldn't write the metadata");
}

fn main() {
    let targets = fs::read_to_string("targets.txt").expect("failed to read targets.txt");
    let targets = targets
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.split_once(" ").expect("failed to parse targets.txt"));
    let mut versions: HashMap<String, String> = HashMap::new();
    for (name, version) in targets {
        let crate_version = latest_matching_version(
            name,
            &VersionReq::parse(version).expect("failed to parse targets.txt"),
        )
        .expect("failed to get latest version of crate");
        versions.insert(
            format!("{} {}", name, version),
            format!("{}-{}", crate_version.name(), crate_version.version().replace(".", "-")),
        );
        build_crate_for_web(&crate_version);
    }
    fs::write(
        "out/targets.json",
        serde_json::to_string(&versions).expect("couldn't write the metadata"),
    )
    .expect("couldn't write the metadata");
}
