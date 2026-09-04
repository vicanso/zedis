use std::collections::BTreeSet;
use vergen::{Build, Emitter};
use vergen_git2::Git2;

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

/// Walk string leaves as `(dotted.key, value)`.
fn collect_strings(table: &toml::Table, prefix: &str, out: &mut Vec<(String, String)>) {
    for (k, v) in table {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::Table(t) => collect_strings(t, &path, out),
            toml::Value::String(s) => out.push((path, s.clone())),
            _ => {}
        }
    }
}

/// Forbidden controls in UI copy:
/// - C1 (U+0080–U+009F), including NEL (U+0085) — typical UTF-8 decoding residue
/// - other C0 controls except TAB / LF (multi-line help text uses `\n`)
/// - DEL (U+007F)
fn has_forbidden_control(s: &str) -> Option<char> {
    s.chars().find(|c| {
        let o = *c as u32;
        match o {
            0x09 | 0x0a => false, // TAB, LF
            0x00..=0x1f | 0x7f..=0x9f => true,
            _ => false,
        }
    })
}

/// Detect classic "UTF-8 bytes read as Latin-1, then saved as UTF-8 again"
/// mojibake (e.g. `Ã©` for `é`, `ç¹` for `点`).
///
/// If every char is ≤ U+00FF and reinterpreting those bytes as UTF-8 yields a
/// *different*, well-formed string, the value is almost certainly double-
/// encoded. Correct accented text (`café`, `Schlüssel`) fails this test
/// because a lone 0xE9 / 0xFC is not valid UTF-8.
fn mojibake_fix(s: &str) -> Option<String> {
    if s.is_ascii() || s.is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        let o = c as u32;
        if o > 0xff {
            return None;
        }
        bytes.push(o as u8);
    }
    let Ok(fixed) = std::str::from_utf8(&bytes) else {
        return None;
    };
    if fixed == s || fixed.is_empty() {
        return None;
    }
    // Ignore "fixes" that still look control-laden (not useful UI copy).
    if has_forbidden_control(fixed).is_some() {
        return None;
    }
    Some(fixed.to_string())
}

/// Encoding / mojibake checks for one locale file's string leaves.
fn check_string_encoding(path: &std::path::Path, table: &toml::Table) -> bool {
    let mut strings = Vec::new();
    collect_strings(table, "", &mut strings);
    let mut ok = true;
    for (key, value) in &strings {
        if let Some(c) = has_forbidden_control(value) {
            ok = false;
            eprintln!(
                "\n[locale check] {} key `{key}` contains forbidden control U+{:04X}",
                path.display(),
                c as u32
            );
            eprintln!("  value: {value:?}");
        }
        if let Some(fixed) = mojibake_fix(value) {
            ok = false;
            eprintln!(
                "\n[locale check] {} key `{key}` looks double-encoded (UTF-8 as Latin-1)",
                path.display()
            );
            eprintln!("  found   : {value:?}");
            eprintln!("  expected : {fixed:?}");
        }
    }
    ok
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
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }

        // Reuse the already-parsed en table; read the rest from disk.
        let table = if stem == "en" {
            en_table.clone()
        } else {
            let src = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("failed to read {}", path.display()));
            toml::from_str(&src).unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
        };

        // Encoding / mojibake — every locale including en.
        if !check_string_encoding(&path, &table) {
            failed = true;
        }

        // Key parity only against en (skip en vs itself).
        if stem == "en" {
            continue;
        }

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
        panic!("locale files failed validation (key parity and/or encoding) — see errors above");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Re-run this build script whenever any locale file changes
    println!("cargo:rerun-if-changed=locales/");
    check_locales();
    let build = Build::all_build();
    let git2 = Git2::all_git();

    // The channel is baked into the binary (`startup::BUILD_CHANNEL`); a
    // changed env must rebuild it.
    println!("cargo:rerun-if-env-changed=ZEDIS_BUILD_CHANNEL");
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
