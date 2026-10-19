//! Generic fresh-owner credentials and seed, never imported personal state.
use crate::{
    materials::{MaterialVault, PlainMaterial},
    session_crypto::random_handle,
};
use hermes_application::materials::MaterialRepository;
use hermes_application::{Error, InstallationScope, OperationClaim};
use hermes_domain::TemplateVersion;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ssh_key::{
    LineEnding, PrivateKey,
    private::{Ed25519Keypair, KeypairData},
};
use url::Url;
use zeroize::{ZeroizeOnDrop, Zeroizing};

#[derive(Serialize, Deserialize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeBootstrap {
    pub schema_version: u32,
    pub dashboard_session_token: String,
    pub execution_client_token: String,
    pub initialization_key: String,
    pub ssh_private_key: String,
    pub ssh_public_key: String,
    #[zeroize(skip)]
    pub template: TemplateVersion,
    #[zeroize(skip)]
    pub native_config: Value,
}
pub const PURPOSE: &str = "native-bootstrap-v1";

impl NativeBootstrap {
    pub fn generate(template: TemplateVersion, execution_url: &str) -> Result<Self, Error> {
        template.validate()?;
        let url = Url::parse(execution_url).map_err(|_| Error::Invalid)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/execution"
        {
            return Err(Error::Invalid);
        }
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *seed).map_err(|_| Error::Unavailable)?;
        let key = PrivateKey::new(KeypairData::Ed25519(Ed25519Keypair::from_seed(&seed)), "")
            .map_err(|_| Error::Unavailable)?;
        let private = key
            .to_openssh(LineEnding::LF)
            .map_err(|_| Error::Unavailable)?;
        let public = key
            .public_key()
            .to_openssh()
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            schema_version: 1,
            dashboard_session_token: random_handle()?,
            execution_client_token: random_handle()?,
            initialization_key: random_handle()?,
            ssh_private_key: private.to_string(),
            ssh_public_key: format!("{public}\n"),
            template,
            native_config: json!({
                "terminal":{"backend":"managed_execution","cwd":"/workspace","ssh_key":"/run/hermes/credentials/id_ed25519"},
                "plugins":{"enabled":["managed_execution"],"entries":{"managed_execution":{"settings":{"manager_url":execution_url,"token_file":"/run/hermes/credentials/execution-client-token","ca_file":"/run/hermes/execution-ca/ca.crt","profile":"default","mode":"per_session","ssh_key":"/run/hermes/credentials/id_ed25519","acquire_timeout_seconds":180}}}},
            }),
        })
    }
    pub fn decode(material: &PlainMaterial) -> Result<Self, Error> {
        let value: Self =
            serde_json::from_slice(&material.value).map_err(|_| Error::RecoveryRequired)?;
        if value.schema_version != 1
            || material.context.purpose != PURPOSE
            || !crate::session_crypto::valid_handle(&value.dashboard_session_token)
            || !crate::session_crypto::valid_handle(&value.execution_client_token)
            || !crate::session_crypto::valid_handle(&value.initialization_key)
            || value
                .native_config
                .pointer("/terminal/backend")
                .and_then(Value::as_str)
                != Some("managed_execution")
        {
            return Err(Error::RecoveryRequired);
        }
        let private = PrivateKey::from_openssh(&value.ssh_private_key)
            .map_err(|_| Error::RecoveryRequired)?;
        if private
            .public_key()
            .to_openssh()
            .map_err(|_| Error::RecoveryRequired)?
            != value.ssh_public_key.trim()
        {
            return Err(Error::RecoveryRequired);
        }
        value.template.validate()?;
        Ok(value)
    }
}
impl<R: MaterialRepository> MaterialVault<R> {
    pub async fn native_bootstrap(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        template: TemplateVersion,
        execution_url: &str,
    ) -> Result<(PlainMaterial, NativeBootstrap), Error> {
        let material = self
            .get_or_create(scope, claim, PURPOSE, || {
                let bootstrap = NativeBootstrap::generate(template, execution_url)?;
                Ok(Zeroizing::new(
                    serde_json::to_vec(&bootstrap).map_err(|_| Error::Invalid)?,
                ))
            })
            .await?;
        let bootstrap = NativeBootstrap::decode(&material)?;
        Ok((material, bootstrap))
    }
}
