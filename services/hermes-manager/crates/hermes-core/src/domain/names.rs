//! Identifier, digest and path validators shared by every context.

use super::{ErrorKind, Rejection};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameRejected {
    Name,
    Digest,
    Image,
    Path,
}

impl Rejection for NameRejected {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Invalid
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Name => "invalid_name",
            Self::Digest => "invalid_digest",
            Self::Image => "invalid_image",
            Self::Path => "invalid_path",
        }
    }
}

/// A DNS-label-like identifier: lowercase, starts with a letter, at most 63 bytes.
pub fn validate_name(name: &str) -> Result<(), NameRejected> {
    let valid = !name.is_empty()
        && name.len() <= 63
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    valid.then_some(()).ok_or(NameRejected::Name)
}

/// Exactly 64 lowercase hexadecimal characters.
pub fn validate_sha256(digest: &str) -> Result<(), NameRejected> {
    let valid = digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    valid.then_some(()).ok_or(NameRejected::Digest)
}

/// An OCI reference pinned by digest: `repository@sha256:<64 hex>`.
pub fn validate_image(image: &str) -> Result<(), NameRejected> {
    let (repository, digest) = image.rsplit_once("@sha256:").ok_or(NameRejected::Image)?;
    validate_sha256(digest).map_err(|_| NameRejected::Image)?;
    let valid = !repository.is_empty()
        && repository.len() <= 512
        && repository.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && !repository.contains("//")
        && !repository
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
        && repository
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._/:-".contains(&b));
    valid.then_some(()).ok_or(NameRejected::Image)
}

/// A clean relative path: no leading `/`, no `.`, `..` or empty segments, no
/// control characters or backslashes, at most 1024 bytes.
pub fn validate_relative_path(path: &str) -> Result<(), NameRejected> {
    let valid = !path.is_empty()
        && path.len() <= 1024
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.chars().any(|c| c.is_control() || c == '\\')
        });
    valid.then_some(()).ok_or(NameRejected::Path)
}

/// A clean absolute path other than `/`.
pub fn validate_absolute_path(path: &str) -> Result<(), NameRejected> {
    path.strip_prefix('/')
        .ok_or(NameRejected::Path)
        .and_then(validate_relative_path)
}

/// Whether one path equals the other or contains it, by whole segments.
pub fn paths_overlap(left: &str, right: &str) -> bool {
    let (left, right) = (left.trim_start_matches('/'), right.trim_start_matches('/'));
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|rest| rest.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_bounded_lowercase_labels() {
        for name in ["a", "personal-default", "a1", &"a".repeat(63)] {
            assert_eq!(validate_name(name), Ok(()), "{name}");
        }
        for name in ["", "1a", "a-", "A", "a_b", "a.b", &"a".repeat(64)] {
            assert_eq!(validate_name(name), Err(NameRejected::Name), "{name}");
        }
    }

    #[test]
    fn images_must_be_pinned_by_digest() {
        let digest = "a".repeat(64);
        assert_eq!(
            validate_image(&format!("ghcr.io/org/hermes@sha256:{digest}")),
            Ok(())
        );
        for image in [
            "ghcr.io/org/hermes:latest".to_owned(),
            format!("ghcr.io/org/hermes@sha256:{}", "A".repeat(64)),
            format!("ghcr.io//hermes@sha256:{digest}"),
            format!("ghcr.io/../hermes@sha256:{digest}"),
            format!("Ghcr.io/hermes@sha256:{digest}"),
        ] {
            assert_eq!(validate_image(&image), Err(NameRejected::Image), "{image}");
        }
    }

    #[test]
    fn relative_paths_cannot_escape_or_be_ambiguous() {
        assert_eq!(validate_relative_path("homes/bob"), Ok(()));
        for path in [
            "", "/bob", "bob/", "a//b", ".", "a/./b", "../x", "a/..", "a\\b", "a\nb",
        ] {
            assert_eq!(
                validate_relative_path(path),
                Err(NameRejected::Path),
                "{path:?}"
            );
        }
        assert_eq!(validate_absolute_path("/workspace"), Ok(()));
        assert_eq!(validate_absolute_path("/"), Err(NameRejected::Path));
        assert_eq!(validate_absolute_path("workspace"), Err(NameRejected::Path));
    }

    #[test]
    fn overlap_is_by_whole_segments() {
        assert!(paths_overlap("homes/bob", "homes/bob"));
        assert!(paths_overlap("homes/bob", "homes/bob/projects"));
        assert!(paths_overlap("/workspace/a", "/workspace"));
        assert!(!paths_overlap("homes/bob", "homes/bobby"));
        assert!(!paths_overlap("homes/alice", "homes/bob"));
    }
}
