//! A native OCI registry client for `oci://` bases.
//!
//! The client pulls an image without Docker: it resolves the reference,
//! gets an anonymous bearer token when the registry asks for one, picks the
//! manifest for the plan's architecture, downloads each layer blob into a
//! content-addressed cache, and applies the layers in order into a rootfs
//! directory. The rootfs cache uses the manifest digest as its key, so a
//! moved tag causes one new extraction and nothing else.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::unpack;

const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json, \
application/vnd.oci.image.manifest.v1+json, \
application/vnd.docker.distribution.manifest.v2+json";

const USER_AGENT: &str = "cell/0.1 (+https://github.com/wiltaylor)";

fn status(msg: &str) {
    eprintln!("cell: {msg}");
}

// ---- references ------------------------------------------------------------

/// One parsed image reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The host the client talks to, such as `registry-1.docker.io`.
    pub registry: String,
    /// The repository path, such as `library/alpine`.
    pub repo: String,
    /// A tag or a `sha256:` digest.
    pub reference: String,
}

impl Reference {
    /// Parse `name[:tag][@digest]` with an optional registry host in front.
    pub fn parse(image: &str) -> Result<Reference> {
        let image = image.trim();
        if image.is_empty() {
            bail!("empty image reference");
        }
        let (name, digest) = match image.split_once('@') {
            Some((n, d)) => (n, Some(d)),
            None => (image, None),
        };
        // A tag follows the last `:` only when no `/` comes after it.
        let (name, tag) = match name.rsplit_once(':') {
            Some((n, t)) if !t.contains('/') => (n, Some(t)),
            _ => (name, None),
        };
        let (host, path) = match name.split_once('/') {
            Some((h, p)) if h.contains('.') || h.contains(':') || h == "localhost" => {
                (h.to_string(), p.to_string())
            }
            _ => ("docker.io".to_string(), name.to_string()),
        };
        if path.is_empty() {
            bail!("no repository in image reference `{image}`");
        }
        let (registry, repo) = if host == "docker.io" || host == "index.docker.io" {
            let repo = if path.contains('/') { path } else { format!("library/{path}") };
            ("registry-1.docker.io".to_string(), repo)
        } else {
            (host, path)
        };
        let reference = match digest {
            Some(d) => {
                if !d.starts_with("sha256:") || d.len() != 71 {
                    bail!("unsupported digest `{d}` (sha256 only)");
                }
                d.to_string()
            }
            None => tag.unwrap_or("latest").to_string(),
        };
        Ok(Reference { registry, repo, reference })
    }

    fn url(&self, kind: &str, item: &str) -> String {
        format!("https://{}/v2/{}/{}/{}", self.registry, self.repo, kind, item)
    }

    /// The reference as a readable string.
    pub fn display(&self) -> String {
        let sep = if self.reference.starts_with("sha256:") { "@" } else { ":" };
        format!("{}/{}{}{}", self.registry, self.repo, sep, self.reference)
    }
}

/// Map a plan architecture to the OCI platform architecture.
pub fn oci_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

// ---- the registry client ---------------------------------------------------

/// One layer of the resolved image.
#[derive(Debug, Clone)]
pub struct LayerDesc {
    pub digest: String,
    pub media_type: String,
    pub size: u64,
}

/// The image after manifest resolution.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The digest of the platform manifest (`sha256:<hex>`).
    pub digest: String,
    pub layers: Vec<LayerDesc>,
}

/// A registry client with one anonymous bearer token.
pub struct Client {
    agent: ureq::Agent,
    token: Option<String>,
}

type Response = ureq::http::Response<ureq::Body>;

impl Client {
    pub fn new() -> Client {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .timeout_connect(Some(Duration::from_secs(30)))
            .build();
        Client {
            agent: ureq::Agent::new_with_config(config),
            token: None,
        }
    }

    fn get_once(&self, url: &str, accept: Option<&str>) -> Result<Response> {
        let mut req = self.agent.get(url);
        if let Some(a) = accept {
            req = req.header("Accept", a);
        }
        if let Some(t) = &self.token {
            req = req.header("Authorization", &format!("Bearer {t}"));
        }
        req.call().with_context(|| format!("GET {url}"))
    }

