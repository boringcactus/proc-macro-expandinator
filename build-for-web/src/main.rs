use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Result};
use crates_index::Version;
use regex::Regex;
use semver::VersionReq;

mod rewrite;

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
    let crate_extract_path = extract_crate_tarball(crate_version).expect("failed to extract crate");
    let crate_root = get_subfolder(&crate_extract_path).expect("failed to extract crate");
    replace_text(&crate_root, "Cargo.toml", |cargo_toml| {
        lazy_static::lazy_static! {
            static ref PROC_MACRO_TRUE: Regex = Regex::new("proc[_-]macro = true").unwrap();
            static ref PROC_MACRO_ERROR_1: Regex = Regex::new(r#"\[dependencies.proc-macro-error\]\nversion = "1(\.\d\.\d)?""#).unwrap();
        }
        let cargo_toml = PROC_MACRO_TRUE.replace_all(cargo_toml, r#"
crate-type = ["cdylib"]
[dependencies.wasm-bindgen]
version = "0.2.80"
[dependencies.prettyplease]
version = "0.1.9"
            "#.trim(),
        );
        let cargo_toml = PROC_MACRO_ERROR_1.replace_all(&cargo_toml, r#"
[dependencies.proc-macro-error]
git = "https://github.com/boringcactus/proc-macro2-error"
            "#.trim(),
        );
        cargo_toml.to_string()
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
                .filter(rewrite::is_not_extern_crate_proc_macro)
                .map(rewrite::rewrite_use_proc_macro_to_use_proc_macro2)
                .map(rewrite::rewrite_parse_macro_input_calls)
                .filter_map(rewrite::no_use_syn_parse_macro_input)
                .map(rewrite::fix_proc_macro_error)
                .map(rewrite::rewrite_proc_macro_fn_types_to_proc_macro2)
                .flat_map(|item| {
                    if let syn::Item::Fn(item) = item {
                        let orig_fn_name = item.sig.ident.clone();
                        let mut derive: Option<syn::Ident> = None;
                        let old_attr_count = item.attrs.len();
                        let new_attrs: Vec<_> = item
                            .attrs
                            .into_iter()
                            .filter(|attr| {
                                if attr.path.is_ident("proc_macro_derive") {
                                    let derive_options =
                                        syn::parse2::<syn::ExprTuple>(attr.tokens.clone()).unwrap();
                                    let derive_type = derive_options.elems.first().unwrap();
                                    if let syn::Expr::Path(path) = derive_type {
                                        derive = path.path.get_ident().cloned();
                                    }
                                    false
                                } else {
                                    true
                                }
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
                                functions
                                    .insert(format!("#[derive({})]", derive), fn_name.to_string());

                                vec![
                                    item,
                                    syn::parse_quote! {
                                        #[wasm_bindgen::prelude::wasm_bindgen]
                                        pub fn #fn_name(input: String) -> String {
                                            let output = #orig_fn_name(input.parse().unwrap());
                                            prettyplease::unparse(&syn::parse2(output).unwrap())
                                        }
                                    },
                                ]
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
        .args([
            "build",
            "--quiet",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .current_dir(&crate_root)
        .status()
        .expect("couldn't run cargo build");
    assert!(cargo_build.success(), "couldn't run cargo build");
    let wasm_path = crate_root
        .join("target/wasm32-unknown-unknown/release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    let wasm_bindgen = Command::new("wasm-bindgen")
        .arg(wasm_path)
        .args(["--out-dir", "out", "--out-name"])
        .arg(format!("{}-{}", &name, &version.replace('.', "-")))
        .args(["--target", "web"])
        .status()
        .expect("couldn't wasm-bindgen");
    assert!(wasm_bindgen.success(), "couldn't bindgen the wasm");
    fs::write(
        format!("out/{}-{}.json", &name, &version.replace('.', "-")),
        serde_json::to_string(&functions).expect("couldn't write the metadata"),
    )
    .expect("couldn't write the metadata");
}

fn main() {
    let cargo_path = env::var_os("CARGO").expect("couldn't run cargo");
    let cargo_install = Command::new(&cargo_path)
        .args(["install", "wasm-bindgen-cli", "--version", "0.2.80"])
        .status()
        .expect("couldn't install wasm-bindgen-cli");
    assert!(cargo_install.success(), "couldn't run cargo install");
    let targets = fs::read_to_string("targets.txt").expect("failed to read targets.txt");
    let targets = targets
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split_once(' ').expect("failed to parse targets.txt"));
    let mut targets_lines = vec![];
    for (name, version) in targets {
        let crate_version = latest_matching_version(
            name,
            &VersionReq::parse(version).expect("failed to parse targets.txt"),
        )
        .expect("failed to get latest version of crate");
        println!(
            "Building {} {} ({})...",
            name,
            version,
            crate_version.version()
        );
        targets_lines.push(
            format!(
                r#""{0} {1}": {{ lib: () => import("./{0}-{2}.js"), data: () => import("./{0}-{2}.json") }}"#,
                name, version,
                crate_version.version().replace('.', "-")
            )
        );
        build_crate_for_web(&crate_version);
    }
    let targets_file = format!(
        r#"export default {{
    {}
}} as Record<string, {{ lib: () => Promise<any>; data: () => Promise<any> }}>;
"#,
        targets_lines.join(",\n")
    );
    fs::write("out/targets.ts", targets_file).expect("couldn't write the metadata");
}
