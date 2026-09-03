use anyhow::Context;
use std::path::Path;

/// Read the token at `path`, creating one if it is not there yet.
///
/// The token is only ever read from this file, and it must never be exported
/// into the environment: it is equivalent to the right to drive the mux, and
/// every child process a session spawns can read its parent's environment.
pub fn load_or_create(path: &Path) -> anyhow::Result<String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    let token = generate().context("generating a bridge token")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, &token).with_context(|| format!("writing {}", path.display()))?;
    restrict_permissions(path);
    Ok(token)
}

/// 32 bytes from the operating system, rendered as hex.
///
/// Deliberately not seeded from the clock and the pid: those are guessable by
/// anything running on the same machine, which is exactly the threat here.
fn generate() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    // getrandom::Error is no_std and does not implement std::error::Error, so
    // it cannot be given to anyhow's context directly.
    getrandom::fill(&mut bytes)
        .map_err(|err| anyhow::anyhow!("reading the operating system's random source: {err}"))?;
    let mut out = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write;
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(out)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {
    // On Windows the file inherits the user profile's ACL, which already
    // excludes other users.
}

/// Compare in constant time, so a wrong token cannot be guessed one byte at a
/// time by measuring how long the answer takes.
pub fn verify(expected: &str, presented: &str) -> bool {
    if expected.len() != presented.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.bytes().zip(presented.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_missing_file_gets_a_fresh_token() {
        let dir = tempfile::tempdir().expect("to make a tempdir");
        let path = dir.path().join("bridge-token");
        let token = load_or_create(&path).expect("to create");
        assert_eq!(token.len(), 64, "32 bytes rendered as hex");
        assert!(path.exists());
    }

    #[test]
    fn an_existing_token_is_reused() {
        let dir = tempfile::tempdir().expect("to make a tempdir");
        let path = dir.path().join("bridge-token");
        let first = load_or_create(&path).expect("to create");
        let second = load_or_create(&path).expect("to load");
        assert_eq!(
            first, second,
            "regenerating would lock out a client that is already connected"
        );
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        let dir = tempfile::tempdir().expect("to make a tempdir");
        let path = dir.path().join("bridge-token");
        std::fs::write(&path, "abc123\n").expect("to write");
        assert_eq!(load_or_create(&path).expect("to load"), "abc123");
    }

    #[test]
    fn two_tokens_are_not_the_same() {
        let dir = tempfile::tempdir().expect("to make a tempdir");
        let first = load_or_create(&dir.path().join("a")).expect("to create");
        let second = load_or_create(&dir.path().join("b")).expect("to create");
        assert_ne!(first, second, "a fixed token would be no token at all");
    }

    #[test]
    fn verify_rejects_a_wrong_token() {
        assert!(!verify("expected", "presented"));
    }

    #[test]
    fn verify_accepts_the_right_token() {
        assert!(verify("expected", "expected"));
    }

    #[test]
    fn verify_rejects_a_prefix() {
        assert!(
            !verify("expectedlonger", "expected"),
            "a prefix must not pass"
        );
    }
}
