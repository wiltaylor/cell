import { createResource, createSignal, For, Show } from "solid-js";
import { A, useNavigate, useParams } from "@solidjs/router";
import { Badge, Button, fmtBytes, fmtTime } from "@cell/ui";
import { api } from "../api";
import { fail, notify } from "../App";

export function GamePage() {
  const params = useParams();
  const nav = useNavigate();
  const [game, { refetch }] = createResource(() => params.id ?? "", api.game);
  const [serverName, setServerName] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const host = async () => {
    const g = game()!;
    setBusy(true);
    try {
      const inst = await api.createServer(g.id, serverName() || `${g.name} server`);
      notify(`created ${inst.name}`);
      nav(`/servers/${inst.id}`);
    } catch (e) { fail(e); } finally { setBusy(false); }
  };
  const remove = async () => {
    if (!confirm(`Remove ${game()!.name} from the hub?`)) return;
    try { await api.deleteGame(game()!.id); notify("removed"); nav("/"); } catch (e) { fail(e); }
  };

  return (
    <Show when={game()} fallback={<div class="empty">{game.error ? String(game.error) : "Loading…"}</div>}>
      {(g) => (
        <>
          <div class="hero">
            <Show when={g().art.includes("hero")}><img class="bg" src={api.art(g().id, "hero")} alt="" /></Show>
            <div class="content">
              <Show when={g().art.includes("logo")} fallback={<div class="title">{g().name}</div>}>
                <img class="logo" src={api.art(g().id, "logo")} alt={g().name} />
              </Show>
              <div class="actions">
                <a class="btn play" href={api.download(g().id)}>Download</a>
              </div>
            </div>
          </div>

          <div class="row" style="margin-top:18px; align-items:flex-start">
            <div class="panel" style="flex:2; min-width:320px">
              <h2 style="margin-top:0">{g().name} <span class="muted small">v{g().version} · {g().arch}</span></h2>
              <Show when={g().comment}><p>{g().comment}</p></Show>
              <div class="chips">
                <For each={g().categories}>{(c) => <Badge>{c}</Badge>}</For>
                <Show when={g().has_server}><Badge kind="on" dot>dedicated server</Badge></Show>
                <Badge kind={g().grants.network ? "warn" : "off"}>network {g().grants.network ? "on" : "off"}</Badge>
                <Badge kind={g().grants.gpu ? "on" : "off"}>gpu {g().grants.gpu ? `· nvidia ${g().grants.nvidia}` : "off"}</Badge>
                <For each={g().grants.display}>{(d) => <Badge>{d}</Badge>}</For>
                <For each={g().grants.audio}>{(d) => <Badge>{d}</Badge>}</For>
                <For each={g().grants.input}>{(d) => <Badge>{d}</Badge>}</For>
                <Badge>home {g().grants.home}</Badge>
              </div>
              <h3>Files</h3>
              <table class="list">
                <thead><tr><th>Part</th><th>Placement</th><th>Size</th><th></th></tr></thead>
                <tbody>
                  <tr><td class="mono">{g().file}</td><td>cell</td><td>{fmtBytes(g().size)}</td><td><a href={api.download(g().id)}>download</a></td></tr>
                  <For each={g().layers.filter((l) => l.placement !== "embedded")}>
                    {(l) => (
                      <tr>
                        <td class="mono">{l.file}</td>
                        <td>{l.placement} {l.role}</td>
                        <td>{fmtBytes(l.size)}</td>
                        <td>
                          <Show when={l.available} fallback={<Badge kind="err">missing on hub</Badge>}>
                            <a href={l.placement === "referenced" ? api.layer(l.file!) : `/api/games/${g().id}/sidecar/${l.file}`}>download</a>
                          </Show>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
              <p class="muted small">Put every file in one directory. Referenced runtimes are shared by all cells in that directory. Uploaded {fmtTime(g().uploaded_at)}.</p>
              <Show when={g().commands.length}>
                <h3>Commands</h3>
                <div class="chips"><For each={g().commands}>{(c) => <Badge>CELL_COMMAND={c}</Badge>}</For></div>
              </Show>
              <Show when={g().protocols.length}>
                <h3>URL schemes</h3>
                <div class="chips"><For each={g().protocols}>{(p) => <Badge>{p}://</Badge>}</For></div>
              </Show>
            </div>

            <div class="panel" style="flex:1; min-width:280px">
              <h3 style="margin-top:0">Host a server</h3>
              <Show when={g().has_server} fallback={<p class="muted">This cell has no <code>server</code> command. The hub can still run its entry point headless.</p>}>
                <p class="muted small">Runs the cell's <code>server</code> command on this hub with its own state, console, config files and backups.</p>
              </Show>
              <label class="field">Server name
                <input class="input" placeholder={`${g().name} server`} value={serverName()} onInput={(e) => setServerName(e.currentTarget.value)} />
              </label>
              <div class="btn-row" style="margin-top:12px">
                <Button variant="accent" onClick={host} disabled={busy()}>Create server</Button>
                <A href="/servers" class="btn ghost">All servers</A>
              </div>
              <h3>Danger</h3>
              <Button variant="danger" size="sm" onClick={remove}>Remove from hub</Button>
              <span style="display:none" onClick={() => refetch()} />
            </div>
          </div>
        </>
      )}
    </Show>
  );
}
