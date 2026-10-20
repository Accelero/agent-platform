//! Semantic comparison only for reviewed Kubernetes quantity locations.
//! Frozen manifests and their authenticated digests remain byte-for-byte intact.
use serde_json::Value;

pub(super) fn equivalent(
    kind: Option<&str>,
    path: &[&str],
    actual: &Value,
    wanted: &Value,
) -> bool {
    let quantity_path = match (kind, path) {
        (
            Some("PersistentVolumeClaim"),
            ["spec", "resources", "requests" | "limits", "storage"],
        ) => true,
        (Some("Pod"), ["spec", rest @ ..])
        | (Some("Deployment"), ["spec", "template", "spec", rest @ ..]) => pod_quantity_path(rest),
        _ => false,
    };
    if !quantity_path {
        return false;
    }
    actual
        .as_str()
        .and_then(milli_units)
        .is_some_and(|actual| wanted.as_str().and_then(milli_units) == Some(actual))
}

fn pod_quantity_path(path: &[&str]) -> bool {
    matches!(
        path,
        [
            "containers" | "initContainers",
            "[]",
            "resources",
            "requests" | "limits",
            "cpu" | "memory"
        ] | ["volumes", "[]", "emptyDir", "sizeLimit"]
    )
}

/// The reviewed renderer emits nonnegative whole decimal/milli units or whole
/// binary Ki..Ei units. Kubernetes canonicalizes those within the same subset.
/// Other legal Quantity syntax (fractions, decimal exponents, SI suffixes) is
/// deliberately not interpreted here: differing unsupported strings fail closed.
/// Checked integer arithmetic avoids floating-point equality and overflow.
fn milli_units(value: &str) -> Option<u128> {
    if value.is_empty() || value.len() > 64 {
        return None;
    }
    let suffix_at = value
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(value.len());
    if suffix_at == 0 {
        return None;
    }
    let number = value[..suffix_at].parse::<u128>().ok()?;
    let multiplier = match &value[suffix_at..] {
        "m" => 1,
        "" => 1000,
        "Ki" => 1024_u128.checked_pow(1)?.checked_mul(1000)?,
        "Mi" => 1024_u128.checked_pow(2)?.checked_mul(1000)?,
        "Gi" => 1024_u128.checked_pow(3)?.checked_mul(1000)?,
        "Ti" => 1024_u128.checked_pow(4)?.checked_mul(1000)?,
        "Pi" => 1024_u128.checked_pow(5)?.checked_mul(1000)?,
        "Ei" => 1024_u128.checked_pow(6)?.checked_mul(1000)?,
        _ => return None,
    };
    number.checked_mul(multiplier)
}

#[cfg(test)]
mod tests {
    use super::super::{contains_fields, contains_pod_spec_fields};
    use serde_json::json;

    #[test]
    fn frozen_quantities_match_canonical_api_values_without_mutating_intent() {
        let wanted = json!({"kind":"Deployment","spec":{"template":{"spec":{"containers":[{"resources":{"requests":{"cpu":"1000m","memory":"1024Mi"},"limits":{"cpu":"1000m","memory":"1024Mi"}}}],"initContainers":[{"resources":{"requests":{"cpu":"1000m","memory":"1024Mi"}}}]}}}});
        let bytes = serde_json::to_vec(&wanted).unwrap();
        let mut actual = wanted.clone();
        for section in ["requests", "limits"] {
            actual["spec"]["template"]["spec"]["containers"][0]["resources"][section] =
                json!({"cpu":"1","memory":"1Gi"});
        }
        actual["spec"]["template"]["spec"]["initContainers"][0]["resources"]["requests"] =
            json!({"cpu":"1","memory":"1Gi"});
        assert!(contains_fields(&actual, &wanted));
        assert_eq!(serde_json::to_vec(&wanted).unwrap(), bytes);
        for (resource, changed) in [("cpu", "1001m"), ("memory", "2Gi")] {
            let mut changed_actual = actual.clone();
            changed_actual["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]
                [resource] = json!(changed);
            assert!(!contains_fields(&changed_actual, &wanted));
        }
        let wanted = json!({"kind":"PersistentVolumeClaim","spec":{"resources":{"requests":{"storage":"1024Gi"}}}});
        let actual = json!({"kind":"PersistentVolumeClaim","spec":{"resources":{"requests":{"storage":"1Ti"}}}});
        assert!(contains_fields(&actual, &wanted));
        let wanted = json!({"containers":[{"resources":{"limits":{"cpu":"1000m","memory":"1024Mi"}},"env":[{"name":"cpu","value":"1000m"}]}]});
        let mut actual = wanted.clone();
        actual["containers"][0]["resources"]["limits"] = json!({"cpu":"1","memory":"1Gi"});
        assert!(!contains_fields(&actual, &wanted));
        assert!(contains_pod_spec_fields(&actual, &wanted));
        actual["containers"][0]["env"][0]["value"] = json!("1");
        assert!(!contains_pod_spec_fields(&actual, &wanted));
    }

    #[test]
    fn quantity_locations_are_explicit_and_empty_selectors_remain_required() {
        for kind in ["Pod", "Deployment", "ConfigMap", "Secret"] {
            let wanted = json!({"kind":kind,"data":{"resources":{"requests":{"cpu":"1000m"}}}});
            let actual = json!({"kind":kind,"data":{"resources":{"requests":{"cpu":"1"}}}});
            assert!(!contains_fields(&actual, &wanted));
        }
        let wanted =
            json!({"kind":"Pod","spec":{"containers":[{"env":[{"name":"cpu","value":"1000m"}]}]}});
        let actual =
            json!({"kind":"Pod","spec":{"containers":[{"env":[{"name":"cpu","value":"1"}]}]}});
        assert!(!contains_fields(&actual, &wanted));
        let wanted = json!({"spec":{"resources":{"requests":{"cpu":"1000m"}}}});
        let actual = json!({"spec":{"resources":{"requests":{"cpu":"1"}}}});
        assert!(!contains_fields(&actual, &wanted));
        assert!(contains_fields(&json!({}), &json!({"ingress":[]})));
        assert!(!contains_fields(
            &json!({}),
            &json!({"namespaceSelector":{}})
        ));
        assert!(!contains_fields(
            &json!({"namespaceSelector":null}),
            &json!({"namespaceSelector":{}})
        ));
    }

    #[test]
    fn unsupported_or_overflowing_quantities_do_not_gain_semantic_equality() {
        let wanted =
            json!({"kind":"Pod","spec":{"containers":[{"resources":{"limits":{"cpu":"1000m"}}}]}});
        for value in [
            "1.0",
            "1e0",
            "-1",
            "+1",
            "1K",
            "340282366920938463463374607431768211455Ei",
            "1001m",
        ] {
            let actual = json!({"kind":"Pod","spec":{"containers":[{"resources":{"limits":{"cpu":value}}}]}});
            assert!(!contains_fields(&actual, &wanted));
        }
    }
}
