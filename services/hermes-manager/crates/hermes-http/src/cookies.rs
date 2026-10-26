//! Opaque browser cookies. Values are generated lowercase hex handles.

use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use hermes_core::app::error::AppError;
use hermes_core::domain::ErrorKind;

const AMBIGUOUS: AppError = AppError::new(ErrorKind::Unauthenticated, "invalid_identity_evidence");

/// The cookie's value. Duplicates are refused rather than letting parser
/// order choose a credential.
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, AppError> {
    let mut found = None;
    for value in headers.get_all(header::COOKIE) {
        let value = value.to_str().map_err(|_| AMBIGUOUS)?;
        for item in value.split(';') {
            let Some((key, value)) = item.trim().split_once('=') else {
                continue;
            };
            if key == name && found.replace(value).is_some() {
                return Err(AMBIGUOUS);
            }
        }
    }
    Ok(found)
}

/// Sets a host-only, secure, HTTP-only cookie. `age = 0` expires it.
pub fn set_cookie(response: &mut Response, name: &'static str, value: &str, age: u32) {
    let value = format!("{name}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={age}");
    if let Ok(value) = HeaderValue::from_str(&value) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

/// Exactly one header with this name, as a string.
pub fn one_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, AppError> {
    let mut values = headers.get_all(name).iter();
    let value = values.next().ok_or_else(AppError::invalid)?;
    if values.next().is_some() {
        return Err(AppError::invalid());
    }
    value.to_str().map_err(|_| AppError::invalid())
}
