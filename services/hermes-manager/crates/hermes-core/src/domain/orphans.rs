//! Owned cluster objects without a registry record, for example after
//! PostgreSQL was restored from an older backup. They are reported, never
//! adopted or deleted.

use std::collections::BTreeMap;

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OrphanKind {
    Namespace,
    PersistentVolume,
}

impl OrphanKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Namespace => "namespace",
            Self::PersistentVolume => "persistent_volume",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "namespace" => Some(Self::Namespace),
            "persistent_volume" => Some(Self::PersistentVolume),
            _ => None,
        }
    }
}

/// A namespace or PersistentVolume carrying this installation's label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedObject {
    pub kind: OrphanKind,
    pub name: String,
    pub uid: String,
    /// Its instance label, when it holds a UUID.
    pub instance: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Orphan {
    pub kind: OrphanKind,
    pub name: String,
    pub uid: String,
}

/// The owned objects that belong to no recorded instance. `recorded` maps
/// every instance to its namespace. A namespace belongs to its instance only
/// under the recorded name; a PersistentVolume only when its instance is
/// recorded and its name starts with that instance's namespace.
pub fn orphans(owned: &[OwnedObject], recorded: &BTreeMap<Uuid, String>) -> Vec<Orphan> {
    let mut found: Vec<Orphan> = owned
        .iter()
        .filter(|object| {
            let namespace = object.instance.and_then(|id| recorded.get(&id));
            let known = match (object.kind, namespace) {
                (_, None) => false,
                (OrphanKind::Namespace, Some(namespace)) => object.name == *namespace,
                (OrphanKind::PersistentVolume, Some(namespace)) => object
                    .name
                    .strip_prefix(namespace.as_str())
                    .is_some_and(|rest| rest.starts_with('-')),
            };
            !known
        })
        .map(|object| Orphan {
            kind: object.kind,
            name: object.name.clone(),
            uid: object.uid.clone(),
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(kind: OrphanKind, name: &str, instance: Option<Uuid>) -> OwnedObject {
        OwnedObject {
            kind,
            name: name.into(),
            uid: format!("uid-{name}"),
            instance,
        }
    }

    #[test]
    fn only_objects_of_recorded_instances_under_their_names_are_known() {
        let known = Uuid::from_u128(1);
        let unknown = Uuid::from_u128(2);
        let recorded = BTreeMap::from([(known, "hermes-1".to_owned())]);
        let owned = [
            object(OrphanKind::Namespace, "hermes-1", Some(known)),
            object(
                OrphanKind::PersistentVolume,
                "hermes-1-workspace",
                Some(known),
            ),
            object(OrphanKind::Namespace, "hermes-2", Some(unknown)),
            object(
                OrphanKind::PersistentVolume,
                "hermes-2-workspace",
                Some(unknown),
            ),
            // A recorded instance's label on another name is not its object.
            object(OrphanKind::Namespace, "hermes-9", Some(known)),
            object(OrphanKind::PersistentVolume, "hermes-10-home", Some(known)),
            object(OrphanKind::PersistentVolume, "unlabelled", None),
        ];
        let names: Vec<_> = orphans(&owned, &recorded)
            .into_iter()
            .map(|orphan| (orphan.kind.as_str(), orphan.name))
            .collect();
        assert_eq!(
            names,
            [
                ("namespace", "hermes-2".to_owned()),
                ("namespace", "hermes-9".to_owned()),
                ("persistent_volume", "hermes-10-home".to_owned()),
                ("persistent_volume", "hermes-2-workspace".to_owned()),
                ("persistent_volume", "unlabelled".to_owned()),
            ]
        );
    }

    #[test]
    fn kinds_round_trip() {
        for kind in [OrphanKind::Namespace, OrphanKind::PersistentVolume] {
            assert_eq!(OrphanKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(OrphanKind::parse("pod"), None);
    }
}