    /// GET `url`. Get a token on 401. Wait and try again on 429 and 5xx.
    fn get(&mut self, url: &str, accept: Option<&str>) -> Result<Response> {
        let mut attempt = 0u32;
        loop {
            let resp = self.get_once(url, accept)?;
            let code = resp.status().as_u16();
            if code == 401 && self.token.is_none() {
                let challenge = resp
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
                    .ok_or_else(|| anyhow!("401 without a WWW-Authenticate header from {url}"))?;
                self.token = Some(self.fetch_token(&challenge)?);
                continue;
            }
            if (code == 429 || code >= 500) && attempt < 4 {
                attempt += 1;
                let wait = 5 * (1 << attempt);
                status(&format!("registry answered {code}; waiting {wait}s before retry"));
                std::thread::sleep(Duration::from_secs(wait));
                continue;
            }
            if code >= 400 {
                bail!("GET {url}: HTTP {code}");
            }
            return Ok(resp);
        }
    }

    /// Get an anonymous token from the realm in a `Bearer` challenge.
    fn fetch_token(&self, challenge: &str) -> Result<String> {
        let Some(params) = challenge.strip_prefix("Bearer ") else {
            bail!("unsupported auth challenge: {challenge}");
        };
        let mut realm = None;
        let mut query = Vec::new();
        for part in split_challenge(params) {
            let Some((k, v)) = part.split_once('=') else { continue };
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "realm" => realm = Some(v.to_string()),
                "service" | "scope" => query.push(format!("{}={}", k.trim(), url_encode(v))),
                _ => {}
            }
        }
        let realm = realm.ok_or_else(|| anyhow!("auth challenge without realm: {challenge}"))?;
        let sep = if realm.contains('?') { '&' } else { '?' };
        let url = format!("{realm}{sep}{}", query.join("&"));
        let mut resp = self.get_once(&url, None)?;
        if resp.status().as_u16() >= 400 {
            bail!("token request to {realm}: HTTP {}", resp.status().as_u16());
        }
        let text = resp.body_mut().read_to_string()?;
        let v: Value = serde_json::from_str(&text).context("parse token response")?;
        v.get("token")
            .or_else(|| v.get("access_token"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("token response without a token"))
    }

    /// GET a manifest. Return its digest, media type and body.
    fn manifest(&mut self, r: &Reference, item: &str) -> Result<(String, String, Vec<u8>)> {
        let url = r.url("manifests", item);
        let mut resp = self.get(&url, Some(ACCEPT))?;
        let media_type = header(&resp, "content-type").unwrap_or_default();
        let media_type = media_type.split(';').next().unwrap_or("").trim().to_string();
        let body = resp.body_mut().with_config().limit(32 << 20).read_to_vec()?;
        let computed = format!("sha256:{:x}", Sha256::digest(&body));
        if item.starts_with("sha256:") && computed != item {
            bail!("manifest {item} has digest {computed}");
        }
        let digest = header(&resp, "docker-content-digest").unwrap_or(computed);
        Ok((digest, media_type, body))
    }

