//! Instance material: credentials and keys generated once per instance and
//! purpose, stored sealed before any use, and never regenerated.

/// One kind of instance material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MaterialPurpose {
    /// The native dashboard's session signing token.
    DashboardSession,
    /// The bearer credential the plugin presents to the execution API.
    ExecutionCredential,
    /// The SSH client key Hermes uses for its terminal backends.
    SshClient,
    /// The host key every terminal backend of this instance presents.
    WorkerHost,
    /// The key that signs the home-initialization receipt. Only the init
    /// container and the controller see it.
    Initialization,
}

impl MaterialPurpose {
    pub const ALL: [Self; 5] = [
        Self::DashboardSession,
        Self::ExecutionCredential,
        Self::SshClient,
        Self::WorkerHost,
        Self::Initialization,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DashboardSession => "dashboard-session",
            Self::ExecutionCredential => "execution-credential",
            Self::SshClient => "ssh-client",
            Self::WorkerHost => "worker-host",
            Self::Initialization => "initialization",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|purpose| purpose.as_str() == value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purposes_round_trip_and_are_distinct() {
        for purpose in MaterialPurpose::ALL {
            assert_eq!(MaterialPurpose::parse(purpose.as_str()), Some(purpose));
        }
        let mut names: Vec<_> = MaterialPurpose::ALL.iter().map(|p| p.as_str()).collect();
        names.dedup();
        assert_eq!(names.len(), MaterialPurpose::ALL.len());
        assert_eq!(MaterialPurpose::parse("native-bootstrap-v1"), None);
    }
}
