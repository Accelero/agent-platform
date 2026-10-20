use super::*;
use serde_json::{Value, json};

fn explicit_config() -> Value {
    json!({
        "apiVersion":"v1","kind":"Config",
        "current-context":"unselected-workstation-context",
        "contexts":[{"name":"controller","context":{"cluster":"test","user":"controller"}}],
        "clusters":[{"name":"test","cluster":{"server":"https://127.0.0.1:1"}}],
        "users":[{"name":"controller","user":{"token":"test-only-controller-token"}}]
    })
}

#[test]
fn explicit_context_and_credential_references_cannot_fall_back() {
    let config: Kubeconfig = serde_json::from_value(explicit_config()).unwrap();
    assert!(validate_kubeconfig(&config, "controller").is_ok());
    assert_eq!(validate_kubeconfig(&config, "missing"), Err(Error::Invalid));
    let mut relative = explicit_config();
    relative["users"][0]["user"]["tokenFile"] = json!("relative-token-file");
    assert_eq!(
        validate_kubeconfig(&serde_json::from_value(relative).unwrap(), "controller"),
        Err(Error::Invalid)
    );
    let mut duplicate = explicit_config();
    let first = duplicate["contexts"][0].clone();
    duplicate["contexts"].as_array_mut().unwrap().push(first);
    assert_eq!(
        validate_kubeconfig(&serde_json::from_value(duplicate).unwrap(), "controller"),
        Err(Error::Invalid)
    );
}

#[tokio::test]
async fn executable_kubeconfig_is_rejected_without_running_its_command() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("auth-plugin-was-run");
    let path = directory.path().join("config.json");
    let mut config = explicit_config();
    config["users"][0]["user"]["exec"] = json!({
        "apiVersion":"client.authentication.k8s.io/v1",
        "command":"touch", "args":[marker], "interactiveMode":"Never"
    });
    tokio::fs::write(&path, serde_json::to_vec(&config).unwrap())
        .await
        .unwrap();
    let signer = OwnershipSigner::new(
        "test".into(),
        BTreeMap::from([("test".into(), vec![7; 32])]),
    )
    .unwrap();
    let result = Kubernetes::Kubeconfig {
        file: path,
        context: "controller".into(),
    }
    .driver(signer, "test-cluster-uid".into())
    .await;
    assert!(matches!(result, Err(Error::Invalid)));
    assert!(!marker.exists());
}

#[tokio::test]
async fn keyring_load_bounds_and_validates_referenced_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let key = directory.path().join("material-key");
    let keyring = directory.path().join("keyring.json");
    tokio::fs::write(
        &keyring,
        serde_json::to_vec(&json!({
            "activeKeyId":"current", "keyFiles":{"current":key}
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    for length in [31, 33] {
        tokio::fs::write(&key, vec![9; length]).await.unwrap();
        assert!(matches!(Keyring::load(&keyring).await, Err(Error::Invalid)));
    }
    tokio::fs::write(&key, vec![9; 32]).await.unwrap();
    let (active, keys) = Keyring::load(&keyring).await.unwrap();
    assert_eq!(active, "current");
    assert_eq!(keys["current"].len(), 32);
}

#[test]
fn kubernetes_source_must_be_explicit_and_does_not_accept_secret_inline() {
    for config in [
        json!({}),
        json!({"source":"infer"}),
        json!({"source":"inCluster", "token":"inline-secret"}),
        json!({"source":"kubeconfig","file":"/mounted/config"}),
    ] {
        assert!(serde_json::from_value::<Kubernetes>(config).is_err());
    }
}
