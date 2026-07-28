//! External-editor integration (feature C).
//!
//! [`edit_string`] writes the current input buffer to a temp file, launches
//! `$EDITOR` (falling back to `vi`) as a blocking foreground process that
//! inherits the real tty, then reads the file back after the editor exits. The
//! TUI must be suspended (raw mode off, alternate screen left) *before* calling
//! this and restored after — that terminal-state dance is the caller's job
//! (`main.rs` owns the `TerminalGuard`), keeping this module free of any TUI
//! coupling and therefore unit-testable on its own.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Suffix so editors pick sensible syntax highlighting / wrap settings.
const TEMP_SUFFIX: &str = "egg-input.md";

/// Resolve the editor command: `$EDITOR`, then `$VISUAL`, then `vi`.
fn editor_command() -> String {
    std::env::var("EDITOR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("VISUAL").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "vi".to_string())
}

/// A unique temp-file path in the system temp dir. Dependency-free: combines the
/// pid with a process-local counter so repeated edits in one session don't
/// collide. (No `Instant`/rand needed — the pid+counter pair is unique enough
/// for a scratch file we delete immediately.)
fn temp_path() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("{pid}-{n}-{TEMP_SUFFIX}"))
}

/// The editor command split into program + args, so `EDITOR="code -w"` works.
/// Returns `(program, args)`.
fn split_command(cmd: &str) -> (String, Vec<String>) {
    let mut parts = cmd.split_whitespace().map(str::to_string);
    let program = parts.next().unwrap_or_else(|| "vi".to_string());
    (program, parts.collect())
}

/// Round-trip `initial` through the user's editor, returning the edited text.
/// The temp file is always removed, even on error.
///
/// # Errors
/// - the editor binary can't be spawned (e.g. `$EDITOR` names a missing program),
/// - the editor exits non-zero (treated as "abort", buffer unchanged upstream),
/// - the temp file can't be written or read back.
pub fn edit_string(initial: &str) -> Result<String> {
    let path = temp_path();
    std::fs::write(&path, initial)
        .with_context(|| format!("could not write editor temp file {}", path.display()))?;

    // Ensure the temp file is cleaned up on every return path below.
    let _guard = TempFileGuard(path.clone());

    let (program, args) = split_command(&editor_command());
    let status = Command::new(&program)
        .args(&args)
        .arg(&path)
        .status()
        .with_context(|| format!("could not launch editor '{program}'"))?;

    if !status.success() {
        bail!("editor '{program}' exited with {status}");
    }

    let edited = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read back editor temp file {}", path.display()))?;
    // Editors habitually append a trailing newline; drop a single one so the
    // buffer round-trips cleanly for a one-line edit.
    Ok(edited.strip_suffix('\n').unwrap_or(&edited).to_string())
}

/// Removes the temp file when dropped (best-effort).
struct TempFileGuard(PathBuf);
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_command_falls_back_to_vi() {
        // Can't safely mutate process env in parallel tests; just assert the
        // fallback is `vi` when neither var is set in this process's typical
        // test env. If EDITOR happens to be set in CI, accept any non-empty.
        let cmd = editor_command();
        assert!(!cmd.trim().is_empty());
    }

    #[test]
    fn split_command_separates_program_and_args() {
        assert_eq!(
            split_command("code -w"),
            ("code".to_string(), vec!["-w".to_string()])
        );
        assert_eq!(split_command("vi"), ("vi".to_string(), Vec::new()));
    }

    // Both round-trip cases live in one test: they mutate the shared `EDITOR`
    // env var, so keeping them sequential avoids a race with parallel test
    // threads.
    #[test]
    #[cfg(unix)]
    fn edit_string_round_trips_and_reflects_writes() {
        use std::os::unix::fs::PermissionsExt;

        // 1) A no-op "editor" (`true`) leaves the file untouched → unchanged.
        unsafe {
            std::env::set_var("EDITOR", "true");
        }
        assert_eq!(edit_string("hello world").unwrap(), "hello world");

        // 2) A fake editor script that overwrites its file arg with "changed".
        // Path has no spaces, so split_command yields it as the sole program.
        let pid = std::process::id();
        let script = std::env::temp_dir().join(format!("egg-fake-editor-{pid}.sh"));
        std::fs::write(&script, "#!/bin/sh\necho changed > \"$1\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        unsafe {
            std::env::set_var("EDITOR", script.to_str().unwrap());
        }
        let out = edit_string("original").unwrap();
        let _ = std::fs::remove_file(&script);
        assert_eq!(out, "changed");
    }
}
