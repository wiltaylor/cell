//! Profiles and pairing.
//!
//! A profile is an Ed25519 keypair with a display name. The public key is
//! the identity; there is no account server. A hub has a profile too.
//! Pairing exchanges public keys through a short invite code, and every
//! later call proves the key by signing a challenge.
//!
//! The export file is the only place a passphrase is used: PBKDF2-SHA256
//! derives an AES-256-GCM key, so a browser can read the same file with
//! WebCrypto.

pub mod client;

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{bail, Context, Result};
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};

pub const PBKDF2_ITERATIONS: u32 = 600_000;

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(s: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .context("invalid base64")
}

/// The part of a profile that is safe to show and store in clear.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicProfile {
    /// Base64 of the 32-byte Ed25519 public key.
    pub pubkey: String,
    pub name: String,
    /// Optional `data:image/png;base64,...` avatar.
    #[serde(default)]
    pub avatar: Option<String>,
}

/// A profile with its secret key. Stored as JSON in a 0600 file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub version: u32,
    /// Base64 of the 32-byte seed.
    pub seed: String,
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
    pub created_at: u64,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Profile {
    pub fn generate(name: &str) -> Self {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        Profile {
            version: 1,
            seed: b64(&seed),
            name: name.to_string(),
            avatar: None,
            created_at: now(),
        }
    }

    pub fn signing_key(&self) -> Result<SigningKey> {
        let seed = unb64(&self.seed)?;
        let seed: [u8; 32] = seed
            .try_into()
            .map_err(|_| anyhow::anyhow!("seed is not 32 bytes"))?;
        Ok(SigningKey::from_bytes(&seed))
    }

    pub fn pubkey(&self) -> Result<String> {
        Ok(b64(self.signing_key()?.verifying_key().as_bytes()))
    }

    pub fn public(&self) -> Result<PublicProfile> {
        Ok(PublicProfile {
            pubkey: self.pubkey()?,
            name: self.name.clone(),
            avatar: self.avatar.clone(),
        })
    }

    pub fn sign(&self, message: &str) -> Result<String> {
        let sig: Signature = self.signing_key()?.sign(message.as_bytes());
        Ok(b64(&sig.to_bytes()))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&bytes).context("parse profile")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let tmp = path.with_extension("json.partial");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// The passphrase-protected export file, as JSON text.
    pub fn export(&self, passphrase: &str) -> Result<String> {
        let mut salt = [0u8; 16];
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut salt);
        rand::thread_rng().fill_bytes(&mut nonce);
        let key = derive_key(passphrase, &salt, PBKDF2_ITERATIONS);
        let cipher = Aes256Gcm::new(&key.into());
        let plain = serde_json::to_vec(self)?;
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), plain.as_ref())
            .map_err(|_| anyhow::anyhow!("encrypt"))?;
        let bundle = ExportBundle {
            format: "cell-profile".into(),
            version: 1,
            kdf: "pbkdf2-sha256".into(),
            iterations: PBKDF2_ITERATIONS,
            salt: b64(&salt),
            cipher: "aes-256-gcm".into(),
            nonce: b64(&nonce),
            ciphertext: b64(&ciphertext),
            pubkey: self.pubkey()?,
            name: self.name.clone(),
        };
        Ok(serde_json::to_string_pretty(&bundle)?)
    }

    pub fn import(text: &str, passphrase: &str) -> Result<Self> {
        let bundle: ExportBundle = serde_json::from_str(text).context("not a profile export")?;
        if bundle.format != "cell-profile"
            || bundle.kdf != "pbkdf2-sha256"
            || bundle.cipher != "aes-256-gcm"
        {
            bail!("unsupported profile export format");
        }
        let salt = unb64(&bundle.salt)?;
        let nonce = unb64(&bundle.nonce)?;
        let ciphertext = unb64(&bundle.ciphertext)?;
        let key = derive_key(passphrase, &salt, bundle.iterations);
        let cipher = Aes256Gcm::new(&key.into());
        let plain = cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|_| anyhow::anyhow!("wrong passphrase"))?;
        let profile: Profile = serde_json::from_slice(&plain).context("decode profile")?;
        if profile.pubkey()? != bundle.pubkey {
            bail!("the export file is inconsistent");
        }
        Ok(profile)
    }
}

