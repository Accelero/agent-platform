//! Strict JSON: duplicate object keys are refused at every depth, and numbers
//! a JavaScript client could not represent exactly are refused. Only then is
//! the document converted to the typed request, which itself refuses unknown
//! fields.

use serde::de::{self, DeserializeOwned, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

/// Largest integer every JSON client represents exactly (2^53 − 1).
const SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Why a body was refused. Never carries the submitted content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictError {
    Malformed,
    DuplicateKey,
    UnsafeNumber,
    Shape,
}

struct Strict(Value);

impl<'de> de::Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Strict;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("unambiguous JSON")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Strict, E> {
        Ok(Strict(Value::Bool(value)))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Strict, E> {
        if (value as f64).abs() > SAFE_INTEGER {
            return Err(E::custom("unsafe number"));
        }
        Ok(Strict(value.into()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Strict, E> {
        if value as f64 > SAFE_INTEGER {
            return Err(E::custom("unsafe number"));
        }
        Ok(Strict(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Strict, E> {
        if !value.is_finite() || value.abs() > SAFE_INTEGER {
            return Err(E::custom("unsafe number"));
        }
        serde_json::Number::from_f64(value)
            .map(|number| Strict(Value::Number(number)))
            .ok_or_else(|| E::custom("unsafe number"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Strict, E> {
        Ok(Strict(Value::String(value.to_owned())))
    }

    fn visit_none<E: de::Error>(self) -> Result<Strict, E> {
        Ok(Strict(Value::Null))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Strict, E> {
        Ok(Strict(Value::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Strict, A::Error> {
        let mut values = Vec::new();
        while let Some(Strict(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(Strict(Value::Array(values)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Strict, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom("duplicate key"));
            }
            let Strict(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(Strict(Value::Object(object)))
    }
}

/// Parses a request body into `T` under the strict rules.
pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, StrictError> {
    let Strict(value) = serde_json::from_slice(bytes).map_err(|error| {
        let message = error.to_string();
        if message.contains("duplicate key") {
            StrictError::DuplicateKey
        } else if message.contains("unsafe number") {
            StrictError::UnsafeNumber
        } else {
            StrictError::Malformed
        }
    })?;
    serde_json::from_value(value).map_err(|_| StrictError::Shape)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_keys_are_refused_at_every_depth_and_unsafe_numbers_too() {
        assert_eq!(
            from_slice::<Value>(br#"{"a":1,"a":2}"#),
            Err(StrictError::DuplicateKey)
        );
        assert_eq!(
            from_slice::<Value>(br#"{"x":{"a":1,"a":2}}"#),
            Err(StrictError::DuplicateKey)
        );
        assert_eq!(
            from_slice::<Value>(br#"[{"a":[{"b":1,"b":1}]}]"#),
            Err(StrictError::DuplicateKey)
        );
        assert_eq!(
            from_slice::<Value>(br#"{"large":1e40}"#),
            Err(StrictError::UnsafeNumber)
        );
        assert_eq!(
            from_slice::<Value>(br#"{"large":9007199254740993}"#),
            Err(StrictError::UnsafeNumber)
        );
        assert_eq!(
            from_slice::<Value>(br#"{"ok":9007199254740991,"f":0.5}"#).map(|_| ()),
            Ok(())
        );
        assert_eq!(from_slice::<Value>(b"{"), Err(StrictError::Malformed));
        #[derive(serde::Deserialize, Debug)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct Typed {
            a: u8,
        }
        assert_eq!(
            from_slice::<Typed>(br#"{"a":1,"b":2}"#).map(|_| ()),
            Err(StrictError::Shape)
        );
    }
}
