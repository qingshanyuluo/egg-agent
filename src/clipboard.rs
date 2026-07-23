//! Clipboard copy for the TUI.
//!
//! Primary path is OSC 52, a terminal escape sequence that asks the terminal to
//! put text on the system clipboard — zero dependencies, works over SSH, and is
//! supported by modern terminals (Ghostty, kitty, iTerm2, WezTerm, tmux).
//!
//! On macOS we also pipe to `pbcopy` as a fallback for terminals that disable
//! OSC 52. Both are best-effort; failure is silent (the UI just won't show the
//! "copied" toast if the caller decides so — here we always attempt both).

use std::io::Write;

/// Copy `text` to the system clipboard. Returns true if at least one method was
/// dispatched without error.
pub fn copy(text: &str) -> bool {
    let osc = osc52(text);
    let pb = pbcopy(text);
    osc || pb
}

/// Emit an OSC 52 sequence on stdout: ESC ] 52 ; c ; <base64> BEL
fn osc52(text: &str) -> bool {
    let encoded = base64_encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{encoded}\x07");
    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes()).is_ok() && out.flush().is_ok()
}

/// macOS fallback: pipe into `pbcopy`.
#[cfg(target_os = "macos")]
fn pbcopy(text: &str) -> bool {
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() else {
        return false;
    };
    if let Some(stdin) = child.stdin.as_mut() {
        if stdin.write_all(text.as_bytes()).is_err() {
            return false;
        }
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn pbcopy(_text: &str) -> bool {
    false
}

/// Minimal standard base64 encoder (no external crate).
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // multibyte
        assert_eq!(base64_encode("你好".as_bytes()), "5L2g5aW9");
    }
}
