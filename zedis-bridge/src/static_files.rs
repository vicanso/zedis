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
//! Off unless `--static <dir>` names a directory, so a bridge that only serves
//! the API exposes no filesystem at all.

use std::path::{Component, Path, PathBuf};

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
