//! Experience memory storage.
//!
//! Memory notes form a skill-style topic tree under `~/.egg-agent/memory/`:
//!
//! ```text
//! memory/<scope>/<category>/<title>.md
//! memory/egg-agent/rust/cargo/依赖冲突排查顺序.md
//! memory/global/shell/quoting.md
//! ```
//!
//! The first-level `<scope>` dir is either `global` (project-agnostic
//! lessons) or a project name derived from the current repo root, so
//! retrieval can later load only what is relevant to the workspace at hand.
//! Below it, notes are organized by *topic* (one or two category levels
//! chosen by the summarizer), deliberately not by date — this is a library
//! of reusable know-how, not a journal. Each note is Markdown with a small
//! frontmatter block, written by [`crate::plugin::memory`] after a
//! "struggle then success" trajectory is detected. Nothing here is ever
//! loaded back into the conversation automatically — retrieval comes later;
//! for now the notes are plain files the user can read.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::Config;
use crate::session;

/// Who a note applies to. Parsed from the `scope: …` line the summarizer is
/// asked to emit; defaults to `Project` — most debugging trajectories only
/// make sense in the codebase they came from, and misfiled global notes
/// would leak into every project's future retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project,
}

/// Root memory directory: `~/.egg-agent/memory/`.
pub fn dir() -> Result<PathBuf> {
    let d = Config::dir()?.join("memory");
    std::fs::create_dir_all(&d)
        .with_context(|| format!("could not create memory dir {}", d.display()))?;
    Ok(d)
}

/// Parse the note's scope: the first line starting with `scope:` (the
/// summarizer is told to put it right under the title). Unparseable or
/// missing → `Project` (fail safe: keep the note next to its context).
pub fn scope_of(body: &str) -> Scope {
    for line in body.lines() {
        let line = line.trim();
        let Some(value) = line
            .get(..6)
            .filter(|prefix| prefix.eq_ignore_ascii_case("scope:"))
            .map(|_| line[6..].trim())
        else {
            continue;
        };
        return if value.eq_ignore_ascii_case("global") {
            Scope::Global
        } else {
            Scope::Project
        };
    }
    Scope::Project
}

/// Identify the current project: the nearest ancestor of the working
/// directory that contains a `.git` entry (i.e. the repo root), falling back
/// to the working directory itself. Returns the directory's name.
pub fn project_name() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let mut dir: &Path = &cwd;
    loop {
        if dir.join(".git").exists() {
            return dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string);
        }
        let Some(parent) = dir.parent() else {
            // No repo root found: use the working directory's own name.
            return cwd.file_name().and_then(|n| n.to_str()).map(str::to_string);
        };
        dir = parent;
    }
}

/// Scope dir for the current project: slug of [`project_name`], `misc` if
/// nothing usable is left.
pub fn project_scope() -> String {
    let raw = project_name().unwrap_or_default();
    let slug = slugify(&raw);
    if slug == "note" { "misc".to_string() } else { slug }
}

/// First-level scope directory for a note: `global`, or the current project.
fn scope_dir(body: &str) -> (String, &'static str) {
    match scope_of(body) {
        Scope::Global => ("global".to_string(), "global"),
        Scope::Project => (project_scope(), "project"),
    }
}

/// Parse the note's `category:` line into 1-2 sanitized path segments (the
/// summarizer is told to put it right under the `scope:` line). Categories
/// are lowercase-kebab topics like `rust/cargo` — anything weird is
/// slugified, `..`-style tricks dissolve, and a missing/empty category falls
/// back to `misc`.
pub fn category_of(body: &str) -> Vec<String> {
    for line in body.lines() {
        let line = line.trim();
        let Some(value) = line
            .get(..9)
            .filter(|prefix| prefix.eq_ignore_ascii_case("category:"))
            .map(|_| line[9..].trim())
        else {
            continue;
        };
        let segs: Vec<String> = value
            .split('/')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| slugify(s).to_lowercase())
            // "note" is slugify's empty-input fallback, not a real category.
            .filter(|s| s != "note")
            .take(2)
            .collect();
        return if segs.is_empty() {
            vec!["misc".to_string()]
        } else {
            segs
        };
    }
    vec!["misc".to_string()]
}

