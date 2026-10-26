//! Platform sessions as the gateway uses them. [`BrowserAuth`] with the
//! gateway purpose implements the port; tests substitute fakes.

use async_trait::async_trait;
use hermes_core::domain::identity::Identity;
use hermes_identity::AuthError;
use hermes_identity::browser::BrowserAuth;

/// A valid session: its verified identity, an opaque handle that identifies
/// it without the cookie value, and its CSRF token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedIn {
    pub identity: Identity,
    pub handle: String,
    pub csrf: String,
}

#[async_trait]
pub trait Sessions: Send + Sync {
    /// Authenticates a request from its two platform cookies.
    async fn authenticate(&self, session: &str, browser: &str) -> Result<SignedIn, AuthError>;
    /// Re-authenticates an established connection by its handle.
    async fn recheck(&self, handle: &str) -> Result<Identity, AuthError>;
    async fn logout(&self, session: &str, browser: &str, csrf: &str) -> Result<(), AuthError>;
}

#[async_trait]
impl Sessions for BrowserAuth {
    async fn authenticate(&self, session: &str, browser: &str) -> Result<SignedIn, AuthError> {
        let signed = BrowserAuth::authenticate(self, session, browser).await?;
        Ok(SignedIn {
            identity: signed.identity,
            handle: signed.session_handle,
            csrf: signed.csrf_token,
        })
    }

    async fn recheck(&self, handle: &str) -> Result<Identity, AuthError> {
        Ok(self.authenticate_handle(handle).await?.identity)
    }

    async fn logout(&self, session: &str, browser: &str, csrf: &str) -> Result<(), AuthError> {
        BrowserAuth::logout(self, session, browser, csrf).await
    }
}
