//! Bounds every request: header and URI size, a 16 KiB body read inside the
//! deadline, and an overall deadline. Normalizes framework errors to the JSON
//! error shape and adds the security headers every response carries.

use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use hermes_core::app::error::AppError;
use hermes_core::domain::ErrorKind;

use crate::error::error_response;

pub const MAX_BODY: usize = 16 * 1024;
const MAX_HEADERS: usize = 16 * 1024;
const MAX_URI: usize = 8192;

/// Refuses any query string; the routes it guards take none.
pub async fn no_query(request: Request, next: Next) -> Response {
    if request.uri().query().is_some() {
        return error_response(AppError::invalid());
    }
    next.run(request).await
}

pub async fn bounded(request: Request, next: Next, deadline: Duration) -> Response {
    let header_size: usize = request
        .headers()
        .iter()
        .map(|(key, value)| key.as_str().len() + value.as_bytes().len())
        .sum();
    let mut response = if header_size > MAX_HEADERS || request.uri().to_string().len() > MAX_URI {
        error_response(AppError::invalid())
    } else {
        let run = async {
            // Body limits on extractors are per route; reading here bounds every
            // route, including streamed bodies without a length.
            let (parts, body) = request.into_parts();
            match to_bytes(body, MAX_BODY).await {
                Ok(bytes) => {
                    next.run(Request::from_parts(parts, Body::from(bytes)))
                        .await
                }
                Err(_) => error_response(AppError::invalid()),
            }
        };
        tokio::time::timeout(deadline, run)
            .await
            .unwrap_or_else(|_| error_response(AppError::unavailable()))
    };
    let json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(b"application/json"));
    if response.status().is_client_error() && !json {
        // Framework rejections must not reflect submitted values or become HTML.
        response = match response.status() {
            StatusCode::NOT_FOUND => {
                error_response(AppError::new(ErrorKind::NotFound, "not_found"))
            }
            StatusCode::METHOD_NOT_ALLOWED => {
                let mut response = error_response(AppError::invalid());
                *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
                response
            }
            _ => error_response(AppError::invalid()),
        };
    }
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}
