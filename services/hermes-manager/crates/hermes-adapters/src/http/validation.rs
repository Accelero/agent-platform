use super::*;
use crate::template_validation::{StrictValue, problem, validate};
pub(super) async fn validate_template(body: Result<Json<StrictValue>, JsonRejection>) -> Response {
    let Ok(Json(StrictValue(value))) = body else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":"invalid_configuration","problems":[problem("".into(),"syntax")]})),
        )
            .into_response();
    };
    let problems = validate(&value);
    let reviewed = value
        .get("image")
        .and_then(Value::as_str)
        .is_some_and(|image| {
            image.ends_with(
                "@sha256:d4da4a40cd7a28aba983775d9fd31d94cbf153eeb0cb9e844d6d0f612b7c24db",
            )
        });
    if problems.is_empty() {
        Json(json!({"valid":true,"schemaId":"urn:hermes-manager:template:1","revision":1,"coverage":if reviewed{"reviewed_keys"}else{"envelope_only"},"problems":[]})).into_response()
    } else {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":"invalid_configuration","problems":problems})),
        )
            .into_response()
    }
}

pub(super) async fn configuration_schemas() -> Response {
    Json(
        serde_json::from_str::<Value>(include_str!(
            "../../../../../../contracts/hermes-manager/schemas/registry.json"
        ))
        .expect("reviewed schema registry"),
    )
    .into_response()
}
