//! Credentials resolve a single immutable instance.
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{Error, InstallationScope, OperationClaim, materials::MaterialContext};

/// Deliberately no Debug/Serialize. Raw bearer credentials never cross this
/// persistence port and never belong in an audit, input JSON, or SQL output.
#[derive(Clone, PartialEq, Eq)]
pub struct ExecutionTokenHash([u8; 32]);
impl ExecutionTokenHash {
    pub fn from_token(token: &str) -> Result<Self, Error> {
        if token.len() != 64
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid);
        }
        Ok(Self(Sha256::digest(token.as_bytes()).into()))
    }
    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Created by credential verification only, never deserialize from requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionClient {
    pub id: Uuid,
    pub installation_id: Uuid,
    pub instance_id: Uuid,
    pub epoch: i64,
}

#[async_trait]
pub trait ExecutionClientRepository: Send + Sync {
    /// Bind the existing encrypted native bootstrap intent to an independent
    /// hashed execution credential before exposing that credential to Hermes.
    /// Reconciliation never replaces the credential or widens its instance.
    async fn register_under_current_claim(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        material: &MaterialContext,
        token_hash: ExecutionTokenHash,
    ) -> Result<ExecutionClient, Error>;
    async fn authenticate_execution_client(
        &self,
        scope: &InstallationScope,
        token_hash: ExecutionTokenHash,
    ) -> Result<ExecutionClient, Error>;
}
