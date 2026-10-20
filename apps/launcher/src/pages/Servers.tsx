import { createResource, For, onCleanup, Show } from "solid-js";
import { A } from "@solidjs/router";
import { Badge, fmtSince } from "@cell/ui";
import { invoke } from "@tauri-apps/api/core";
import { library, settings } from "../store";
import type { ServerInstance } from "../types";

export function Servers() {
  const [servers, { refetch }] = createResource(() => settings().hub_url, async (url) => (url ? await invoke<ServerInstance[]>("hub_servers", { url }) : []));
  const timer = setInterval(() => refetch(), 5000);
  onCleanup(() => clearInterval(timer));
  const host = () => { try { return new URL(settings().hub_url).hostname; } catch { return "the hub"; } };
  const adminUrl = (id: string) => `${settings().hub_url.replace(/\/$/, "")}/servers/${id}`;
  return (
    <>
      <h1>Game servers</h1>
      <p class="muted">Servers hosted on {settings().hub_url}. Join them from the game with the host <code>{host()}</code>. Manage them in the hub's web UI.</p>
      <Show when={servers.error}><div class="empty">Cannot reach the hub: {String(servers.error)}</div></Show>
      <Show when={servers() && servers()!.length > 0} fallback={<Show when={servers() && servers()!.length === 0}><div class="empty">No servers on this hub.</div></Show>}>
        <table class="list">
          <thead><tr><th>Server</th><th>Game</th><th>State</th><th>Uptime</th><th>Installed</th><th></th></tr></thead>
          <tbody>
            <For each={servers()}>
              {(s) => (
                <tr>
                  <td><b>{s.name}</b><div class="muted small mono">{s.id}</div></td>
                  <td>{s.game_name}</td>
                  <td><Badge kind={s.state === "running" ? "on" : "off"} dot>{s.state}</Badge></td>
                  <td>{s.state === "running" ? fmtSince(s.started_at) : "—"}</td>
                  <td>
                    <Show when={library()?.some((g) => g.id === s.game_id)} fallback={<A href="/hub">get it from the hub</A>}>
                      <A href={`/game/${encodeURIComponent(s.game_id)}`}>play</A>
                    </Show>
                  </td>
                  <td><a class="btn sm ghost" href={adminUrl(s.id)} target="_blank">Admin</a></td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </>
  );
}
