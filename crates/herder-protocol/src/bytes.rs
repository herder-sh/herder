//! Raw bytes carried as base64 text.

use std::borrow::Cow;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Raw bytes, encoded on the wire as a standard padded base64 string.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <Cow<'de, str>>::deserialize(deserializer)?;
        STANDARD
            .decode(text.as_bytes())
            .map(Bytes)
            .map_err(de::Error::custom)
    }
}

impl JsonSchema for Bytes {
    fn schema_name() -> Cow<'static, str> {
        "Bytes".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Raw bytes, encoded as a standard padded base64 string.",
            "type": "string",
            "contentEncoding": "base64"
        })
    }
}
