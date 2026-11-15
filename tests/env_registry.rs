//! Completeness guard for the `MACP_*` environment-variable registry
//! (`crates/macp-auth/src/env.rs`, re-exported as `macp_runtime::env`).
//!
//! Scans every `.rs` file in the repository (both cargo workspaces, so
//! `integration_tests/` too) and enforces:
//!
//! 1. Non-test code holds no bare `"MACP_*"` string literal: it reads a
//!    variable through its registry constant (`env::MACP_BIND_ADDR`), so an
//!    unregistered name either fails to compile or fails here.
//! 2. Each non-test row's `read_in` is exactly the set of non-test files that
//!    use its constant, and no test-only row is used by non-test code.
//! 3. Every `"MACP_*"` literal in test code is registered, and each test-only
//!    row's `read_in` is exactly the set of test files that name it.
//! 4. The number of non-test rows equals the number of distinct `MACP_*`
//!    names non-test code reads (the kb-facts acceptance criterion).
//!
//! Test code is: any file under a `tests/` or `benches/` directory or in
//! `integration_tests/`, any `*_tests.rs` file, and everything from a file's
//! first `#[cfg(test)]` `mod` onward. `//` comment lines are ignored.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use macp_runtime::env::REGISTRY;

/// The registry itself: its doc text mentions `env::MACP_*` constants.
const REGISTRY_FILE: &str = "crates/macp-auth/src/env.rs";

/// `"MACP_*"` literals in test code that are not variable names.
const NOT_VARIABLES: &[&str] = &[
    // tests/http_binary.rs: a substring searched for in a startup error.
    "MACP_HTTP",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

fn is_test_file(rel: &str) -> bool {
    rel.starts_with("integration_tests/")
        || rel.split('/').any(|c| c == "tests" || c == "benches")
        || rel.ends_with("_tests.rs")
}

/// (non-test text, test text) of one file, comment lines dropped.
fn split_regions(rel: &str, text: &str) -> (String, String) {
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    if is_test_file(rel) {
        return (String::new(), strip(text));
    }
    let cut = text
        .match_indices("#[cfg(test)]\n")
        .map(|(i, m)| i + m.len())
        .find(|&after| text[after..].trim_start().starts_with("mod "))
        .map(|after| after - "#[cfg(test)]\n".len())
        .unwrap_or(text.len());
    (strip(&text[..cut]), strip(&text[cut..]))
}

/// Every `MACP_[A-Z0-9_]+` name that directly follows `prefix` and is
/// followed by `suffix`.
fn names_after(text: &str, prefix: &str, suffix: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, _) in text.match_indices(prefix) {
        let rest = &text[i + prefix.len()..];
        if !rest.starts_with("MACP_") {
            continue;
        }
        let len = rest
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(rest.len());
        if rest[len..].starts_with(suffix) {
            out.push(rest[..len].to_string());
        }
    }
    out
}

struct Scan {
    /// Non-test files with a bare `"MACP_*"` literal, and the literal.
    stray_literals: Vec<(String, String)>,
    /// Name -> non-test files using `env::NAME`.
    non_test_uses: BTreeMap<String, BTreeSet<String>>,
    /// Name -> test files naming `"NAME"`.
    test_literals: BTreeMap<String, BTreeSet<String>>,
}

fn scan() -> Scan {
    let root = repo_root();
    let mut files = Vec::new();
    for top in ["src", "crates", "tests", "benches", "integration_tests"] {
        rust_files(&root.join(top), &mut files);
    }
    files.push(root.join("build.rs"));
    files.sort();
    assert!(files.len() > 50, "the scan found too few files: {files:?}");

    let mut scan = Scan {
        stray_literals: Vec::new(),
        non_test_uses: BTreeMap::new(),
        test_literals: BTreeMap::new(),
    };
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == REGISTRY_FILE || rel == "tests/env_registry.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let (prod, test) = split_regions(&rel, &text);
        for name in names_after(&prod, "\"", "\"") {
            scan.stray_literals.push((rel.clone(), name));
        }
        for name in names_after(&prod, "env::", "") {
            scan.non_test_uses
                .entry(name)
                .or_default()
                .insert(rel.clone());
        }
        for name in names_after(&test, "\"", "\"") {
            if !NOT_VARIABLES.contains(&name.as_str()) {
                scan.test_literals
                    .entry(name)
                    .or_default()
                    .insert(rel.clone());
            }
        }
    }
    scan
}

#[test]
fn non_test_code_reads_env_only_through_the_registry() {
    let scan = scan();
    assert!(
        scan.stray_literals.is_empty(),
        "non-test code names these MACP_* variables as bare string literals; \
         register each in crates/macp-auth/src/env.rs and read it through its \
         constant (`env::NAME`): {:?}",
        scan.stray_literals
    );
}

#[test]
fn read_in_matches_the_files_that_read_each_variable() {
    let scan = scan();
    let mut problems = Vec::new();
    for row in REGISTRY {
        let declared: BTreeSet<String> = row.read_in.iter().map(|s| s.to_string()).collect();
        let used = scan
            .non_test_uses
            .get(row.name)
            .cloned()
            .unwrap_or_default();
        let actual = if row.test_only {
            if !used.is_empty() {
                problems.push(format!(
                    "{} is test-only but non-test code reads it in {used:?}",
                    row.name
                ));
            }
            scan.test_literals
                .get(row.name)
                .cloned()
                .unwrap_or_default()
        } else {
            used
        };
        if declared != actual {
            problems.push(format!(
                "{}: read_in is {declared:?} but the files are {actual:?}",
                row.name
            ));
        }
    }
    for name in scan.non_test_uses.keys() {
        if macp_runtime::env::lookup(name).is_none() {
            problems.push(format!("env::{name} is used but not registered"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_macp_literal_in_test_code_is_registered() {
    let scan = scan();
    let unregistered: Vec<_> = scan
        .test_literals
        .iter()
        .filter(|(name, _)| macp_runtime::env::lookup(name).is_none())
        .collect();
    assert!(
        unregistered.is_empty(),
        "test code names MACP_* variables the registry does not know; add each \
         to crates/macp-auth/src/env.rs (component: Test if only tests read it): \
         {unregistered:?}"
    );
}

#[test]
fn non_test_row_count_equals_distinct_non_test_names() {
    let scan = scan();
    let rows = REGISTRY.iter().filter(|r| !r.test_only).count();
    assert_eq!(rows, scan.non_test_uses.len());
}
