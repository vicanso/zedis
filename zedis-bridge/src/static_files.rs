// Copyright 2026 Tree xie.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Serving the web build from the bridge itself.
//!
//! Same origin is the point. With the page and the API on one origin there is
//! no CORS to configure, and the login cookie's `SameSite=Strict` keeps
//! working — a cookie that had to travel cross-site could not be `Strict`, and
//! the protection it gives against another site driving the API would be gone.
//!
//! The page is compiled into the binary ([`WebBuild`]), so a deployment is
//! one file. `--static <dir>` serves a directory instead — a rebuilt bundle
//! without recompiling the bridge — and only then does the bridge touch the
//! filesystem for a request.

use flate2::read::GzDecoder;
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// The web build, compiled in from `zedis-web/www`.
///
/// A release build carries the page. A debug build reads the directory from
/// disk on every request instead (rust-embed's behaviour without
/// `debug-embed`), which is what `make web-serve` wants: `make web-bundle`
/// then refreshes the page with no rebuild of the bridge. `build.rs` refuses
/// a release build whose `www/wasm` is missing, because rust-embed would
/// happily embed the directory without it.
///
/// Raw, not compressed: the compressing variant inflates the 20 MB module on
/// every `get`, while a raw embed serves it from the binary's own read-only
/// pages with no copy at all (see the manifest for the feature-unification
/// caveat).
#[derive(RustEmbed)]
#[folder = "../zedis-web/www"]
// wasm-pack's typings and package manifest are for an npm consumer, which
// the page is not; the `.gitignore` in the wasm directory is ours.
#[exclude = "wasm/*.d.ts"]
#[exclude = "wasm/package.json"]
#[exclude = "wasm/.gitignore"]
// The module is stored compressed and only compressed (`.gz` always, `.br`
// from a release bundle): every browser accepts gzip, so the raw 20 MB is
// never what is sent, and leaving it out makes the binary smaller than the
// module it serves. A caller that accepts neither gets it inflated on the
// way out ([`negotiate`]).
#[exclude = "wasm/*.wasm"]
pub struct WebBuild;

/// How the body that answers a request is stored.
#[derive(Debug, PartialEq, Eq)]
pub enum Representation {
    /// The file under `key`, sent as it is and labelled `encoding`.
    Stored {
        key: String,
        encoding: Option<&'static str>,
    },
    /// Only the gzip under `key` exists and the caller accepts no coding that
    /// is stored: inflate it on the way out. The rare path — `curl` without
    /// `--compressed` — and the price of not storing the module raw.
    Inflated { key: String },
}

/// Whether an `Accept-Encoding` header admits `coding`. A token with `q=0`
/// is a refusal; an absent header admits nothing here, because serving the
/// identity is always possible and never wrong.
pub fn accepts(accept_encoding: &str, coding: &str) -> bool {
    accept_encoding.split(',').any(|part| {
        let mut pieces = part.split(';');
        let name = pieces.next().unwrap_or_default().trim();
        let refused = pieces.any(|p| matches!(p.trim().strip_prefix("q="), Some(q) if q.parse::<f32>() == Ok(0.0)));
        name.eq_ignore_ascii_case(coding) && !refused
    })
}

/// Which stored file answers a request for `key`, given what the caller
/// accepts and what exists (`has`): brotli, then gzip, then the file itself,
/// then the gzip inflated. `None` when nothing by that name is stored.
pub fn negotiate(key: &str, accept_encoding: &str, has: impl Fn(&str) -> bool) -> Option<Representation> {
    let (br, gz) = (format!("{key}.br"), format!("{key}.gz"));
    if accepts(accept_encoding, "br") && has(&br) {
        return Some(Representation::Stored {
            key: br,
            encoding: Some("br"),
        });
    }
    if accepts(accept_encoding, "gzip") && has(&gz) {
        return Some(Representation::Stored {
            key: gz,
            encoding: Some("gzip"),
        });
    }
    if has(key) {
        return Some(Representation::Stored {
            key: key.to_string(),
            encoding: None,
        });
    }
    has(&gz).then_some(Representation::Inflated { key: gz })
}

/// The gzip in `bytes`, inflated.
pub fn inflate(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    GzDecoder::new(bytes).read_to_end(&mut out)?;
    Ok(out)
}