/// Existing category paths under a scope dir (`/`-joined, depth ≤ 2, sorted,
/// capped). Fed to the summarizer so the tree converges on shared branches
/// instead of sprouting near-duplicates like `rust-cargo` vs `rust/cargo`.
pub fn list_categories(scope: &str) -> Vec<String> {
    const MAX_LISTED: usize = 40;
    let Ok(root) = dir() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join(scope)) else {
        return out; // scope dir doesn't exist yet — no categories
    };
    let mut first: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    first.sort();
    for d1 in first {
        let Some(n1) = d1.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        out.push(n1.to_string());
        if let Ok(sub) = std::fs::read_dir(&d1) {
            let mut second: Vec<PathBuf> = sub
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            second.sort();
            for d2 in second {
                if let Some(n2) = d2.file_name().and_then(|n| n.to_str()) {
                    out.push(format!("{n1}/{n2}"));
                }
            }
        }
    }
    out.truncate(MAX_LISTED);
    out
}

/// Persist one experience note. Returns the file path.
///
/// `body` is the Markdown produced by the summarizer model; its `# heading`
/// becomes the filename slug, and its `scope:` / `category:` lines pick the
/// directories. Reusing an existing title is fine — a numeric suffix keeps
/// the earlier note instead of clobbering it.
pub fn save(body: &str, tool_calls: u32, failures: u32) -> Result<PathBuf> {
    let now = session::chrono_now(); // creation time goes to frontmatter only
    let (scope_dir, kind) = scope_dir(body);
    let category = category_of(body);
    let cat_dir = category
        .iter()
        .fold(dir()?.join(&scope_dir), |p, seg| p.join(seg));
    std::fs::create_dir_all(&cat_dir)
        .with_context(|| format!("could not create memory dir {}", cat_dir.display()))?;

    let slug = slugify(title_of(body));
    let path = dedup_path(&cat_dir, &slug);

    let note = format!(
        "---\ncreated: {now}\nsource: egg-agent auto-memory\nscope: {kind}:{scope_dir}\ncategory: {}\ntool_calls: {tool_calls}\nfailures: {failures}\n---\n\n{}",
        category.join("/"),
        body.trim()
    );
    std::fs::write(&path, note)
        .with_context(|| format!("could not write memory note to {}", path.display()))?;
    Ok(path)
}

// ---- Retrieval: the read side of the memory system ----
//
// The write side (`save`) files notes under `memory/<scope>/<category>/`.
// These functions let the main model pull relevant notes back into the
// conversation on demand (via the `memory_search` / `skill_tree` tools) —
// nothing is loaded automatically; the model decides when a past lesson is
// worth recalling.

/// One search hit: a note that matched the query, with its relevance score
/// and a short preview.
#[derive(Debug, Clone)]
pub struct NoteMatch {
    pub path: PathBuf,
    pub title: String,
    pub score: u32,
    /// First paragraph of the body, ≤200 chars — enough to judge relevance
    /// without loading the whole note.
    pub snippet: String,
}

/// The scope dirs a search covers: `global` (project-agnostic lessons) plus
/// the current project's own dir. Notes filed under *other* projects are
/// intentionally invisible — they rarely apply to the workspace at hand.
fn search_scopes() -> Vec<String> {
    let mut scopes = vec!["global".to_string()];
    let project = project_scope();
    if project != "global" {
        scopes.push(project);
    }
    scopes
}

