//! Versioned catalogs: Hermes templates and terminal backends share one
//! lifecycle. A draft is edited in place (each edit advances its revision);
//! publishing freezes it; withdrawing stops new selections.

use serde::{Deserialize, Serialize};

use super::{ErrorKind, Rejection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogKind {
    Template,
    TerminalBackend,
}

impl CatalogKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Template => "template",
            Self::TerminalBackend => "terminal_backend",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionState {
    Draft,
    Published,
    Withdrawn,
}

impl VersionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::Withdrawn => "withdrawn",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(Self::Draft),
            "published" => Some(Self::Published),
            "withdrawn" => Some(Self::Withdrawn),
            _ => None,
        }
    }
}

/// An exact catalog version, as assignments pin it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VersionRef {
    pub id: String,
    pub version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionStatus {
    pub state: VersionState,
    pub revision: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogRejected {
    NotDraft,
    StaleRevision,
    NotFound,
    /// Not published, or withdrawn: cannot be newly selected.
    Unavailable(CatalogKind),
}

impl Rejection for CatalogRejected {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::NotDraft => ErrorKind::Conflict,
            Self::StaleRevision => ErrorKind::Precondition,
            Self::NotFound => ErrorKind::NotFound,
            Self::Unavailable(_) => ErrorKind::Invalid,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::NotDraft => "version_not_draft",
            Self::StaleRevision => "stale_version",
            Self::NotFound => "not_found",
            Self::Unavailable(CatalogKind::Template) => "template_unavailable",
            Self::Unavailable(CatalogKind::TerminalBackend) => "backend_unavailable",
        }
    }
}

/// Editing keeps the draft and advances its revision.
pub fn edit(
    current: VersionStatus,
    expected_revision: i64,
) -> Result<VersionStatus, CatalogRejected> {
    if current.state != VersionState::Draft {
        return Err(CatalogRejected::NotDraft);
    }
    if current.revision != expected_revision {
        return Err(CatalogRejected::StaleRevision);
    }
    Ok(VersionStatus {
        state: VersionState::Draft,
        revision: current.revision + 1,
    })
}

pub fn publish(
    current: VersionStatus,
    expected_revision: i64,
) -> Result<VersionStatus, CatalogRejected> {
    let edited = edit(current, expected_revision)?;
    Ok(VersionStatus {
        state: VersionState::Published,
        ..edited
    })
}

pub fn withdraw(
    current: VersionStatus,
    expected_revision: i64,
) -> Result<VersionStatus, CatalogRejected> {
    if current.state != VersionState::Published {
        return Err(CatalogRejected::NotDraft);
    }
    if current.revision != expected_revision {
        return Err(CatalogRejected::StaleRevision);
    }
    Ok(VersionStatus {
        state: VersionState::Withdrawn,
        revision: current.revision + 1,
    })
}

/// Only published versions can be newly selected by an assignment.
pub fn selectable(kind: CatalogKind, status: Option<VersionStatus>) -> Result<(), CatalogRejected> {
    match status {
        Some(VersionStatus {
            state: VersionState::Published,
            ..
        }) => Ok(()),
        _ => Err(CatalogRejected::Unavailable(kind)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRAFT: VersionStatus = VersionStatus {
        state: VersionState::Draft,
        revision: 2,
    };

    #[test]
    fn drafts_edit_publish_and_withdraw_in_order() {
        assert_eq!(edit(DRAFT, 2).unwrap().revision, 3);
        assert_eq!(edit(DRAFT, 1), Err(CatalogRejected::StaleRevision));
        let published = publish(DRAFT, 2).unwrap();
        assert_eq!(published.state, VersionState::Published);
        assert_eq!(
            edit(published, published.revision),
            Err(CatalogRejected::NotDraft)
        );
        assert_eq!(
            publish(published, published.revision),
            Err(CatalogRejected::NotDraft)
        );
        let withdrawn = withdraw(published, published.revision).unwrap();
        assert_eq!(withdrawn.state, VersionState::Withdrawn);
        assert_eq!(
            withdraw(withdrawn, withdrawn.revision),
            Err(CatalogRejected::NotDraft)
        );
        assert_eq!(withdraw(DRAFT, 2), Err(CatalogRejected::NotDraft));
    }

    #[test]
    fn only_published_versions_are_selectable() {
        let kind = CatalogKind::Template;
        assert!(selectable(kind, Some(publish(DRAFT, 2).unwrap())).is_ok());
        for status in [
            None,
            Some(DRAFT),
            Some(VersionStatus {
                state: VersionState::Withdrawn,
                revision: 9,
            }),
        ] {
            assert_eq!(
                selectable(kind, status),
                Err(CatalogRejected::Unavailable(kind))
            );
        }
    }
}
