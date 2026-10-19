use super::*;
use hermes_domain::TemplateVersion;
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[test]
fn real_python_initializer_receipt_is_verified_for_exact_owner_material_and_key() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("home");
    std::fs::create_dir(&root).unwrap();
    let material = MaterialContext {
        id: Uuid::new_v4(),
        installation_id: Uuid::new_v4(),
        instance_id: Uuid::new_v4(),
        epoch: 1,
        operation_id: Uuid::new_v4(),
        generation: 1,
        purpose: crate::native_bootstrap::PURPOSE.into(),
    };
    let mut bootstrap = NativeBootstrap::generate(
        TemplateVersion {
            id: "personal-default".into(),
            version: 1,
        },
        "https://manager.example.org/execution",
    )
    .unwrap();
    let owner = Identity {
        issuer: "https://sso.example.org/realms/agents".into(),
        subject: "Owner-A".into(),
    };
    let context_path = temporary.path().join("context.json");
    let seed = temporary.path().join("seed.json");
    let key = temporary.path().join("key");
    let result = temporary.path().join("result.json");
    std::fs::write(
        &context_path,
        serde_json::to_vec(&context(&material, &bootstrap, &owner).unwrap()).unwrap(),
    )
    .unwrap();
    std::fs::write(&seed, serde_json::to_vec(&bootstrap.native_config).unwrap()).unwrap();
    std::fs::write(&key, &bootstrap.initialization_key).unwrap();
    let program = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../runtimes/hermes/personal-init.py");
    let mut child = Command::new("python3")
        .arg(program)
        .arg("--root")
        .arg(&root)
        .arg("--context")
        .arg(context_path)
        .arg("--config")
        .arg(seed)
        .arg("--key")
        .arg(key)
        .arg("--result")
        .arg(&result)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("initializer deadline exceeded");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let receipt = std::fs::read_to_string(result).unwrap();
    verify(&material, &bootstrap, &owner, &receipt).unwrap();
    let mut foreign = owner.clone();
    foreign.subject = "owner-a".into();
    assert_eq!(
        verify(&material, &bootstrap, &foreign, &receipt),
        Err(Error::OwnershipConflict)
    );
    let mut other = material.clone();
    other.id = Uuid::new_v4();
    assert_eq!(
        verify(&other, &bootstrap, &owner, &receipt),
        Err(Error::OwnershipConflict)
    );
    let mut tampered: Value = serde_json::from_str(&receipt).unwrap();
    tampered["record"]["complete"] = json!(false);
    assert_eq!(
        verify(
            &material,
            &bootstrap,
            &owner,
            &serde_json::to_string(&tampered).unwrap()
        ),
        Err(Error::OwnershipConflict)
    );
    bootstrap.initialization_key = "b".repeat(64);
    assert_eq!(
        verify(&material, &bootstrap, &owner, &receipt),
        Err(Error::OwnershipConflict)
    );
    bootstrap.initialization_key = "invalid".into();
    assert_eq!(
        verify(&material, &bootstrap, &owner, &receipt),
        Err(Error::Invalid)
    );
}
