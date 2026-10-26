//! `GET /api/v1/session`: the verified administrator and permitted actions.

use axum::Extension;
use axum::Json;
use hermes_core::app::access::permitted_actions;
use hermes_wire::admin::Session;

use super::auth::Authenticated;

pub async fn session(Extension(authenticated): Extension<Authenticated>) -> Json<Session> {
    Json(Session {
        actor: authenticated.identity,
        permitted_actions: permitted_actions(&authenticated.grant),
        csrf_token: authenticated.csrf_token,
    })
}