/// Keyword search across `global` + current-project notes. Each query token
/// scores by *where* it appears: title ×3, tags ×2, body ×1 (case-insensitive).
/// Returns the top `top_n` notes with a positive score, best first.
///
/// This is the v1 retrieval described in the design doc — pure string
/// matching, zero dependencies. Embedding-based recall is a later stage.
pub fn search(query: &str, top_n: usize) -> Vec<NoteMatch> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return Vec::new();
    }

    let Ok(root) = dir() else {
        return Vec::new();
    };

    let mut hits: Vec<NoteMatch> = Vec::new();
    for scope in search_scopes() {
        for path in walk_notes(&root.join(&scope)) {
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            let title = title_of(&body).to_string();
            let title_lc = title.to_lowercase();
            let tags_lc = tags_line(&body).to_lowercase();
            let body_lc = body.to_lowercase();

            let mut score = 0u32;
            for tok in &tokens {
                if title_lc.contains(tok.as_str()) {
                    score += 3;
                }
                if tags_lc.contains(tok.as_str()) {
                    score += 2;
                }
                if body_lc.contains(tok.as_str()) {
                    score += 1;
                }
            }
            if score > 0 {
                hits.push(NoteMatch {
                    path,
                    title,
                    score,
                    snippet: first_paragraph(&body, 200),
                });
            }
        }
    }

    // Highest score first; break ties by title so results are deterministic.
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
    hits.truncate(top_n);
    hits
}

/// Read one note's full text (frontmatter + body). Used by both tools to
/// deliver a note once the model has decided it's relevant.
pub fn read_note(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("could not read memory note {}", path.display()))
}

/// Render the memory tree as an indented listing. `path` restricts it to a
/// subtree (e.g. `global/aliyun` or `global`); `None` lists everything under
/// `memory/`. Files show as `… <title>.md`; the model can then open one with
/// `read_note`.
pub fn list_tree(path: Option<&str>) -> Result<String> {
    let root = dir()?;
    // Sanitize any caller-supplied subpath to stay inside `memory/`: drop
    // empty / `.` / `..` segments so `../../etc` can't escape.
    let base = match path {
        Some(p) => {
            let mut b = root.clone();
            for seg in p.split(['/', '\\']) {
                let seg = seg.trim();
                if seg.is_empty() || seg == "." || seg == ".." {
                    continue;
                }
                b = b.join(seg);
            }
            b
        }
        None => root.clone(),
    };

    if !base.exists() {
        return Ok("(no matching memory notes)".to_string());
    }
    // A file path resolves to its contents directly.
    if base.is_file() {
        return read_note(&base);
    }

    let mut out = String::new();
    render_tree(&base, 0, &mut out);
    if out.trim().is_empty() {
        Ok("(no memory notes yet)".to_string())
    } else {
        Ok(out)
    }
}

/// Recursively collect every `.md` file under `dir` (any depth).
fn walk_notes(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            out.extend(walk_notes(&p));
        } else if p.extension().map(|e| e == "md").unwrap_or(false) {
            out.push(p);
        }
    }
    out
}

