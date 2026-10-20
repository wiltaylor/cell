// App-wide state: settings, the local library, art cache, downloads.
import { createResource, createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { GameInvite, LocalGame, Member, ProfileView, Progress, Settings } from "./types";

export const [settings, setSettings] = createSignal<Settings>({ games_dir: "", hub_url: "", hub_token: "" });
export async function loadSettings() {
  setSettings(await invoke<Settings>("get_settings"));
}
export async function saveSettings(s: Settings) {
  await invoke("save_settings", { settings: s });
  setSettings(s);
}

export const [library, { refetch: refetchLibrary }] = createResource(
  () => settings().games_dir,
  async (dir) => (dir ? await invoke<LocalGame[]>("scan_library", { dir }) : []),
);

const artCache = new Map<string, Promise<string | undefined>>();
export function art(path: string, kind: string): Promise<string | undefined> {
  const key = `${path}|${kind}`;
  if (!artCache.has(key)) {
    artCache.set(key, invoke<string>("game_art", { path, kind }).catch(() => undefined));
  }
  return artCache.get(key)!;
}

export const [downloads, setDownloads] = createSignal<Record<string, Progress>>({});
listen<Progress>("download", (ev) => {
  setDownloads((d) => ({ ...d, [ev.payload.id]: ev.payload }));
  if (ev.payload.finished && !ev.payload.error) refetchLibrary();
});

const [toast, setToast] = createSignal<{ text: string; err?: boolean } | null>(null);
let timer: number | undefined;
export { toast };
export function notify(text: string, err = false) {
  setToast({ text, err });
  clearTimeout(timer);
  timer = window.setTimeout(() => setToast(null), err ? 6000 : 3000);
}
export function fail(e: unknown) {
  notify(e instanceof Error ? e.message : String(e), true);
}

// ---- profile and hub session ----
export const [profile, { refetch: refetchProfile }] = createResource(() => invoke<ProfileView | null>("profile_get"));
export const [me, { refetch: refetchMe }] = createResource(
  () => [settings().hub_url, profile()] as const,
  async ([url, p]) => (url && p ? await invoke<Member | null>("hub_me", { url }).catch(() => null) : null),
);

// Presence heartbeat and invite inbox, while a hub session exists.
export const [inbox, setInbox] = createSignal<GameInvite[]>([]);
const seenInvites = new Set<string>();
export const [currentGame, setCurrentGame] = createSignal<string | null>(null);
setInterval(async () => {
  if (!me() || !settings().hub_url) return;
  const url = settings().hub_url;
  invoke("hub_presence", { url, status: currentGame() ? "in-game" : "online", gameId: currentGame() }).catch(() => {});
  try {
    const list = await invoke<GameInvite[]>("hub_inbox", { url });
    setInbox(list);
    for (const i of list) {
      if (!seenInvites.has(i.id)) {
        seenInvites.add(i.id);
        notify(`${i.from.name} invites you to ${i.server_name || i.game_name || "play"}`);
      }
    }
  } catch {}
}, 10000);
