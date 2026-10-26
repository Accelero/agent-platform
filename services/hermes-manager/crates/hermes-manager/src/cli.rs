//! Command-line parsing. Identities and secrets are never arguments; they come
//! from files.

use std::path::PathBuf;

use hermes_core::app::error::AppError;

pub enum Role {
    Migrate,
    Check,
    Initialize,
    GrantAdministrator {
        identity_file: PathBuf,
        recover: bool,
    },
    RebindCluster,
    Api,
    Controller,
    /// Writes a new controller sealing key pair; needs no configuration.
    GenerateSealingKey {
        private_key_file: PathBuf,
        public_key_file: PathBuf,
    },
}

pub struct Command {
    pub role: Role,
    /// Absent only for `generate-sealing-key`.
    pub config: Option<PathBuf>,
}

/// `hermes-manager <role> --config <file> [--identity-file <file>] [--recover]`, or
/// `hermes-manager generate-sealing-key --private-key-file <file> --public-key-file <file>`.
pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Command, AppError> {
    let role = args.next().ok_or_else(AppError::invalid)?;
    let (mut config, mut identity_file, mut recover) = (None, None, false);
    let (mut private_key_file, mut public_key_file) = (None, None);
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "--config" => &mut config,
            "--identity-file" => &mut identity_file,
            "--private-key-file" => &mut private_key_file,
            "--public-key-file" => &mut public_key_file,
            "--recover" if !recover => {
                recover = true;
                continue;
            }
            _ => return Err(AppError::invalid()),
        };
        if slot.is_some() {
            return Err(AppError::invalid());
        }
        *slot = Some(PathBuf::from(args.next().ok_or_else(AppError::invalid)?));
    }
    let keys = private_key_file.is_some() || public_key_file.is_some();
    let role = match (role.as_str(), identity_file, recover, keys) {
        ("migrate", None, false, false) => Role::Migrate,
        ("check", None, false, false) => Role::Check,
        ("initialize", None, false, false) => Role::Initialize,
        ("rebind-cluster", None, false, false) => Role::RebindCluster,
        ("api", None, false, false) => Role::Api,
        ("controller", None, false, false) => Role::Controller,
        ("grant-administrator", Some(identity_file), recover, false) => Role::GrantAdministrator {
            identity_file,
            recover,
        },
        ("generate-sealing-key", None, false, true) if config.is_none() => {
            return Ok(Command {
                role: Role::GenerateSealingKey {
                    private_key_file: private_key_file.ok_or_else(AppError::invalid)?,
                    public_key_file: public_key_file.ok_or_else(AppError::invalid)?,
                },
                config: None,
            });
        }
        _ => return Err(AppError::invalid()),
    };
    Ok(Command {
        role,
        config: Some(config.ok_or_else(AppError::invalid)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> impl Iterator<Item = String> {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn roles_take_only_their_own_options() {
        for (line, check) in [
            (
                "api --config c.json",
                (|r: &Role| matches!(r, Role::Api)) as fn(&Role) -> bool,
            ),
            ("controller --config c.json", |r| {
                matches!(r, Role::Controller)
            }),
            ("initialize --config c.json", |r| {
                matches!(r, Role::Initialize)
            }),
            ("rebind-cluster --config c.json", |r| {
                matches!(r, Role::RebindCluster)
            }),
            (
                "grant-administrator --config c.json --identity-file i.json --recover",
                |r| matches!(r, Role::GrantAdministrator { recover: true, .. }),
            ),
            (
                "generate-sealing-key --private-key-file k --public-key-file p",
                |r| matches!(r, Role::GenerateSealingKey { .. }),
            ),
        ] {
            assert!(check(&parse(args(line)).unwrap().role), "{line}");
        }
        for line in [
            "",
            "api",
            "api --config",
            "api --config a --config b",
            "api --config c.json --recover",
            "grant-administrator --config c.json",
            "unknown --config c.json",
            "api --config c.json extra",
            "controller --config c.json --private-key-file k",
            "generate-sealing-key --private-key-file k",
            "generate-sealing-key --config c.json --private-key-file k --public-key-file p",
        ] {
            assert!(parse(args(line)).is_err(), "{line:?}");
        }
    }
}
