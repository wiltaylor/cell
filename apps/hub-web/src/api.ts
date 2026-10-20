// The hub API client. The admin token lives in localStorage.

import type { Backup, FileEntry, Game, HubInfo, LogPage, ServerInstance } from "./types";

import { loadProfile, loginMessage, pairMessage, publicKey, sign, normalizeCode } from "@cell/ui/src/identity";
import type { Challenge, GameInvite, HubIdentity, InviteCode, Member, PairResponse, Peer, PeerCode, Session } from "./types";

export const tokenKey = "cell-hub-session";
export const token = () => localStorage.getItem(tokenKey) ?? "";
export const setToken = (t: string) => (t ? localStorage.setItem(tokenKey, t) : localStorage.removeItem(tokenKey));

/** Sign in with the browser profile. Returns null when there is no profile or it is not paired. */
export async function signIn(): Promise<Session | null> {
  const p = loadProfile();
  if (!p) return null;
  const hub = await api.identity();
  const pubkey = await publicKey(p);
  const chRes = await fetch("/api/auth/challenge", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ pubkey }) });
  if (!chRes.ok) return null;
  const ch = (await chRes.json()) as Challenge;
  const signature = await sign(p, loginMessage(hub.pubkey, ch.nonce));
  const res = await fetch("/api/auth/login", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ pubkey, nonce: ch.nonce, signature }) });
  if (!res.ok) return null;
  const session = (await res.json()) as Session;
  setToken(session.token);
  return session;
}

/** Pair the browser profile with this hub using an invite code. */
export async function pairWithHub(code: string): Promise<PairResponse> {
  const p = loadProfile();
  if (!p) throw new Error("make a profile first");
  const hub = await api.identity();
  const c = normalizeCode(code);
  const pubkey = await publicKey(p);
  const signature = await sign(p, pairMessage(hub.pubkey, c));
  const res = await fetch("/api/pair", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ code: c, pubkey, name: p.name, avatar: p.avatar ?? null, signature }) });
  if (!res.ok) throw new Error(((await res.json()) as { error: string }).error);
  const out = (await res.json()) as PairResponse;
  await signIn();
  return out;
}

function headers(json = false): HeadersInit {
  const h: Record<string, string> = {};
  if (json) h["Content-Type"] = "application/json";
  const t = token();
  if (t) h["Authorization"] = `Bearer ${t}`;
  return h;
}

let retried = false;
async function go<T>(path: string, init?: RequestInit): Promise<T> {
  let r = await fetch(path, init);
  if (r.status === 401 && !retried) {
    // The session expired or never existed: sign in once and retry.
    retried = true;
    const s = await signIn().catch(() => null);
    retried = false;
    if (s) r = await fetch(path, { ...init, headers: { ...(init?.headers as Record<string, string>), Authorization: `Bearer ${s.token}` } });
  }
  if (!r.ok) {
    let msg = `${r.status} ${r.statusText}`;
    try {
      const j = await r.json();
      if (j.error) msg = j.error;
    } catch {}
    throw new Error(msg);
  }
  return (await r.json()) as T;
}

