//! Salted Argon2id password verification. Plaintext passwords are never serialized.
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Verifier {
    version: u8,
    salt: [u8; 16],
    hash: [u8; 32],
}

impl Verifier {
    pub(super) fn new(password: &str) -> anyhow::Result<Self> {
        let mut salt = [0; 16];
        rand::rng().fill_bytes(&mut salt);
        Ok(Self {
            version: 1,
            salt,
            hash: *derive(password, &salt)?,
        })
    }

    pub(super) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.version == 1, "unsupported app lock version");
        Ok(())
    }

    pub(super) fn verify(&self, password: &str) -> anyhow::Result<bool> {
        self.validate()?;
        Ok(bool::from(
            self.hash.ct_eq(derive(password, &self.salt)?.as_ref()),
        ))
    }
}

fn derive(password: &str, salt: &[u8; 16]) -> anyhow::Result<Zeroizing<[u8; 32]>> {
    let params = Params::new(64 * 1024, 3, 1, Some(32)).map_err(|e| anyhow::anyhow!(e))?;
    let mut hash = Zeroizing::new([0; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, hash.as_mut())
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::Verifier;

    #[test]
    fn passwords_are_salted_and_round_trip_without_plaintext() {
        let first = Verifier::new("a long 密码 123").unwrap();
        let second = Verifier::new("a long 密码 123").unwrap();
        assert_ne!(first.salt, second.salt);
        assert_ne!(first.hash, second.hash);
        let json = serde_json::to_string(&first).unwrap();
        assert!(!json.contains("a long 密码 123"));
        let loaded: Verifier = serde_json::from_str(&json).unwrap();
        assert!(loaded.verify("a long 密码 123").unwrap());
        assert!(!loaded.verify("wrong password").unwrap());
        assert!(!loaded.verify("a long 密码 123 ").unwrap());
    }

    #[test]
    fn invalid_or_unknown_verifiers_are_rejected() {
        assert!(serde_json::from_str::<Verifier>("broken").is_err());
        let mut verifier = Verifier::new("password123").unwrap();
        verifier.version = 2;
        assert!(verifier.validate().is_err());
    }
}
