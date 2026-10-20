import { createResource, createSignal, For, Show } from "solid-js";
import { A } from "@solidjs/router";
import { Badge, Button, Cover, fmtBytes } from "@cell/ui";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { downloads, fail, library, notify, refetchLibrary, settings } from "../store";
import type { HubGame } from "../types";

export function Hub() {
  const [games, { refetch }] = createResource(() => settings().hub_url, async (url) => (url ? await invoke<HubGame[]>("hub_games", { url }) : []));
  const [busy, setBusy] = createSignal<string | null>(null);
  const installed = (id: string) => library()?.some((g) => g.id === id);
  const artUrl = (g: HubGame, kind: string) => (g.art.includes(kind) ? `${settings().hub_url.replace(/\/$/, "")}/api/games/${g.id}/art/${kind}` : undefined);
  const total = (g: HubGame) => g.size + g.layers.filter((l) => l.placement !== "embedded").reduce((a, l) => a + l.size, 0);

  const download = async (g: HubGame) => {
    setBusy(g.id);
    try {
      await invoke("hub_download", { url: settings().hub_url, id: g.id, gamesDir: settings().games_dir });
      notify(`${g.name} installed`);
      refetchLibrary();
    } catch (e) { fail(e); } finally { setBusy(null); }
  };
  const uploadLocal = async () => {
    const picked = await open({ multiple: false, filters: [{ name: "Cell", extensions: ["cell"] }] });
    if (!picked) return;
    setBusy("upload");
    try {
      const g = await invoke<HubGame>("hub_upload", { url: settings().hub_url, token: settings().hub_token, path: picked as string });
      notify(`uploaded ${g.name}`);
      refetch();
    } catch (e) { fail(e); } finally { setBusy(null); }
  };

  return (
    <>
      <div class="row" style="justify-content:space-between">
        <div><h1>Hub</h1><p class="muted">{settings().hub_url || "No hub configured."} <A href="/settings">Change</A></p></div>
        <div class="btn-row">
          <Button variant="ghost" onClick={() => refetch()}>Refresh</Button>
          <Button variant="accent" onClick={uploadLocal} disabled={busy() !== null}>Upload a cell…</Button>
        </div>
      </div>
      <Show when={games.error}><div class="empty">Cannot reach the hub: {String(games.error)}</div></Show>
      <Show when={games()}>
        <Show when={games()!.length > 0} fallback={<div class="empty">The hub has no games yet.</div>}>
          <div class="grid">
            <For each={games()}>
              {(g) => {
                const p = () => downloads()[g.id];
                return (
                  <div class="card" style="cursor:default">
                    <Cover src={artUrl(g, "portrait")} icon={artUrl(g, "icon")} name={g.name} />
                    <Show when={g.has_server}><span class="badge on">Server</span></Show>
                    <div class="meta">
                      <div class="name">{g.name}</div>
                      <div class="sub">v{g.version} · {fmtBytes(total(g))}</div>
                      <Show when={p() && !p()!.finished}>
                        <div class="progress" style="margin-top:8px"><div style={{ width: `${Math.round((p()!.done / Math.max(1, p()!.total)) * 100)}%` }} /></div>
                        <div class="dl">{p()!.file} · {fmtBytes(p()!.done)} / {fmtBytes(p()!.total)}</div>
                      </Show>
                      <div class="btn-row" style="margin-top:10px">
                        <Show when={installed(g.id)} fallback={
                          <Button size="sm" variant="accent" disabled={busy() !== null || g.layers.some((l) => !l.available)} onClick={() => download(g)}>
                            {busy() === g.id ? "Downloading…" : "Install"}
                          </Button>
                        }>
                          <Badge kind="on">installed</Badge>
                          <Button size="sm" variant="ghost" disabled={busy() !== null} onClick={() => download(g)}>Update</Button>
                        </Show>
                      </div>
                    </div>
                  </div>
                );
              }}
            </For>
          </div>
        </Show>
      </Show>
    </>
  );
}