/// A strong validator for `bytes` as they will be sent. With
/// `Cache-Control: no-cache` the browser revalidates on every load, and this
/// is what lets the answer be `304` and no body instead of the whole module
/// again — which, without a validator, is what every reload used to cost.
pub fn etag(hash: [u8; 32]) -> String {
    let hex: String = hash[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

/// [`etag`] for bytes that carry no precomputed hash (a file off disk, or an
/// inflated body).
pub fn etag_of(bytes: &[u8]) -> String {
    etag(Sha256::digest(bytes).into())
}

/// Whether an `If-None-Match` header names `etag`.
pub fn matches_etag(if_none_match: Option<&str>, etag: &str) -> bool {
    if_none_match.is_some_and(|header| {
        header
            .split(',')
            .map(str::trim)
            .any(|candidate| candidate == "*" || candidate.trim_start_matches("W/") == etag)
    })
}

/// The relative key rust-embed (and the negotiation above) names a request
/// path by, or `None` if the path could climb out.
pub fn request_key(request_path: &str) -> Option<String> {
    safe_relative(request_path).map(|relative| embed_key(&relative))
}

/// Whether the embedded build holds `key`. By name, not by `get`: in a debug
/// build `get` reads the file, and this is asked up to three times a request.
pub fn embedded_has(key: &str) -> bool {
    WebBuild::iter().any(|name| name == key)
}

/// rust-embed keys a file by its `/`-separated relative path whatever the
/// host's separator is; a `PathBuf` on Windows would say `\`.
fn embed_key(relative: &Path) -> String {
    relative
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// The `Content-Type` for a file the browser is about to use.
///
/// `.wasm` is the one that must be right rather than merely plausible:
/// `WebAssembly.instantiateStreaming` refuses anything but `application/wasm`
/// and the page then fails to start with a message about the MIME type.
pub fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or_default() {
        "wasm" => "application/wasm",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/vnd.microsoft.icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The request path as a relative path under the web root, or `None` if it
/// could escape.
///
/// Rejects rather than sanitises. A `..` that is stripped out silently turns
/// one caller's mistake into a different file being served, while a refusal is
/// a 404 the caller can see. Absolute paths, `..`, Windows prefixes and NUL
/// are all refused; the canonical check in [`resolve`] is the second line,
/// because a symlink inside the root can still point outside it.
pub fn safe_relative(request_path: &str) -> Option<PathBuf> {
    let trimmed = request_path.trim_start_matches('/');
    let trimmed = if trimmed.is_empty() { "index.html" } else { trimmed };
    if trimmed.contains('\0') {
        return None;
    }
    let candidate = Path::new(trimmed);
    let mut safe = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            // Harmless, and dropping it changes nothing about which file is named.
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!safe.as_os_str().is_empty()).then_some(safe)
}

/// The file `request_path` names inside `root`, if it is one.
///
/// Both sides are canonicalised before they are compared, so a symlink that
/// leaves the root is refused even though its literal path looked contained.
pub fn resolve(root: &Path, request_path: &str) -> Option<PathBuf> {
    let relative = safe_relative(request_path)?;
    let root = root.canonicalize().ok()?;
    let full = root.join(relative).canonicalize().ok()?;
    full.starts_with(&root).then_some(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_is_the_index() {
        assert_eq!(safe_relative("/"), Some(PathBuf::from("index.html")));
        assert_eq!(safe_relative(""), Some(PathBuf::from("index.html")));
    }

    #[test]
    fn an_ordinary_asset_passes_through() {
        assert_eq!(safe_relative("/zedis_bg.wasm"), Some(PathBuf::from("zedis_bg.wasm")));
        assert_eq!(
            safe_relative("/assets/icons/search.svg"),
            Some(PathBuf::from("assets/icons/search.svg"))
        );
        assert_eq!(safe_relative("/./app.js"), Some(PathBuf::from("app.js")));
    }

    #[test]
    fn nothing_may_climb_out_of_the_root() {
        for attempt in [
            "/../etc/passwd",
            "/assets/../../etc/passwd",
            "..",
            "/a/b/../../../c",
            "//../x",
        ] {
            assert_eq!(safe_relative(attempt), None, "{attempt:?} must be refused");
        }
    }

    #[test]
    fn an_absolute_path_is_not_a_request_path() {
        // After the leading slashes come off, anything still rooted is a
        // caller trying something, not a relative asset.
        assert_eq!(safe_relative("/\0evil"), None);
    }

    #[test]
    fn wasm_gets_the_type_the_browser_insists_on() {
        assert_eq!(content_type(Path::new("a/zedis_bg.wasm")), "application/wasm");
        assert_eq!(content_type(Path::new("app.js")), "text/javascript; charset=utf-8");
        assert_eq!(content_type(Path::new("index.html")), "text/html; charset=utf-8");
        assert_eq!(content_type(Path::new("f.woff2")), "font/woff2");
        assert_eq!(content_type(Path::new("noextension")), "application/octet-stream");
    }

    #[test]
    fn the_embedded_build_serves_the_page_and_nothing_beside_it() {
        let key = request_key("/").expect("the root is the index");
        assert_eq!(key, "index.html");
        assert_eq!(content_type(Path::new(&key)), "text/html; charset=utf-8");
        let index = WebBuild::get(&key).expect("index.html is checked in");
        assert!(
            std::str::from_utf8(&index.data)
                .expect("html")
                .contains("<title>Zedis</title>")
        );
        assert!(embedded_has("fonts/OFL.txt"), "the font's licence ships with the font");
        assert_eq!(request_key("/../Cargo.toml"), None, "nothing outside www");
        assert!(!embedded_has("missing.html"));
        assert!(
            !embedded_has("wasm/package.json"),
            "excluded, whether or not wasm-pack has written it"
        );
        assert!(
            !embedded_has("wasm/zedis_web_bg.wasm"),
            "the module is stored compressed only, never raw"
        );
        assert_eq!(embed_key(Path::new("assets/icons/x.svg")), "assets/icons/x.svg");
    }

    #[test]
    fn the_best_stored_coding_the_caller_accepts_is_chosen() {
        let stored = ["wasm/app.wasm.br", "wasm/app.wasm.gz", "app.js", "app.js.gz"];
        let has = |key: &str| stored.contains(&key);
        let pick = |key: &str, accept: &str| negotiate(key, accept, has);
        let stored_as = |key: &str, encoding| {
            Some(Representation::Stored {
                key: key.to_string(),
                encoding,
            })
        };

        // What Chrome sends, on localhost and over https alike.
        assert_eq!(
            pick("wasm/app.wasm", "gzip, deflate, br, zstd"),
            stored_as("wasm/app.wasm.br", Some("br"))
        );
        // Plain http to a remote host: no br on offer.
        assert_eq!(
            pick("wasm/app.wasm", "gzip, deflate"),
            stored_as("wasm/app.wasm.gz", Some("gzip"))
        );
        assert_eq!(
            pick("wasm/app.wasm", "br;q=0, gzip"),
            stored_as("wasm/app.wasm.gz", Some("gzip")),
            "q=0 is a refusal"
        );
        // The module is never stored raw, so a caller that accepts nothing gets it inflated.
        assert_eq!(
            pick("wasm/app.wasm", ""),
            Some(Representation::Inflated {
                key: "wasm/app.wasm.gz".to_string()
            })
        );
        // A file that is stored raw is sent raw to such a caller, and compressed to the rest.
        assert_eq!(pick("app.js", ""), stored_as("app.js", None));
        assert_eq!(pick("app.js", "gzip"), stored_as("app.js.gz", Some("gzip")));
        assert_eq!(pick("app.js", "br"), stored_as("app.js", None), "no .br stored for it");
        assert_eq!(pick("missing.html", "gzip, br"), None);
    }

    #[test]
    fn an_inflated_body_is_the_original_and_an_etag_names_exactly_its_bytes() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;
        let original = b"\0asm the module, more or less".repeat(50);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&original).expect("gzip");
        let gz = encoder.finish().expect("gzip");
        assert_eq!(inflate(&gz).expect("inflate"), original);

        let tag = etag_of(&original);
        assert!(tag.starts_with('"') && tag.ends_with('"') && tag.len() == 34, "{tag}");
        assert_ne!(tag, etag_of(&gz), "each coding is its own representation");
        assert!(matches_etag(Some(&tag), &tag));
        assert!(
            matches_etag(Some(&format!("\"other\", W/{tag}")), &tag),
            "a list, and a weak match"
        );
        assert!(matches_etag(Some("*"), &tag));
        assert!(!matches_etag(Some("\"stale\""), &tag));
        assert!(!matches_etag(None, &tag));
    }

    #[test]
    fn resolve_refuses_what_is_not_under_the_root() {
        let dir = std::env::temp_dir().join(format!("zedis-static-{}", std::process::id()));
        let web = dir.join("web");
        std::fs::create_dir_all(&web).expect("mkdir");
        std::fs::write(web.join("index.html"), b"hi").expect("write");
        std::fs::write(dir.join("secret.txt"), b"no").expect("write");

        assert!(resolve(&web, "/index.html").is_some());
        assert!(resolve(&web, "/").is_some(), "the root serves the index");
        assert!(resolve(&web, "/../secret.txt").is_none(), "must not climb out");
        assert!(
            resolve(&web, "/missing.html").is_none(),
            "a missing file is not resolved"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
