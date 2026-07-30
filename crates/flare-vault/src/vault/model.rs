use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VaultFile {
    pub version: u32,
    #[serde(with = "serde_bytes")]
    pub encrypted_dek: Vec<u8>,
    pub salt: Vec<u8>,
    #[serde(default)]
    pub body: VaultBody,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SecretEntry {
    #[serde(with = "serde_bytes")]
    pub value: Vec<u8>,
    pub added_at: DateTime<Utc>,
    #[serde(default)]
    pub rotated_at: Option<DateTime<Utc>>,
}

impl Drop for SecretEntry {
    fn drop(&mut self) {
        self.value.fill(0);
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct VaultBody(#[serde(with = "serde_map")] pub BTreeMap<String, SecretEntry>);

impl std::ops::Deref for VaultBody {
    type Target = BTreeMap<String, SecretEntry>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for VaultBody {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for VaultBody {
    fn drop(&mut self) {
        for entry in self.0.values_mut() {
            entry.value.fill(0);
        }
    }
}

impl VaultFile {
    pub fn new(encrypted_dek: Vec<u8>, salt: Vec<u8>) -> Self {
        Self {
            version: 1,
            encrypted_dek,
            salt,
            body: VaultBody::default(),
        }
    }
}

mod serde_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(data: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&hex::encode(data))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        hex::decode(&s).map_err(serde::de::Error::custom)
    }
}

mod serde_map {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S>(
        map: &BTreeMap<String, super::SecretEntry>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let flat: BTreeMap<&String, &super::SecretEntry> = map.iter().collect();
        flat.serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<String, super::SecretEntry>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let parsed: BTreeMap<String, super::SecretEntry> = BTreeMap::deserialize(deserializer)?;
        Ok(parsed)
    }
}
