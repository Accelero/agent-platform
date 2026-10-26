//! Shared request and response handling for administration handlers.

use axum::Json;

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use hermes_core::app::envelope::IdempotencyKey;
use hermes_core::app::error::AppError;
use hermes_core::app::queries::PageRequest;
use hermes_core::domain::ErrorKind;
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::cookies::one_header;

pub const NOT_FOUND: AppError = AppError::new(ErrorKind::NotFound, "not_found");

pub use crate::body::Body;

pub fn idempotency_key(headers: &HeaderMap) -> Result<IdempotencyKey, AppError> {
    IdempotencyKey::parse(one_header(headers, "idempotency-key")?)
}

/// `If-Match: "<version>"`.
pub fn if_match(headers: &HeaderMap) -> Result<i64, AppError> {
    let value = one_header(headers, "if-match")?;
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|version| *version >= 1)
        .ok_or_else(AppError::invalid)
}

/// A JSON body with `ETag: "<version>"`.
pub fn tagged(version: i64, body: &impl Serialize) -> Response {
    let mut response = Json(body).into_response();
    if let Ok(tag) = HeaderValue::from_str(&format!("\"{version}\"")) {
        response.headers_mut().insert(header::ETAG, tag);
    }
    response
}

pub fn with_status(status: StatusCode, body: &impl Serialize) -> Response {
    (status, Json(body)).into_response()
}

pub fn request_id() -> Uuid {
    Uuid::new_v4()
}

pub fn path_uuid(value: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(value).map_err(|_| NOT_FOUND)
}

pub fn page(after: Option<String>, limit: Option<u16>) -> Result<PageRequest, AppError> {
    PageRequest::new(after, limit)
}

/// Query strings are refused, not ignored, when they carry unknown fields.
pub fn query<T: DeserializeOwned>(raw: Option<&str>) -> Result<T, AppError> {
    serde_urlencoded_strict(raw.unwrap_or(""))
}

fn serde_urlencoded_strict<T: DeserializeOwned>(raw: &str) -> Result<T, AppError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut map = serde_json::Map::new();
    for pair in url::form_urlencoded::parse(raw.as_bytes()) {
        let (key, value) = pair;
        if !seen.insert(key.to_string()) {
            return Err(AppError::invalid());
        }
        let value = match value.parse::<u64>() {
            Ok(number) if key == "limit" => serde_json::Value::from(number),
            _ => serde_json::Value::String(value.into_owned()),
        };
        map.insert(key.into_owned(), value);
    }
    serde_json::from_value(serde_json::Value::Object(map)).map_err(|_| AppError::invalid())
}
