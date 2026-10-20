import { createResource, For, Show } from "solid-js";
import { A } from "@solidjs/router";
import { Badge, Cover, fmtBytes } from "@cell/ui";
import { api } from "../api";

export function Library() {
  const [games] = createResource(api.games);
  return (
    <>
      <h1>Library</h1>
      <p class="muted">Every cell on this hub. Open a game for details, downloads and hosting.</p>
      <Show when={games()} fallback={<div class="empty">Loading…</div>}>
        <Show when={games()!.length > 0} fallback={<div class="empty">No games yet. Upload a .cell from the Upload page or with <code>cell-hub upload</code>.</div>}>
          <div class="grid">
            <For each={games()}>
              {(g) => (
                <A href={`/games/${g.id}`} class="card">
                  <Cover src={g.art.includes("portrait") ? api.art(g.id, "portrait") : undefined} icon={g.art.includes("icon") ? api.art(g.id, "icon") : undefined} name={g.name} />
                  <Show when={g.has_server}><span class="badge on">Server</span></Show>
                  <div class="meta">
                    <div class="name">{g.name}</div>
                    <div class="sub">v{g.version} · {fmtBytes(g.size + g.layers.filter((l) => l.placement !== "embedded").reduce((a, l) => a + l.size, 0))}</div>
                  </div>
                </A>
              )}
            </For>
          </div>
        </Show>
      </Show>
      <Show when={games.error}><div class="empty">Cannot reach the hub: {String(games.error)}</div></Show>
      <span style="display:none"><Badge>.</Badge></span>
    </>
  );
}
