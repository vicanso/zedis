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

//! A dialog's OK handler that opens another dialog must not answer `true`.
//!
//! `ZedisDialog` closes the top of the dialog stack when `on_ok` answers
//! `true`, and a dialog opened inside the handler *is* the top by then: the
//! question it was going to ask is closed before it is drawn, and whatever
//! it guarded never runs. From the outside the button does nothing.
//!
//! It has happened three times, each found by hand and long after it was
//! written: the pops and trims (`open_key_op_form`), Kill by filter in the
//! clients page and Set Stream ID — the last two while a button label was
//! being checked. The shape is always the same two lines, so this test
//! looks for them: `.open(window, cx);` answered by `true`. The handler
//! closes its own dialog first (`window.close_dialog(cx)`), opens the next
//! one, and answers `false`.

use std::fs;
use std::path::{Path, PathBuf};

const SOURCES: &str = "src";

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The lines where a dialog is opened and the handler then answers `true`.
fn chained_closes(source: &str) -> Vec<usize> {
    let lines: Vec<&str> = source.lines().collect();
    lines
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[0].trim_end().ends_with(".open(window, cx);") && pair[1].trim() == "true")
        .map(|(index, _)| index + 1)
        .collect()
}

#[test]
fn a_handler_that_opens_a_dialog_does_not_close_it() {
    let mut files = Vec::new();
    rust_files(Path::new(SOURCES), &mut files);
    assert!(!files.is_empty(), "no sources under {SOURCES}");
    files.sort();

    let mut found = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file).expect("readable source");
        for line in chained_closes(&source) {
            found.push(format!("{}:{line}", file.display()));
        }
    }
    assert!(
        found.is_empty(),
        "an OK handler opens a dialog and then answers `true`, which closes the dialog it just opened:\n  {}\n\
         close this handler's own dialog first (`window.close_dialog(cx)`), open the next one, and answer `false`",
        found.join("\n  ")
    );
}

#[test]
fn the_shape_is_recognised() {
    let broken = "    .on_ok(move |_, window, cx| {\n        ZedisDialog::new_alert(title, body)\n            .open(window, cx);\n        true\n    })\n";
    assert_eq!(chained_closes(broken), [3]);
    let fixed = broken.replace("        true\n", "        false\n");
    assert!(chained_closes(&fixed).is_empty());
    // A dialog opened as the last thing a function does is not a handler's answer.
    assert!(chained_closes("        .open(window, cx);\n    }\n").is_empty());
}
