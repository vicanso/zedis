use std::collections::BTreeSet;
use vergen::{BuildBuilder, Emitter};
use vergen_git2::Git2Builder;

/// Recursively collect all dotted key paths from a TOML table.
/// e.g. `[common]\nsubmit = "..."` → `"common.submit"`
fn collect_keys(table: &toml::Table, prefix: &str, keys: &mut BTreeSet<String>) {
    for (k, v) in table {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::Table(t) => collect_keys(t, &path, keys),
            _ => {
                keys.insert(path);
            }
        }
    }
}

fn check_locales() {
    let locales_dir = std::path::Path::new("locales");

    let en_src = std::fs::read_to_string(locales_dir.join("en.toml")).expect("locales/en.toml not found");
    let en_table: toml::Table = toml::from_str(&en_src).expect("failed to parse locales/en.toml");
    let mut en_keys = BTreeSet::new();
    collect_keys(&en_table, "", &mut en_keys);

    let entries = std::fs::read_dir(locales_dir).expect("locales/ directory not found");
    let mut failed = false;

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem == "en" || path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }

        let src = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("failed to read {}", path.display()));
        let table: toml::Table =
            toml::from_str(&src).unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));
        let mut keys = BTreeSet::new();
        collect_keys(&table, "", &mut keys);

        let missing: Vec<_> = en_keys.difference(&keys).collect();
        let extra: Vec<_> = keys.difference(&en_keys).collect();

        if !missing.is_empty() || !extra.is_empty() {
            failed = true;
            eprintln!("\n[locale check] {} is out of sync with en.toml:", path.display());
            for k in &missing {
                eprintln!("  missing : {k}");
            }
            for k in &extra {
                eprintln!("  extra   : {k}");
            }
        }
    }

    if failed {
        panic!("locale files are out of sync with en.toml — see errors above");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Re-run this build script whenever any locale file changes
    println!("cargo:rerun-if-changed=locales/");
    check_locales();
    let build = BuildBuilder::all_build()?;
    let git2 = Git2Builder::all_git()?;

    Emitter::default()
        .add_instructions(&build)?
        .add_instructions(&git2)?
        .emit()?;

    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() == Some("windows") {
        let mut res = winres::WindowsResource::new();

        res.set_icon("icons/zedis.ico");

        if let Err(e) = res.compile() {
            eprintln!("Failed to compile Windows resources: {}", e);
            std::process::exit(1);
        }
    }
    Ok(())
}