/// Append an indented tree listing of `dir` to `out` (dirs first, then files).
fn render_tree(dir: &Path, depth: usize, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for p in entries.flatten().map(|e| e.path()) {
        if p.is_dir() {
            dirs.push(p);
        } else if p.extension().map(|e| e == "md").unwrap_or(false) {
            files.push(p);
        }
    }
    dirs.sort();
    files.sort();
    let indent = "  ".repeat(depth);
    for d in dirs {
        if let Some(name) = d.file_name().and_then(|n| n.to_str()) {
            out.push_str(&format!("{indent}{name}/\n"));
            render_tree(&d, depth + 1, out);
        }
    }
    for f in files {
        let title = std::fs::read_to_string(&f)
            .ok()
            .map(|b| title_of(&b).to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_default();
        let rel = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Show a path the model can hand straight back to skill_tree/read.
        let display = display_path(&f);
        if title.is_empty() {
            out.push_str(&format!("{indent}{rel}  → {display}\n"));
        } else {
            out.push_str(&format!("{indent}{rel}  [{title}]  → {display}\n"));
        }
    }
}

/// A note path relative to the `memory/` root (forward slashes), so listings
/// echo back a value the tools accept as `path`.
fn display_path(file: &Path) -> String {
    let root = dir().ok();
    let rel = root
        .as_ref()
        .and_then(|r| file.strip_prefix(r).ok())
        .unwrap_or(file);
    rel.components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// The content of the note's `## 标签` (tags) line, joined into one string for
/// scoring. Empty if there is no tags section.
fn tags_line(body: &str) -> String {
    let mut lines = body.lines();
    while let Some(line) = lines.next() {
        if line.trim_start_matches('#').trim() == "标签" {
            // Tags may be on the same heading line or the next non-empty line.
            for next in lines.by_ref() {
                let t = next.trim();
                if t.is_empty() {
                    continue;
                }
                if t.starts_with('#') {
                    break; // ran into the next section
                }
                return t.to_string();
            }
            break;
        }
    }
    String::new()
}

/// First non-empty, non-frontmatter, non-heading paragraph of the body,
/// truncated to `cap` chars — a compact preview for search results.
fn first_paragraph(body: &str, cap: usize) -> String {
    let mut in_frontmatter = false;
    for (i, line) in body.lines().enumerate() {
        let trimmed = line.trim();
        // Skip a leading `--- … ---` frontmatter block.
        if i == 0 && trimmed == "---" {
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter {
            if trimmed == "---" {
                in_frontmatter = false;
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let preview: String = trimmed.chars().take(cap).collect();
        return preview;
    }
    String::new()
}

/// `<dir>/<slug>.md`, or `<slug>-2.md`, `-3.md`, … when already taken.
fn dedup_path(dir: &Path, slug: &str) -> PathBuf {
    let candidate = dir.join(format!("{slug}.md"));
    if !candidate.exists() {
        return candidate;
    }
    for n in 2u32.. {
        let candidate = dir.join(format!("{slug}-{n}.md"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// Extract the first `# heading` line, without the `#` prefix.
pub fn title_of(body: &str) -> &str {
    for line in body.lines() {
        let line = line.trim();
        if let Some(title) = line.strip_prefix("# ") {
            return title.trim();
        }
    }
    ""
}

/// Turn a title into a filesystem-safe slug: keep alphanumerics and CJK
/// characters, collapse everything else to `-`, cap at 24 chars.
fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut count = 0;
    for ch in title.chars() {
        if count >= 24 {
            break;
        }
        if ch.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            out.push(ch);
            count += 1;
        } else if (ch.is_whitespace() || ch == '-' || ch == '_') && !out.ends_with('-') && !out.is_empty()
        {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "note".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_parses_line_and_defaults_to_project() {
        assert_eq!(scope_of("# t\nscope: global\n\n## 任务"), Scope::Global);
        assert_eq!(scope_of("# t\nScope: Global\n"), Scope::Global);
        assert_eq!(scope_of("# t\nscope: project\n"), Scope::Project);
        assert_eq!(scope_of("# t\nscope: 项目\n"), Scope::Project);
        // Missing or unrecognized → Project (fail safe).
        assert_eq!(scope_of("# t\n\n## 任务"), Scope::Project);
        assert_eq!(scope_of("# t\nscope: maybe\n"), Scope::Project);
        // Only the first `scope:` line counts.
        assert_eq!(
            scope_of("# t\nscope: global\nscope: project\n"),
            Scope::Global
        );
    }

    #[test]
    fn scope_dir_routes_global_and_project() {
        assert_eq!(scope_dir("# t\nscope: global\n").0, "global");
        let (dir, kind) = scope_dir("# t\nscope: project\n");
        assert_eq!(kind, "project");
        assert_ne!(dir, "global");
        assert!(!dir.is_empty());
    }

    #[test]
    fn category_parses_sanitizes_and_defaults_to_misc() {
        assert_eq!(category_of("# t\ncategory: rust/cargo\n"), ["rust", "cargo"]);
        assert_eq!(category_of("# t\ncategory: Git\n"), ["git"]);
        assert_eq!(category_of("# t\ncategory: Shell Quoting \n"), ["shell-quoting"]);
        // Missing or empty → misc.
        assert_eq!(category_of("# t\n\n## 任务"), ["misc"]);
        assert_eq!(category_of("# t\ncategory: / \n"), ["misc"]);
        // Traversal tricks dissolve; depth capped at 2.
        assert_eq!(category_of("# t\ncategory: ../../etc\n"), ["etc"]);
        assert_eq!(category_of("# t\ncategory: a/b/c\n"), ["a", "b"]);
    }

    #[test]
    fn dedup_path_suffixes_existing_titles() {
        let dir = std::env::temp_dir().join(format!("egg-mem-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p1 = dedup_path(&dir, "some-title");
        assert_eq!(p1.file_name().unwrap(), "some-title.md");
        std::fs::write(&p1, "x").unwrap();
        let p2 = dedup_path(&dir, "some-title");
        assert_eq!(p2.file_name().unwrap(), "some-title-2.md");
        std::fs::write(&p2, "x").unwrap();
        let p3 = dedup_path(&dir, "some-title");
        assert_eq!(p3.file_name().unwrap(), "some-title-3.md");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn title_extracts_first_heading() {
        assert_eq!(title_of("# cargo 依赖冲突排查\n\n## 任务"), "cargo 依赖冲突排查");
        assert_eq!(title_of("no heading here"), "");
        assert_eq!(title_of("text\n\n# later heading\n## x"), "later heading");
    }

    #[test]
    fn slug_keeps_cjk_and_collapses_separators() {
        assert_eq!(slugify("cargo 依赖冲突时的排查顺序"), "cargo-依赖冲突时的排查顺序");
        assert_eq!(slugify("Fix: ratatui 0.30 / crossterm!"), "Fix-ratatui-030-crossterm");
        assert_eq!(slugify(""), "note");
        assert_eq!(
            slugify("a very long english title that keeps going and going and going"),
            "a-very-long-english-title-tha"
        );
    }

    #[test]
    fn tags_line_reads_the_标签_section() {
        let body = "# t\nscope: global\n\n## 任务背景\n干活\n\n## 标签\nrust, cargo, serde\n";
        assert_eq!(tags_line(body), "rust, cargo, serde");
        // No tags section → empty.
        assert_eq!(tags_line("# t\n\n## 任务背景\n干活"), "");
        // Tags section present but empty before next heading → empty.
        assert_eq!(tags_line("# t\n\n## 标签\n\n## 下一节\nx"), "");
    }

    #[test]
    fn first_paragraph_skips_frontmatter_and_headings() {
        let note = "---\ncreated: x\nscope: global\n---\n\n# 标题\n\n这是正文第一段。\n\n## 小节\n内容";
        assert_eq!(first_paragraph(note, 200), "这是正文第一段。");
        // Truncation on char boundary.
        assert_eq!(first_paragraph("正文很长很长很长", 3), "正文很");
        // Nothing but headings → empty.
        assert_eq!(first_paragraph("# a\n## b", 200), "");
    }

    #[test]
    fn walk_and_render_tree_over_temp_dir() {
        let base = std::env::temp_dir().join(format!("egg-mem-walk-{}", std::process::id()));
        let cat = base.join("global").join("aliyun");
        std::fs::create_dir_all(&cat).unwrap();
        std::fs::write(cat.join("sls-tag.md"), "# SLS tag 搜索坑\n正文").unwrap();
        std::fs::write(cat.join("readme.txt"), "not a note").unwrap();

        let notes = walk_notes(&base);
        assert_eq!(notes.len(), 1, "only .md files count");
        assert!(notes[0].ends_with("sls-tag.md"));

        let mut out = String::new();
        render_tree(&base, 0, &mut out);
        assert!(out.contains("global/"));
        assert!(out.contains("aliyun/"));
        assert!(out.contains("sls-tag.md"));
        assert!(out.contains("[SLS tag 搜索坑]"), "shows the title: {out}");
        assert!(!out.contains("readme.txt"), "non-md excluded");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn scoring_ranks_title_over_tags_over_body() {
        // Exercise the scoring arithmetic directly (search() itself needs a
        // real HOME dir, tested via the tools' integration tests).
        let score = |title: &str, tags: &str, body: &str, tok: &str| {
            let mut s = 0u32;
            if title.to_lowercase().contains(tok) {
                s += 3;
            }
            if tags.to_lowercase().contains(tok) {
                s += 2;
            }
            if body.to_lowercase().contains(tok) {
                s += 1;
            }
            s
        };
        assert_eq!(score("SLS", "", "", "sls"), 3);
        assert_eq!(score("", "sls, log", "", "sls"), 2);
        assert_eq!(score("", "", "about sls", "sls"), 1);
        assert_eq!(score("SLS", "sls", "sls here", "sls"), 6);
    }
}