/// The export file. The clear fields let a UI show whose profile it is
/// before asking for the passphrase.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExportBundle {
    pub format: String,
    pub version: u32,
    pub kdf: String,
    pub iterations: u32,
    pub salt: String,
    pub cipher: String,
    pub nonce: String,
    pub ciphertext: String,
    pub pubkey: String,
    pub name: String,
}

fn derive_key(passphrase: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(passphrase.as_bytes(), salt, iterations, &mut key);
    key
}

/// Check a base64 signature over `message` by a base64 public key.
pub fn verify(pubkey_b64: &str, message: &str, signature_b64: &str) -> Result<()> {
    let pk = unb64(pubkey_b64)?;
    let pk: [u8; 32] = pk
        .try_into()
        .map_err(|_| anyhow::anyhow!("public key is not 32 bytes"))?;
    let vk = VerifyingKey::from_bytes(&pk).context("invalid public key")?;
    let sig = unb64(signature_b64)?;
    let sig = Signature::from_slice(&sig).context("invalid signature")?;
    vk.verify(message.as_bytes(), &sig)
        .map_err(|_| anyhow::anyhow!("signature does not match"))
}

/// `AB12-CD34-EF56-7890`: the first ten bytes of the public key as hex.
pub fn fingerprint(pubkey_b64: &str) -> String {
    let bytes = unb64(pubkey_b64).unwrap_or_default();
    let hex: String = bytes.iter().take(10).map(|b| format!("{b:02X}")).collect();
    hex.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).to_string())
        .collect::<Vec<_>>()
        .join("-")
}

/// A short code such as `K7F3-9Q2M`, from an alphabet without look-alikes.
pub fn invite_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    let mut s = String::new();
    for i in 0..8 {
        if i == 4 {
            s.push('-');
        }
        let idx = (rng.next_u32() as usize) % ALPHABET.len();
        s.push(ALPHABET[idx] as char);
    }
    s
}

/// Normalize a typed code: uppercase, no spaces, dash in the middle.
pub fn normalize_code(input: &str) -> String {
    let raw: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if raw.len() == 8 {
        format!("{}-{}", &raw[..4], &raw[4..])
    } else {
        raw
    }
}

/// The messages both sides sign. One place, so the Rust and TypeScript
/// clients agree byte for byte.
pub fn pair_message(hub_pubkey: &str, code: &str) -> String {
    format!("cell-pair:v1:{hub_pubkey}:{code}")
}

pub fn login_message(hub_pubkey: &str, nonce: &str) -> String {
    format!("cell-login:v1:{hub_pubkey}:{nonce}")
}

/// A random token or nonce, base64url without padding.
pub fn random_token() -> String {
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let p = Profile::generate("wil");
        let sig = p.sign("hello").unwrap();
        verify(&p.pubkey().unwrap(), "hello", &sig).unwrap();
        assert!(verify(&p.pubkey().unwrap(), "hellx", &sig).is_err());
    }

    #[test]
    fn export_round_trip() {
        let p = Profile::generate("wil");
        let text = p.export("correct horse").unwrap();
        let back = Profile::import(&text, "correct horse").unwrap();
        assert_eq!(back.seed, p.seed);
        assert!(Profile::import(&text, "wrong").is_err());
    }

    #[test]
    fn codes() {
        let c = invite_code();
        assert_eq!(c.len(), 9);
        assert_eq!(normalize_code(" k7f3 9q2m "), "K7F3-9Q2M");
        assert_eq!(
            fingerprint(&Profile::generate("x").pubkey().unwrap()).len(),
            24
        );
    }
}
