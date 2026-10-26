//! One mapping from application errors to HTTP responses.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use hermes_core::app::error::AppError;
use hermes_core::domain::ErrorKind;
use hermes_wire::ErrorBody;

pub fn status(kind: ErrorKind) -> StatusCode {
    match kind {
        ErrorKind::Invalid => StatusCode::UNPROCESSABLE_ENTITY,
        ErrorKind::Unauthenticated => StatusCode::UNAUTHORIZED,
        ErrorKind::Forbidden => StatusCode::FORBIDDEN,
        ErrorKind::NotFound => StatusCode::NOT_FOUND,
        ErrorKind::Conflict | ErrorKind::Pending => StatusCode::CONFLICT,
        ErrorKind::Precondition => StatusCode::PRECONDITION_FAILED,
        ErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// `{"error": code}` with the kind's status. Never reflects request content.
pub fn error_response(error: AppError) -> Response {
    let body = ErrorBody {
        error: error.code,
        problems: hermes_wire::problems(&error.problems),
    };
    let mut response = (status(error.kind), Json(body)).into_response();
    if error.kind == ErrorKind::Unauthenticated {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}

/// Lets handlers use `?` on anything convertible to [`AppError`].
pub struct ApiError(pub AppError);

impl<E: Into<AppError>> From<E> for ApiError {
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error_response(self.0)
    }
}
