// A browser profile: an Ed25519 keypair in WebCrypto, kept in localStorage
// as a JWK, plus the export format shared with the Rust side
// (PBKDF2-SHA256 + AES-256-GCM).

const KEY = "cell-profile";
const enc = new TextEncoder();

export interface StoredProfile { version: 1; name: string; avatar?: string; seed: string; created_at: number }

const b64 = (bytes: ArrayBuffer | Uint8Array) => btoa(String.fromCharCode(...new Uint8Array(bytes)));
const unb64 = (s: string) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
/** A standalone ArrayBuffer, which WebCrypto's typings want. */
const buf = (u8: Uint8Array): ArrayBuffer => u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength) as ArrayBuffer;
const b64url = (bytes: Uint8Array) => b64(bytes).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

export function loadProfile(): StoredProfile | null {
  try { const s = localStorage.getItem(KEY); return s ? (JSON.parse(s) as StoredProfile) : null; } catch { return null; }
}
export function clearProfile() { localStorage.removeItem(KEY); }

async function keysFromSeed(seed: Uint8Array): Promise<{ priv: CryptoKey; pub: Uint8Array }> {
  // Derive the public key by importing the seed as PKCS#8 and exporting the public half.
  const pkcs8 = new Uint8Array([0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20, ...seed]);
  const priv = await crypto.subtle.importKey("pkcs8", buf(pkcs8), { name: "Ed25519" }, true, ["sign"]);
  const jwk = await crypto.subtle.exportKey("jwk", priv);
  const pub = unb64((jwk.x as string).replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil((jwk.x as string).length / 4) * 4, "="));
  return { priv, pub };
}

export async function createProfile(name: string): Promise<StoredProfile> {
  const seed = crypto.getRandomValues(new Uint8Array(32));
  const p: StoredProfile = { version: 1, name, seed: b64(seed), created_at: Math.floor(Date.now() / 1000) };
  localStorage.setItem(KEY, JSON.stringify(p));
  return p;
}

export async function publicKey(p: StoredProfile): Promise<string> {
  const { pub } = await keysFromSeed(unb64(p.seed));
  return b64(pub);
}

export async function sign(p: StoredProfile, message: string): Promise<string> {
  const { priv } = await keysFromSeed(unb64(p.seed));
  const sig = await crypto.subtle.sign({ name: "Ed25519" }, priv, buf(enc.encode(message)));
  return b64(sig);
}

export function fingerprint(pubkeyB64: string): string {
  const bytes = unb64(pubkeyB64).slice(0, 10);
  const hex = Array.from(bytes).map((b) => b.toString(16).padStart(2, "0").toUpperCase()).join("");
  return hex.match(/.{1,4}/g)!.join("-");
}

export const pairMessage = (hubPubkey: string, code: string) => `cell-pair:v1:${hubPubkey}:${code}`;
export const loginMessage = (hubPubkey: string, nonce: string) => `cell-login:v1:${hubPubkey}:${nonce}`;
export const normalizeCode = (s: string) => {
  const raw = s.replace(/[^a-z0-9]/gi, "").toUpperCase();
  return raw.length === 8 ? `${raw.slice(0, 4)}-${raw.slice(4)}` : raw;
};

async function deriveKey(passphrase: string, salt: Uint8Array, iterations: number): Promise<CryptoKey> {
  const base = await crypto.subtle.importKey("raw", buf(enc.encode(passphrase)), "PBKDF2", false, ["deriveKey"]);
  return crypto.subtle.deriveKey({ name: "PBKDF2", hash: "SHA-256", salt: buf(salt), iterations }, base, { name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
}

/** The same file format `cell-hub profile export` writes. */
export async function exportProfile(p: StoredProfile, passphrase: string): Promise<string> {
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const iterations = 600000;
  const key = await deriveKey(passphrase, salt, iterations);
  const plain = enc.encode(JSON.stringify({ version: 1, seed: p.seed, name: p.name, avatar: p.avatar ?? null, created_at: p.created_at }));
  const ct = await crypto.subtle.encrypt({ name: "AES-GCM", iv: buf(nonce) }, key, buf(plain));
  return JSON.stringify({
    format: "cell-profile", version: 1, kdf: "pbkdf2-sha256", iterations, salt: b64(salt), cipher: "aes-256-gcm",
    nonce: b64(nonce), ciphertext: b64(ct), pubkey: await publicKey(p), name: p.name,
  }, null, 2);
}

export async function importProfile(text: string, passphrase: string): Promise<StoredProfile> {
  const bundle = JSON.parse(text);
  if (bundle.format !== "cell-profile") throw new Error("not a profile export");
  const key = await deriveKey(passphrase, unb64(bundle.salt), bundle.iterations);
  let plain: ArrayBuffer;
  try {
    plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv: buf(unb64(bundle.nonce)) }, key, buf(unb64(bundle.ciphertext)));
  } catch {
    throw new Error("wrong passphrase");
  }
  const inner = JSON.parse(new TextDecoder().decode(plain));
  const p: StoredProfile = { version: 1, name: inner.name, avatar: inner.avatar ?? undefined, seed: inner.seed, created_at: inner.created_at };
  if ((await publicKey(p)) !== bundle.pubkey) throw new Error("the export file is inconsistent");
  localStorage.setItem(KEY, JSON.stringify(p));
  return p;
}

export function download(name: string, text: string) {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(new Blob([text], { type: "application/json" }));
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}
export { b64url };