export const api = {
  info: () => go<HubInfo>("/api/info"),
  identity: () => go<HubIdentity>("/api/identity"),
  me: () => go<Member>("/api/me", { headers: headers() }),
  logout: () => go("/api/auth/logout", { method: "POST", headers: headers() }).finally(() => setToken("")),
  members: () => go<Member[]>("/api/members", { headers: headers() }),
  updateMember: (pubkey: string, role?: string, servers?: string[]) =>
    go<Member>(`/api/members/${encodeURIComponent(pubkey)}`, { method: "PATCH", headers: headers(true), body: JSON.stringify({ role, servers }) }),
  removeMember: (pubkey: string) => go(`/api/members/${encodeURIComponent(pubkey)}`, { method: "DELETE", headers: headers() }),
  invites: () => go<InviteCode[]>("/api/invites", { headers: headers() }),
  createInvite: (role: string, servers: string[] = []) =>
    go<InviteCode>("/api/invites", { method: "POST", headers: headers(true), body: JSON.stringify({ role, servers }) }),
  presence: (status: string, game_id?: string) =>
    go("/api/presence", { method: "POST", headers: headers(true), body: JSON.stringify({ status, game_id: game_id ?? null }) }),
  peers: () => go<Peer[]>("/api/peers", { headers: headers() }),
  peerCode: () => go<PeerCode>("/api/peers/code", { method: "POST", headers: headers() }),
  pairPeer: (code: string) => go<Member>("/api/peers/pair", { method: "POST", headers: headers(true), body: JSON.stringify({ code }) }),
  removePeer: (pubkey: string) => go(`/api/peers/${encodeURIComponent(pubkey)}`, { method: "DELETE", headers: headers() }),
  sendInvite: (to: string, server_id: string | null, message: string) =>
    go("/api/peers/invite", { method: "POST", headers: headers(true), body: JSON.stringify({ to, server_id, message }) }),
  inbox: () => go<GameInvite[]>("/api/inbox", { headers: headers() }),
  dismiss: (id: string) => go(`/api/inbox/${id}`, { method: "DELETE", headers: headers() }),
  updateServer: (id: string, name?: string, join_args?: string[]) =>
    go<ServerInstance>(`/api/servers/${id}`, { method: "PATCH", headers: headers(true), body: JSON.stringify({ name, join_args }) }),
  games: () => go<Game[]>("/api/games"),
  game: (id: string) => go<Game>(`/api/games/${id}`),
  deleteGame: (id: string) => go(`/api/games/${id}`, { method: "DELETE", headers: headers() }),
  art: (id: string, kind: string) => `/api/games/${id}/art/${kind}`,
  download: (id: string) => `/api/games/${id}/download`,
  layer: (file: string) => `/api/layers/${file}`,
  servers: () => go<ServerInstance[]>("/api/servers", { headers: headers() }),
  server: (id: string) => go<ServerInstance>(`/api/servers/${id}`, { headers: headers() }),
  createServer: (game_id: string, name: string, join_args: string[] = []) =>
    go<ServerInstance>("/api/servers", { method: "POST", headers: headers(true), body: JSON.stringify({ game_id, name, join_args }) }),
  serverAction: (id: string, action: "start" | "stop" | "restart") =>
    go<ServerInstance>(`/api/servers/${id}/${action}`, { method: "POST", headers: headers() }),
  deleteServer: (id: string) => go(`/api/servers/${id}`, { method: "DELETE", headers: headers() }),
  log: (id: string, after = 0) => go<LogPage>(`/api/servers/${id}/log?after=${after}`, { headers: headers() }),
  input: (id: string, line: string) =>
    go(`/api/servers/${id}/input`, { method: "POST", headers: headers(true), body: JSON.stringify({ line }) }),
  files: (id: string, path = "") => go<FileEntry[]>(`/api/servers/${id}/files?path=${encodeURIComponent(path)}`, { headers: headers() }),
  file: (id: string, path: string) => go<{ path: string; text: string }>(`/api/servers/${id}/file?path=${encodeURIComponent(path)}`, { headers: headers() }),
  saveFile: (id: string, path: string, text: string) =>
    go(`/api/servers/${id}/file`, { method: "PUT", headers: headers(true), body: JSON.stringify({ path, text }) }),
  backups: (id: string) => go<Backup[]>(`/api/servers/${id}/backups`, { headers: headers() }),
  backup: (id: string) => go<Backup>(`/api/servers/${id}/backups`, { method: "POST", headers: headers() }),
  restore: (id: string, name: string) => go(`/api/servers/${id}/backups/${name}/restore`, { method: "POST", headers: headers() }),
  deleteBackup: (id: string, name: string) => go(`/api/servers/${id}/backups/${name}`, { method: "DELETE", headers: headers() }),
  backupUrl: (id: string, name: string) => `/api/servers/${id}/backups/${name}?token=${encodeURIComponent(token())}`,
  consoleUrl: (id: string) => {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const t = token();
    return `${proto}://${location.host}/api/servers/${id}/console${t ? `?token=${encodeURIComponent(t)}` : ""}`;
  },
};

/** Upload a file with progress. `path` is /api/games or /api/layers. */
export function upload(path: string, file: File, onProgress: (frac: number) => void): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", path);
    xhr.setRequestHeader("X-File-Name", file.name);
    const t = token();
    if (t) xhr.setRequestHeader("Authorization", `Bearer ${t}`);
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(e.loaded / e.total);
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) resolve(JSON.parse(xhr.responseText));
      else {
        let msg = `${xhr.status}`;
        try { msg = JSON.parse(xhr.responseText).error ?? msg; } catch {}
        reject(new Error(msg));
      }
    };
    xhr.onerror = () => reject(new Error("upload failed"));
    xhr.send(file);
  });
}
