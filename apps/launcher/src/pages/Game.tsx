import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";
import { Badge, Button, fmtBytes, fmtTime } from "@cell/ui";
import { invoke } from "@tauri-apps/api/core";
import { art, fail, library, notify, refetchLibrary, setCurrentGame } from "../store";

export function GamePage() {
  const params = useParams();
  const nav = useNavigate();
  const game = createMemo(() => library()?.find((g) => g.id === decodeURIComponent(params.id ?? "")));
  const [hero] = createResource(() => game()?.path, (p) => art(p, "hero"));
  const [logo] = createResource(() => game()?.path, (p) => art(p, "logo"));
  const [menu, setMenu] = createSignal(false);
  const [out, setOut] = createSignal("");

  const play = async (command: string | null) => {
    setMenu(false);
    try { await invoke("play", { path: game()!.path, command, args: [] }); if (!command) setCurrentGame(game()!.id); notify(command ? `running ${command}` : `starting ${game()!.name}`); } catch (e) { fail(e); }
  };
  const cmd = async (what: string) => {
    try { setOut(await invoke<string>("cell_cmd", { path: game()!.path, what })); notify(`${what}: done`); } catch (e) { setOut(String(e)); fail(e); }
  };
  const remove = async () => {
    if (!confirm(`Delete ${game()!.file} from the games folder? Saves are kept.`)) return;
    try { await invoke("remove_game", { path: game()!.path }); await refetchLibrary(); nav("/"); } catch (e) { fail(e); }
  };

  return (
    <Show when={game()} fallback={<div class="empty">Game not found in the library.</div>}>
      {(g) => (
        <div class="game-detail">
          <div class="hero">
            <Show when={hero()}><img class="bg" src={hero()} alt="" /></Show>
            <div class="content">
              <Show when={logo()} fallback={<div class="title">{g().name}</div>}><img class="logo" src={logo()} alt={g().name} /></Show>
              <div class="actions">
                <Show when={g().missing_layers.length === 0} fallback={<Badge kind="err">missing: {g().missing_layers.join(", ")}</Badge>}>
                  <button class="btn play" onClick={() => play(null)}>▶ Play</button>
                </Show>
                <Show when={g().commands.length}>
                  <div class="menu">
                    <Button variant="ghost" onClick={() => setMenu(!menu())}>More ▾</Button>
                    <Show when={menu()}>
                      <div class="items">
                        <For each={g().commands}>{(c) => <button onClick={() => play(c)}>Run {c}</button>}</For>
                      </div>
                    </Show>
                  </div>
                </Show>
              </div>
            </div>
          </div>

          <div class="row" style="margin-top:18px; align-items:flex-start">
            <div class="panel" style="flex:2; min-width:320px">
              <h2 style="margin-top:0">{g().name} <span class="muted small">v{g().version}</span></h2>
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
              <dl class="kv" style="margin-top:16px">
                <dt>File</dt><dd class="mono">{g().path}</dd>
                <dt>Size</dt><dd>{fmtBytes(g().size)} in the file, {fmtBytes(g().total_size)} with shared layers</dd>
                <dt>Installed</dt><dd>{fmtTime(g().installed_at)}</dd>
                <dt>App id</dt><dd class="mono">{g().id}</dd>
              </dl>
            </div>
            <div class="panel" style="flex:1; min-width:280px">
              <h3 style="margin-top:0">Integrate</h3>
              <p class="muted small">Register with the desktop (menu entry, icon, URL schemes) or add to Steam with the game's artwork. Quit Steam before adding.</p>
              <div class="btn-row">
                <Button size="sm" onClick={() => cmd("install")}>Desktop entry</Button>
                <Button size="sm" variant="ghost" onClick={() => cmd("uninstall")}>Remove entry</Button>
                <Button size="sm" onClick={() => cmd("steam-add")}>Add to Steam</Button>
                <Button size="sm" variant="ghost" onClick={() => cmd("steam-remove")}>Remove from Steam</Button>
              </div>
              <h3>Diagnostics</h3>
              <div class="btn-row">
                <Button size="sm" variant="ghost" onClick={() => cmd("info")}>Grants and host</Button>
                <Button size="sm" variant="ghost" onClick={() => cmd("verify")}>Verify layers</Button>
              </div>
              <Show when={out()}><pre class="console" style="height:220px; margin-top:10px">{out()}</pre></Show>
              <h3>Danger</h3>
              <Button size="sm" variant="danger" onClick={remove}>Delete file</Button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}