    /// Resolve `r` to the manifest for `arch` (a plan architecture).
    pub fn resolve(&mut self, r: &Reference, arch: &str) -> Result<Resolved> {
        let (digest, media_type, body) = self.manifest(r, &r.reference)?;
        let doc: Value = serde_json::from_slice(&body).context("parse manifest")?;
        let media_type = doc
            .get("mediaType")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or(media_type);
        let (digest, doc) = if is_index(&media_type) {
            let want = oci_arch(arch);
            let entry = pick_platform(&doc, want)
                .ok_or_else(|| anyhow!("{} has no linux/{want} manifest", r.display()))?;
            let (d, _, body) = self.manifest(r, &entry)?;
            let doc: Value = serde_json::from_slice(&body).context("parse platform manifest")?;
            (d, doc)
        } else if is_manifest(&media_type) {
            (digest, doc)
        } else {
            bail!("unsupported manifest type `{media_type}` for {}", r.display());
        };
        let layers = doc
            .get("layers")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("manifest without layers"))?
            .iter()
            .map(|l| {
                Ok(LayerDesc {
                    digest: l
                        .get("digest")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("layer without digest"))?
                        .to_string(),
                    media_type: l
                        .get("mediaType")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    size: l.get("size").and_then(Value::as_u64).unwrap_or(0),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Resolved { digest, layers })
    }

    /// Download one blob into `dest` and check its sha256 against `digest`.
    pub fn fetch_blob(&mut self, r: &Reference, digest: &str, dest: &Path) -> Result<u64> {
        let url = r.url("blobs", digest);
        let resp = self.get(&url, None)?;
        let partial = dest.with_extension("partial");
        let mut out = fs::File::create(&partial)?;
        let mut hasher = Sha256::new();
        let mut reader = resp.into_body().into_reader();
        let mut buf = vec![0u8; 1 << 20];
        let mut total = 0u64;
        loop {
            let n = reader.read(&mut buf).with_context(|| format!("read {url}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            total += n as u64;
        }
        out.flush()?;
        drop(out);
        let got = format!("sha256:{:x}", hasher.finalize());
        if got != digest {
            let _ = fs::remove_file(&partial);
            bail!("blob {digest} downloaded with digest {got}");
        }
        fs::rename(&partial, dest)?;
        Ok(total)
    }
}

impl Default for Client {
    fn default() -> Self {
        Client::new()
    }
}

fn header(resp: &Response, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn is_index(media_type: &str) -> bool {
    matches!(
        media_type,
        "application/vnd.docker.distribution.manifest.list.v2+json"
            | "application/vnd.oci.image.index.v1+json"
    )
}

fn is_manifest(media_type: &str) -> bool {
    matches!(
        media_type,
        "application/vnd.docker.distribution.manifest.v2+json"
            | "application/vnd.oci.image.manifest.v1+json"
    )
}

/// Pick the digest of the `linux/<arch>` entry from an image index.
fn pick_platform(index: &Value, arch: &str) -> Option<String> {
    let list = index.get("manifests")?.as_array()?;
    let candidates: Vec<&Value> = list
        .iter()
        .filter(|m| {
            let p = m.get("platform");
            let os = p.and_then(|p| p.get("os")).and_then(Value::as_str);
            let a = p.and_then(|p| p.get("architecture")).and_then(Value::as_str);
            os == Some("linux") && a == Some(arch)
        })
        .collect();
    // Attestation manifests carry `unknown/unknown`, so they never match.
    // Prefer the entry without a variant, then `v8` for arm64.
    let variant = |m: &&Value| -> String {
        m.get("platform")
            .and_then(|p| p.get("variant"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let best = candidates
        .iter()
        .find(|m| variant(m).is_empty())
        .or_else(|| candidates.iter().find(|m| variant(m) == "v8"))
        .or_else(|| candidates.first())?;
    best.get("digest").and_then(Value::as_str).map(str::to_string)
}

/// Split the parameters of an auth challenge at commas outside quotes.
fn split_challenge(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' if !quoted => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn mib(n: u64) -> String {
    format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
}

// ---- the rootfs cache ------------------------------------------------------

fn cache_dir() -> PathBuf {
    cell_core::host::cache_home().join("oci")
}

fn ref_key(image: &str, arch: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(image.as_bytes());
    h.update(b"\0");
    h.update(arch.as_bytes());
    h.finalize().to_hex()[..24].to_string()
}

/// Return the rootfs directory for `image` on `arch`, and the manifest
/// digest that identifies it. `no_cache` forces a new extraction.
pub fn rootfs(image: &str, arch: &str, no_cache: bool) -> Result<(PathBuf, String)> {
    let r = Reference::parse(image)?;
    let cache = cache_dir();
    let refs = cache.join("refs");
    let blobs = cache.join("blobs").join("sha256");
    fs::create_dir_all(&refs)?;
    fs::create_dir_all(&blobs)?;
    let ref_file = refs.join(ref_key(image, arch));

    let mut client = Client::new();
    status(&format!("base oci://{image}: resolving {}", r.display()));
    let resolved = match client.resolve(&r, arch) {
        Ok(res) => {
            let _ = fs::write(&ref_file, &res.digest);
            res
        }
        Err(e) => {
            // Without a network, a rootfs that this reference gave before
            // still serves.
            let last = fs::read_to_string(&ref_file).ok();
            let hex = last.as_deref().and_then(|d| d.strip_prefix("sha256:")).map(str::to_string);
            match hex {
                Some(hex) if cache.join("rootfs").join(&hex).join("ok").exists() && !no_cache => {
                    status(&format!("warning: {e:#}; using the last rootfs for this reference"));
                    return Ok((cache.join("rootfs").join(&hex).join("rootfs"), format!("sha256:{hex}")));
                }
                _ => return Err(e),
            }
        }
    };
    let hex = resolved
        .digest
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("unsupported manifest digest {}", resolved.digest))?
        .to_string();
    let dir = cache.join("rootfs").join(&hex);
    let rootfs = dir.join("rootfs");
    let stamp = dir.join("ok");
    if stamp.exists() && !no_cache {
        status(&format!("base oci://{image}: cached ({})", &resolved.digest[..19]));
        return Ok((rootfs, resolved.digest));
    }
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&rootfs)?;

    let n = resolved.layers.len();
    for (i, layer) in resolved.layers.iter().enumerate() {
        let Some(layer_hex) = layer.digest.strip_prefix("sha256:") else {
            bail!("unsupported layer digest {}", layer.digest);
        };
        let blob = blobs.join(layer_hex);
        let short = &layer.digest[7..19];
        if blob.exists() {
            status(&format!("layer {}/{n} {short} {}: cached", i + 1, mib(layer.size)));
        } else {
            status(&format!("layer {}/{n} {short} {}: downloading", i + 1, mib(layer.size)));
            client.fetch_blob(&r, &layer.digest, &blob)?;
        }
        status(&format!("layer {}/{n} {short}: applying", i + 1));
        let reader = unpack::open_compressed(&blob, &layer.media_type)?;
        let opts = unpack::Options { strip_root_dir: false, whiteouts: true };
        unpack::unpack_tar(reader, &rootfs, &opts)
            .with_context(|| format!("apply layer {}", layer.digest))?;
    }
    fs::write(&stamp, b"ok")?;
    Ok((rootfs, resolved.digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(s: &str) -> (String, String, String) {
        let r = Reference::parse(s).unwrap();
        (r.registry, r.repo, r.reference)
    }

    #[test]
    fn parse_references() {
        assert_eq!(
            parsed("alpine:3.22"),
            ("registry-1.docker.io".into(), "library/alpine".into(), "3.22".into())
        );
        assert_eq!(
            parsed("archlinux"),
            ("registry-1.docker.io".into(), "library/archlinux".into(), "latest".into())
        );
        assert_eq!(
            parsed("docker.io/library/archlinux:base"),
            ("registry-1.docker.io".into(), "library/archlinux".into(), "base".into())
        );
        assert_eq!(
            parsed("ghcr.io/owner/image:tag"),
            ("ghcr.io".into(), "owner/image".into(), "tag".into())
        );
        let d = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(
            parsed(&format!("quay.io/x/y@{d}")),
            ("quay.io".into(), "x/y".into(), d.into())
        );
        assert_eq!(
            parsed("localhost:5000/foo"),
            ("localhost:5000".into(), "foo".into(), "latest".into())
        );
    }

    #[test]
    fn challenge_split() {
        let parts = split_challenge(
            r#"realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/alpine:pull,push""#,
        );
        assert_eq!(parts.len(), 3);
        assert!(parts[2].ends_with(r#"pull,push""#));
    }

    #[test]
    fn platform_pick() {
        let idx: Value = serde_json::json!({
            "manifests": [
                {"digest": "sha256:a", "platform": {"os": "linux", "architecture": "arm64", "variant": "v8"}},
                {"digest": "sha256:b", "platform": {"os": "linux", "architecture": "amd64"}},
                {"digest": "sha256:c", "platform": {"os": "unknown", "architecture": "unknown"}}
            ]
        });
        assert_eq!(pick_platform(&idx, "amd64").as_deref(), Some("sha256:b"));
        assert_eq!(pick_platform(&idx, "arm64").as_deref(), Some("sha256:a"));
        assert_eq!(pick_platform(&idx, "riscv64"), None);
    }
}
