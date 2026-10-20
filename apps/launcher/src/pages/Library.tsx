import { createResource, For, Show } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { Badge, Cover, fmtBytes } from "@cell/ui";
import { invoke } from "@tauri-apps/api/core";
import { art, fail, library, notify, setCurrentGame, settings } from "../store";
import type { LocalGame } from "../types";

function GameCard(props: { g: LocalGame }) {
  const [portrait] = createResource(() => props.g.path, (p) => art(p, "portrait"));
  const [icon] = createResource(() => props.g.path, (p) => art(p, "icon"));
  return (
    <A href={`/game/${encodeURIComponent(props.g.id)}`} class="card">
      <Cover src={portrait()} icon={icon()} name={props.g.name} />
      <Show when={props.g.missing_layers.length}><span class="badge err">missing runtime</span></Show>
      <div class="meta">
        <div class="name">{props.g.name}</div>
        <div class="sub">v{props.g.version} · {fmtBytes(props.g.total_size)}</div>
      </div>
    </A>
  );
}

function Featured(props: { g: LocalGame }) {
  const nav = useNavigate();
  const [hero] = createResource(() => props.g.path, (p) => art(p, "hero"));
  const [logo] = createResource(() => props.g.path, (p) => art(p, "logo"));
  const play = async (e: Event) => {
    e.preventDefault();
    try { await invoke("play", { path: props.g.path, command: null, args: [] }); setCurrentGame(props.g.id); notify(`starting ${props.g.name}`); } catch (err) { fail(err); }
  };
  return (
    <div class="featured">
      <Show when={hero()}><img class="bg" src={hero()} alt="" /></Show>
      <div class="content">
        <Show when={logo()} fallback={<div class="title">{props.g.name}</div>}><img class="logo" src={logo()} alt={props.g.name} /></Show>
        <div class="actions">
          <button class="btn play" onClick={play}>▶ Play</button>
          <button class="btn ghost" onClick={() => nav(`/game/${encodeURIComponent(props.g.id)}`)}>Details</button>
        </div>
      </div>
    </div>
  );
}

export function Library() {
  return (
    <>
      <Show when={library() && library()!.length > 0} fallback={
        <div class="empty">
          <h2 style="margin-top:0">Your library is empty</h2>
          <p>Drop <code>.cell</code> files into <code>{settings().games_dir || "the games folder"}</code>, or download games from the <A href="/hub">Hub</A>.</p>
        </div>
      }>
        <Featured g={library()![0]} />
        <div class="row" style="justify-content:space-between; margin-bottom:12px">
          <h2 style="margin:0">All games</h2>
          <span class="muted small">{library()!.length} installed · {settings().games_dir}</span>
        </div>
        <div class="grid">
          <For each={library()}>{(g) => <GameCard g={g} />}</For>
        </div>
      </Show>
      <span style="display:none"><Badge>.</Badge></span>
    </>
  );
}
