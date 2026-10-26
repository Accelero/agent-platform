//! The strict JSON body extractor every API uses.

use axum::body::Bytes;
use axum::extract::{FromRequest, Request};
use axum::http::header;
use hermes_core::app::error::AppError;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// A JSON body parsed under the strict rules: no duplicate keys, no unsafe
/// numbers, no unknown fields. Failures never echo the body.
pub struct Body<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        let json = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(';').next() == Some("application/json"));
        if !json {
            return Err(AppError::invalid().into());
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|_| AppError::invalid())?;
        hermes_wire::strict::from_slice(&bytes)
            .map(Body)
            .map_err(|_| AppError::invalid().into())
    }
}
