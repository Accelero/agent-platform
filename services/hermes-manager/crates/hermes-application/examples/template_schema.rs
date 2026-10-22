use hermes_application::commissioning::TemplateDefinition;
fn main() {
    let schema = schemars::generate::SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<TemplateDefinition>();
    let mut schema = serde_json::to_value(schema).unwrap();
    schema["$id"] = serde_json::json!("urn:hermes-manager:template:1");
    schema["x-revision"] = serde_json::json!(1);
    println!("{}", serde_json::to_string_pretty(&schema).unwrap());
}
