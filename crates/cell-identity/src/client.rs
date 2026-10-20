//! A small blocking client for the pairing and login flow. Used by the
//! hub's own CLI and console; the launcher has an async twin.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{login_message, pair_message, Profile};

/// `~/.config/cell/profile.json`, or `$CELL_PROFILE`.
pub fn profile_path() -> PathBuf {
    if let Ok(p) = std::env::var("CELL_PROFILE") {
        return PathBuf::from(p);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".config")
        });
    base.join("cell/profile.json")
}

/// Sessions per hub URL, beside the profile.
pub fn sessions_path() -> PathBuf {
    profile_path().with_file_name("sessions.json")
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Sessions {
    pub by_hub: HashMap<String, String>,
}

impl Sessions {
    pub fn load() -> Self {
        std::fs::read(sessions_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) -> Result<()> {
        let p = sessions_path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, serde_json::to_vec_pretty(self)?)?;
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct HubIdentityLite {
    pub pubkey: String,
    pub name: String,
    pub fingerprint: String,
}

#[derive(Debug, Deserialize)]
struct ChallengeLite {
    nonce: String,
}

#[derive(Debug, Deserialize)]
pub struct SessionLite {
    pub token: String,
    pub expires_at: u64,
}

#[derive(Debug, Deserialize)]
struct ErrorLite {
    error: String,
}

fn fail(resp: reqwest::blocking::Response) -> anyhow::Error {
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    let msg = serde_json::from_str::<ErrorLite>(&text)
        .map(|e| e.error)
        .unwrap_or(text);
    anyhow::anyhow!("{status}: {msg}")
}

pub fn hub_identity(url: &str) -> Result<HubIdentityLite> {
    let resp = reqwest::blocking::get(format!("{}/api/identity", url.trim_end_matches('/')))
        .context("reach the hub")?;
    if !resp.status().is_success() {
        return Err(fail(resp));
    }
    Ok(resp.json()?)
}

/// Pair the profile with a hub using an invite code.
pub fn pair(url: &str, code: &str, profile: &Profile) -> Result<HubIdentityLite> {
    let url = url.trim_end_matches('/');
    let hub = hub_identity(url)?;
    let code = crate::normalize_code(code);
    let body = serde_json::json!({
        "code": code,
        "pubkey": profile.pubkey()?,
        "name": profile.name,
        "avatar": profile.avatar,
        "signature": profile.sign(&pair_message(&hub.pubkey, &code))?,
    });
    let resp = reqwest::blocking::Client::new()
        .post(format!("{url}/api/pair"))
        .json(&body)
        .send()?;
    if !resp.status().is_success() {
        return Err(fail(resp));
    }
    Ok(hub)
}

/// Sign in and return a session token.
pub fn login(url: &str, profile: &Profile) -> Result<SessionLite> {
    let url = url.trim_end_matches('/');
    let hub = hub_identity(url)?;
    let client = reqwest::blocking::Client::new();
    let pubkey = profile.pubkey()?;
    let resp = client
        .post(format!("{url}/api/auth/challenge"))
        .json(&serde_json::json!({ "pubkey": pubkey }))
        .send()?;
    if !resp.status().is_success() {
        return Err(fail(resp));
    }
    let ch: ChallengeLite = resp.json()?;
    let resp = client
        .post(format!("{url}/api/auth/login"))
        .json(&serde_json::json!({
            "pubkey": pubkey,
            "nonce": ch.nonce,
            "signature": profile.sign(&login_message(&hub.pubkey, &ch.nonce))?,
        }))
        .send()?;
    if !resp.status().is_success() {
        return Err(fail(resp));
    }
    Ok(resp.json()?)
}

/// A usable token for `url`: the cached session, or a fresh login.
pub fn token_for(url: &str, profile_file: &Path) -> Result<String> {
    let url = url.trim_end_matches('/').to_string();
    let mut sessions = Sessions::load();
    if let Some(t) = sessions.by_hub.get(&url) {
        let ok = reqwest::blocking::Client::new()
            .get(format!("{url}/api/me"))
            .bearer_auth(t)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if ok {
            return Ok(t.clone());
        }
    }
    if !profile_file.exists() {
        bail!(
            "no profile at {}. Make one with `cell-hub profile init --name <you>`",
            profile_file.display()
        );
    }
    let profile = Profile::load(profile_file)?;
    let s = login(&url, &profile)
        .context("sign in (is this profile paired with the hub? use `cell-hub pair`)")?;
    sessions.by_hub.insert(url, s.token.clone());
    sessions.save()?;
    Ok(s.token)
}
