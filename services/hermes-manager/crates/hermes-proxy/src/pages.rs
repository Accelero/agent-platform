//! The gateway's own responses: status pages for browser navigation and JSON
//! for everything else. They never name another owner, an instance or a
//! namespace.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use hermes_wire::routing::PendingCode;
use serde_json::json;

const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; \
                   form-action 'self'; frame-ancestors 'none'";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Pending(PendingCode),
    Denied,
    Unavailable,
    /// The owner's instance did not answer.
    BadGateway,
    SignedOut,
}

const SIGN_OUT: &str =
    "<form method=\"post\" action=\"/auth/logout\"><button>Sign out</button></form>";

fn document(title: &str, body: &str, refresh: bool) -> String {
    let refresh = if refresh {
        "<meta http-equiv=\"refresh\" content=\"5\">"
    } else {
        ""
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">{refresh}\
         <title>{title} · Hermes</title><style>body{{font:16px/1.5 system-ui,sans-serif;\
         max-width:36rem;margin:4rem auto;padding:0 1rem}}</style></head>\
         <body><main><h1>{title}</h1>{body}</main></body></html>"
    )
}

impl Page {
    pub fn status(self) -> StatusCode {
        match self {
            Self::Pending(_) => StatusCode::ACCEPTED,
            Self::Denied => StatusCode::FORBIDDEN,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::BadGateway => StatusCode::BAD_GATEWAY,
            Self::SignedOut => StatusCode::OK,
        }
    }

    fn html(self) -> String {
        match self {
            Self::Pending(PendingCode::InstanceNotReady) => document(
                "Starting your Hermes",
                "<p>Your Hermes is being prepared. This page reloads by itself.</p>",
                true,
            ),
            Self::Pending(PendingCode::InstanceBlocked) => document(
                "Your Hermes needs an administrator",
                &format!(
                    "<p>Your Hermes cannot start until an administrator resolves a problem. \
                     This page reloads by itself.</p>{SIGN_OUT}"
                ),
                true,
            ),
            Self::Denied => document(
                "No Hermes for this account",
                &format!(
                    "<p>This account has no enabled Hermes. Ask an administrator for access.</p>{SIGN_OUT}"
                ),
                false,
            ),
            Self::Unavailable => document(
                "Hermes is unavailable",
                "<p>Hermes cannot check your access right now. Try again in a moment.</p>",
                false,
            ),
            Self::BadGateway => document(
                "Your Hermes did not respond",
                "<p>Your Hermes is not answering right now. Try again in a moment.</p>",
                false,
            ),
            Self::SignedOut => document(
                "Signed out",
                "<p>Your Hermes session has ended.</p><p><a href=\"/_platform/login\">Sign in again</a></p>",
                false,
            ),
        }
    }

    fn json(self) -> serde_json::Value {
        match self {
            Self::Pending(reason) => json!({"status": "pending", "reason": reason}),
            Self::Denied => json!({"error": "access_denied"}),
            Self::Unavailable | Self::BadGateway => json!({"error": "dependency_unavailable"}),
            Self::SignedOut => json!({"status": "signed_out"}),
        }
    }

    /// The page for a browser navigation, otherwise its JSON form.
    pub fn respond(self, navigation: bool) -> Response {
        let mut response = if navigation {
            let mut response = (self.status(), Html(self.html())).into_response();
            response.headers_mut().insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(CSP),
            );
            response
        } else {
            (self.status(), Json(self.json())).into_response()
        };
        own_headers(&mut response);
        response
    }
}

/// Headers on every response the gateway writes itself.
pub fn own_headers(response: &mut Response) {
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
}

/// A JSON error in the gateway contract's shape.
pub fn error(status: StatusCode, code: &'static str) -> Response {
    let mut response = (status, Json(json!({"error": code}))).into_response();
    own_headers(&mut response);
    response
}
