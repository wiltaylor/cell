// The hub API client. The admin token lives in localStorage.

import type { Backup, FileEntry, Game, HubInfo, LogPage, ServerInstance } from "./types";

export const tokenKey = "cell-hub-token";
export const token = () => localStorage.getItem(tokenKey) ?? "";
export const setToken = (t: string) => localStorage.setItem(tokenKey, t);

function headers(json = false): HeadersInit {
  const h: Record<string, string> = {};
  if (json) h["Content-Type"] = "application/json";
  const t = token();
  if (t) h["Authorization"] = `Bearer ${t}`;
  return h;
}

async function go<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch(path, init);
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
  games: () => go<Game[]>("/api/games"),
  game: (id: string) => go<Game>(`/api/games/${id}`),
  deleteGame: (id: string) => go(`/api/games/${id}`, { method: "DELETE", headers: headers() }),
  art: (id: string, kind: string) => `/api/games/${id}/art/${kind}`,
  download: (id: string) => `/api/games/${id}/download`,
  layer: (file: string) => `/api/layers/${file}`,
  servers: () => go<ServerInstance[]>("/api/servers"),
  server: (id: string) => go<ServerInstance>(`/api/servers/${id}`),
  createServer: (game_id: string, name: string) =>
    go<ServerInstance>("/api/servers", { method: "POST", headers: headers(true), body: JSON.stringify({ game_id, name }) }),
  serverAction: (id: string, action: "start" | "stop" | "restart") =>
    go<ServerInstance>(`/api/servers/${id}/${action}`, { method: "POST", headers: headers() }),
  deleteServer: (id: string) => go(`/api/servers/${id}`, { method: "DELETE", headers: headers() }),
  log: (id: string, after = 0) => go<LogPage>(`/api/servers/${id}/log?after=${after}`),
  input: (id: string, line: string) =>
    go(`/api/servers/${id}/input`, { method: "POST", headers: headers(true), body: JSON.stringify({ line }) }),
  files: (id: string, path = "") => go<FileEntry[]>(`/api/servers/${id}/files?path=${encodeURIComponent(path)}`),
  file: (id: string, path: string) => go<{ path: string; text: string }>(`/api/servers/${id}/file?path=${encodeURIComponent(path)}`),
  saveFile: (id: string, path: string, text: string) =>
    go(`/api/servers/${id}/file`, { method: "PUT", headers: headers(true), body: JSON.stringify({ path, text }) }),
  backups: (id: string) => go<Backup[]>(`/api/servers/${id}/backups`),
  backup: (id: string) => go<Backup>(`/api/servers/${id}/backups`, { method: "POST", headers: headers() }),
  restore: (id: string, name: string) => go(`/api/servers/${id}/backups/${name}/restore`, { method: "POST", headers: headers() }),
  deleteBackup: (id: string, name: string) => go(`/api/servers/${id}/backups/${name}`, { method: "DELETE", headers: headers() }),
  backupUrl: (id: string, name: string) => `/api/servers/${id}/backups/${name}`,
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
