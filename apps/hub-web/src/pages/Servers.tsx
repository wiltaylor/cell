import { createResource, For, onCleanup, Show } from "solid-js";
import { A } from "@solidjs/router";
import { Badge, Button, fmtSince } from "@cell/ui";
import { api } from "../api";
import { fail, notify } from "../App";

export function Servers() {
  const [servers, { refetch }] = createResource(api.servers);
  const timer = setInterval(() => refetch(), 3000);
  onCleanup(() => clearInterval(timer));
  const act = async (id: string, action: "start" | "stop" | "restart") => {
    try { await api.serverAction(id, action); notify(`${action}: ok`); refetch(); } catch (e) { fail(e); }
  };
  return (
    <>
      <h1>Game servers</h1>
      <p class="muted">Dedicated servers run from cells on this hub. Open one for its console, config and backups.</p>
      <Show when={servers()} fallback={<div class="empty">Loading…</div>}>
        <Show when={servers()!.length > 0} fallback={<div class="empty">No servers. Open a game in the <A href="/">library</A> and create one.</div>}>
          <table class="list">
            <thead><tr><th>Server</th><th>Game</th><th>State</th><th>Uptime</th><th>PID</th><th></th></tr></thead>
            <tbody>
              <For each={servers()}>
                {(s) => (
                  <tr>
                    <td><A href={`/servers/${s.id}`}><b>{s.name}</b></A><div class="muted small mono">{s.id}</div></td>
                    <td>{s.game_name}</td>
                    <td><Badge kind={s.state === "running" ? "on" : "off"} dot>{s.state}</Badge></td>
                    <td>{s.state === "running" ? fmtSince(s.started_at) : "—"}</td>
                    <td class="mono">{s.pid ?? "—"}</td>
                    <td>
                      <div class="btn-row">
                        <Show when={s.state !== "running"} fallback={<>
                          <Button size="sm" variant="ghost" onClick={() => act(s.id, "restart")}>Restart</Button>
                          <Button size="sm" variant="danger" onClick={() => act(s.id, "stop")}>Stop</Button>
                        </>}>
                          <Button size="sm" variant="accent" onClick={() => act(s.id, "start")}>Start</Button>
                        </Show>
                        <A class="btn sm ghost" href={`/servers/${s.id}`}>Console</A>
                      </div>
                    </td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </Show>
      </Show>
    </>
  );
}
