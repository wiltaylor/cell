import { createEffect, createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { A, useNavigate, useParams } from "@solidjs/router";
import { Badge, Button, fmtBytes, fmtSince, fmtTime } from "@cell/ui";
import { api } from "../api";
import { fail, notify } from "../App";
import type { Backup, FileEntry, LogLine } from "../types";

type Tab = "console" | "files" | "backups";

export function ServerPage() {
  const params = useParams();
  const id = () => params.id ?? "";
  const nav = useNavigate();
  const [server, { refetch }] = createResource(id, api.server);
  const [tab, setTab] = createSignal<Tab>("console");
  const timer = setInterval(() => refetch(), 3000);
  onCleanup(() => clearInterval(timer));

  const act = async (action: "start" | "stop" | "restart") => {
    try { await api.serverAction(id(), action); notify(`${action}: ok`); refetch(); } catch (e) { fail(e); }
  };
  const remove = async () => {
    if (!confirm("Delete this server and all of its state? Backups are kept.")) return;
    try { await api.deleteServer(id()); notify("deleted"); nav("/servers"); } catch (e) { fail(e); }
  };

  return (
    <Show when={server()} fallback={<div class="empty">{server.error ? String(server.error) : "Loading…"}</div>}>
      {(s) => (
        <>
          <div class="row" style="justify-content:space-between">
            <div>
              <div class="muted small"><A href="/servers">Servers</A> / {s().game_name}</div>
              <h1 style="display:flex; gap:12px; align-items:center">{s().name}
                <Badge kind={s().state === "running" ? "on" : "off"} dot>{s().state}</Badge>
              </h1>
              <div class="muted small">
                <Show when={s().state === "running"}>up {fmtSince(s().started_at)} · pid {s().pid} · </Show>
                runs the {s().command === "server" ? "server command" : "entry point"} · created {fmtTime(s().created_at)}
              </div>
            </div>
            <div class="btn-row">
              <Show when={s().state !== "running"} fallback={<>
                <Button variant="ghost" onClick={() => act("restart")}>Restart</Button>
                <Button variant="danger" onClick={() => act("stop")}>Stop</Button>
              </>}>
                <Button variant="play" onClick={() => act("start")}>Start</Button>
              </Show>
            </div>
          </div>

          <div class="tabs">
            <button class={tab() === "console" ? "active" : ""} onClick={() => setTab("console")}>Console</button>
            <button class={tab() === "files" ? "active" : ""} onClick={() => setTab("files")}>Config files</button>
            <button class={tab() === "backups" ? "active" : ""} onClick={() => setTab("backups")}>Backups</button>
          </div>
          <Show when={tab() === "console"}><Console id={id()} running={s().state === "running"} /></Show>
          <Show when={tab() === "files"}><Files id={id()} /></Show>
          <Show when={tab() === "backups"}><Backups id={id()} onChange={refetch} /></Show>

          <div class="panel" style="margin-top:24px">
            <h3 style="margin-top:0">Danger</h3>
            <Button variant="danger" size="sm" onClick={remove}>Delete server</Button>
          </div>
        </>
      )}
    </Show>
  );
}

function Console(props: { id: string; running: boolean }) {
  const [lines, setLines] = createSignal<LogLine[]>([]);
  const [input, setInput] = createSignal("");
  let box!: HTMLDivElement;
  let ws: WebSocket | undefined;

  const connect = () => {
    ws?.close();
    ws = new WebSocket(api.consoleUrl(props.id));
    ws.onmessage = (ev) => {
      const line = JSON.parse(ev.data) as LogLine;
      setLines((l) => {
        const next = [...l, line];
        return next.length > 3000 ? next.slice(next.length - 3000) : next;
      });
      queueMicrotask(() => { box.scrollTop = box.scrollHeight; });
    };
    ws.onclose = () => setTimeout(() => { if (ws?.readyState !== WebSocket.OPEN) connect(); }, 2000);
  };
  createEffect(() => { setLines([]); connect(); });
  onCleanup(() => ws?.close());

  const send = async (e: Event) => {
    e.preventDefault();
    const line = input();
    setInput("");
    try { await api.input(props.id, line); } catch (err) { fail(err); }
  };
  const hhmm = (ts: number) => new Date(ts * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  return (
    <>
      <div class="console" ref={box}>
        <For each={lines()}>{(l) => <div class={`line ${l.stream}`}><span class="ts">{hhmm(l.ts)}</span>{l.text}</div>}</For>
        <Show when={lines().length === 0}><div class="line hub">No output yet. Start the server to see its log here.</div></Show>
      </div>
      <form class="console-input" onSubmit={send}>
        <input class="input" placeholder={props.running ? "console command, e.g. server_info" : "server is stopped"} disabled={!props.running} value={input()} onInput={(e) => setInput(e.currentTarget.value)} />
        <Button variant="accent" disabled={!props.running}>Send</Button>
      </form>
    </>
  );
}

function Files(props: { id: string }) {
  const [dir, setDir] = createSignal("");
  const [entries, { refetch }] = createResource(() => [props.id, dir()] as const, ([id, d]) => api.files(id, d));
  const [open, setOpen] = createSignal<string | null>(null);
  const [text, setText] = createSignal("");
  const [dirty, setDirty] = createSignal(false);
  const openFile = async (path: string) => {
    try { const f = await api.file(props.id, path); setOpen(path); setText(f.text); setDirty(false); } catch (e) { fail(e); }
  };
  const save = async () => {
    try { await api.saveFile(props.id, open()!, text()); setDirty(false); notify("saved"); } catch (e) { fail(e); }
  };
  const up = () => setDir(dir().split("/").slice(0, -1).join("/"));
  return (
    <div class="files">
      <div>
        <div class="muted small mono" style="margin-bottom:6px">home/{dir()}</div>
        <div class="tree">
          <Show when={dir() !== ""}><button class="dir" onClick={up}>..</button></Show>
          <For each={entries()}>
            {(e: FileEntry) => (
              <button class={e.is_dir ? "dir" : ""} onClick={() => (e.is_dir ? setDir(dir() ? `${dir()}/${e.name}` : e.name) : openFile(dir() ? `${dir()}/${e.name}` : e.name))}>
                {e.is_dir ? "▸ " : "  "}{e.name}<Show when={!e.is_dir}><span class="muted"> · {fmtBytes(e.size)}</span></Show>
              </button>
            )}
          </For>
          <Show when={entries() && entries()!.length === 0}><div class="muted small" style="padding:8px">Empty. The game creates its config on first start.</div></Show>
        </div>
        <Button size="sm" variant="ghost" style="margin-top:8px" onClick={() => refetch()}>Refresh</Button>
      </div>
      <div>
        <Show when={open()} fallback={<div class="empty">Pick a file to edit it. Changes apply on the next start.</div>}>
          <div class="row" style="justify-content:space-between; margin-bottom:8px">
            <span class="mono">{open()}</span>
            <Button size="sm" variant="accent" disabled={!dirty()} onClick={save}>Save</Button>
          </div>
          <textarea class="input" value={text()} onInput={(e) => { setText(e.currentTarget.value); setDirty(true); }} spellcheck={false} />
        </Show>
      </div>
    </div>
  );
}

function Backups(props: { id: string; onChange: () => void }) {
  const [list, { refetch }] = createResource(() => props.id, api.backups);
  const [busy, setBusy] = createSignal(false);
  const create = async () => {
    setBusy(true);
    try { const b = await api.backup(props.id); notify(`backup ${b.name} (${fmtBytes(b.size)})`); refetch(); } catch (e) { fail(e); } finally { setBusy(false); }
  };
  const restore = async (b: Backup) => {
    if (!confirm(`Restore ${b.name}? The server stops and its current state is replaced.`)) return;
    try { await api.restore(props.id, b.name); notify("restored"); props.onChange(); } catch (e) { fail(e); }
  };
  const del = async (b: Backup) => {
    if (!confirm(`Delete backup ${b.name}?`)) return;
    try { await api.deleteBackup(props.id, b.name); refetch(); } catch (e) { fail(e); }
  };
  return (
    <div class="panel">
      <div class="row" style="justify-content:space-between">
        <div><b>Backups</b><div class="muted small">A backup is the whole server state: config, saves, and everything the game wrote.</div></div>
        <Button variant="accent" onClick={create} disabled={busy()}>Back up now</Button>
      </div>
      <Show when={list() && list()!.length > 0} fallback={<div class="empty" style="margin-top:12px">No backups yet.</div>}>
        <table class="list" style="margin-top:12px">
          <thead><tr><th>Backup</th><th>Created</th><th>Size</th><th></th></tr></thead>
          <tbody>
            <For each={list()}>
              {(b) => (
                <tr>
                  <td class="mono">{b.name}</td>
                  <td>{fmtTime(b.created_at)}</td>
                  <td>{fmtBytes(b.size)}</td>
                  <td><div class="btn-row">
                    <a class="btn sm ghost" href={api.backupUrl(props.id, b.name)}>Download</a>
                    <Button size="sm" variant="ghost" onClick={() => restore(b)}>Restore</Button>
                    <Button size="sm" variant="danger" onClick={() => del(b)}>Delete</Button>
                  </div></td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </div>
  );
}
