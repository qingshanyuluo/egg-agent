//! Fuzzy file completion for the `@`-mention popup (feature D).
//!
//! [`walk_files`] does a cheap recursive walk of a root directory, skipping the
//! usual heavy / vcs dirs (`.git`, `target`, `node_modules`, …), and returns
//! repo-relative paths. [`rank`] fuzzy-matches a query against those paths with
//! [`nucleo_matcher`] (the matcher codex uses) and returns the best `limit`.
//!
//! The walk is synchronous and pure; `main.rs` runs it on a Tokio blocking task
//! and delivers the ranked result over the existing `CtlEvent` channel, so the
//! UI thread never blocks on filesystem I/O even in a large tree.

use std::path::Path;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

/// Directory names never descended into during the walk.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".venv", "dist", "build"];

/// Hard cap on files collected, so a pathologically large tree can't stall the
/// walk or balloon memory. Ranking still happens over whatever was collected.
const MAX_FILES: usize = 20_000;

/// Recursively collect repo-relative file paths under `root`, skipping
/// [`SKIP_DIRS`] and hidden dot-entries. Returns forward-slash paths for stable
/// matching and display regardless of platform.
pub fn walk_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk_inner(root, root, &mut out);
    out.sort();
    out
}

fn walk_inner(root: &Path, dir: &Path, out: &mut Vec<String>) {
    if out.len() >= MAX_FILES {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Skip hidden entries and the heavy/vcs dirs.
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            walk_inner(root, &path, out);
        } else if file_type.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
            if out.len() >= MAX_FILES {
                return;
            }
        }
    }
}

/// Fuzzy-rank `paths` against `query`, returning up to `limit` best matches,
/// highest score first. An empty query returns the first `limit` paths as-is
/// (the popup shows something the moment `@` is typed).
pub fn rank(paths: &[String], query: &str, limit: usize) -> Vec<String> {
    if query.is_empty() {
        return paths.iter().take(limit).cloned().collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(u32, &String)> = paths
        .iter()
        .filter_map(|p| {
            let mut buf = Vec::new();
            let haystack = nucleo_matcher::Utf32Str::new(p, &mut buf);
            pattern.score(haystack, &mut matcher).map(|s| (s, p))
        })
        .collect();
    // Sort by descending score, then by path for a stable tie-break.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().take(limit).map(|(_, p)| p.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<String> {
        vec![
            "src/main.rs".to_string(),
            "src/app.rs".to_string(),
            "src/ui/cell.rs".to_string(),
            "src/ui/mod.rs".to_string(),
            "README.md".to_string(),
            "Cargo.toml".to_string(),
        ]
    }

    #[test]
    fn ranks_expected_path_first() {
        let files = sample();
        let top = rank(&files, "cell", 5);
        assert_eq!(top.first().map(String::as_str), Some("src/ui/cell.rs"));
    }

    #[test]
    fn path_segments_match_fuzzily() {
        let files = sample();
        // "uicell" should still find src/ui/cell.rs across the slash.
        let top = rank(&files, "uicell", 5);
        assert!(top.iter().any(|p| p == "src/ui/cell.rs"));
    }

    #[test]
    fn empty_query_returns_prefix() {
        let files = sample();
        let top = rank(&files, "", 3);
        assert_eq!(top.len(), 3);
        assert_eq!(top[0], files[0]);
    }

    #[test]
    fn limit_is_respected() {
        let files = sample();
        assert_eq!(rank(&files, "s", 2).len(), 2);
    }

    #[test]
    fn walk_finds_this_source_file_and_skips_target() {
        // Walk the crate root (cwd during tests) and confirm we see a src file
        // but never descend into target/.
        let files = walk_files(Path::new("."));
        assert!(files.iter().any(|p| p == "src/file_search.rs"));
        assert!(!files.iter().any(|p| p.starts_with("target/")));
        assert!(!files.iter().any(|p| p.starts_with(".git/")));
    }
}
