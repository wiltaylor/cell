import { createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { A } from "@solidjs/router";
import { Badge, Button, fmtSince } from "@cell/ui";
import { invoke } from "@tauri-apps/api/core";
import { fail, inbox, library, me, notify, setInbox, settings } from "../store";
import type { GameInvite, Peer, ServerInstance } from "../types";

export function Peers() {
  const url = () => settings().hub_url;
  const [peers, { refetch }] = createResource(() => [url(), me()] as const, async ([u, m]) => (u && m ? await invoke<Peer[]>("hub_peers", { url: u }) : []));
  const [servers] = createResource(() => [url(), me()] as const, async ([u, m]) => (u && m ? await invoke<ServerInstance[]>("hub_servers", { url: u }).catch(() => []) : []));
  const [myCode, setMyCode] = createSignal("");
  const [code, setCode] = createSignal("");
  const [target, setTarget] = createSignal("");
  const timer = setInterval(() => refetch(), 15000);
  onCleanup(() => clearInterval(timer));

  const makeCode = async () => { try { setMyCode((await invoke<{ code: string }>("hub_peer_code", { url: url() })).code); } catch (e) { fail(e); } };
  const pair = async () => { try { const m = await invoke<{ name: string }>("hub_peer_pair", { url: url(), code: code() }); notify(`paired with ${m.name}`); setCode(""); refetch(); } catch (e) { fail(e); } };
  const invite = async (to: string) => { try { await invoke("hub_invite", { url: url(), to, serverId: target() || null, message: "" }); notify("invite sent"); } catch (e) { fail(e); } };
  const join = async (i: GameInvite) => {
    const g = library()?.find((x) => x.id === i.game_id);
    if (!g) return notify(`install ${i.game_name} from the hub first`, true);
    try { await invoke("play", { path: g.path, command: null, args: i.join_args }); notify(`joining ${i.server_name}`); dismiss(i); } catch (e) { fail(e); }
  };
  const dismiss = async (i: GameInvite) => { await invoke("hub_dismiss", { url: url(), id: i.id }).catch(() => {}); setInbox(inbox().filter((x) => x.id !== i.id)); };

  return (
    <>
      <h1>Peers</h1>
      <Show when={me()} fallback={<div class="empty">Pair your <A href="/profile">profile</A> with a hub to see peers.</div>}>
        <p class="muted">Pair with people on {url()} to see when they are online and invite them to a server. One side makes a code, the other enters it.</p>
        <div class="row" style="align-items:flex-start">
          <div class="panel" style="flex:1; min-width:280px">
            <h3 style="margin-top:0">Your code</h3>
            <div class="row"><span class="mono" style="font-size:22px; letter-spacing:0.1em">{myCode() || "————"}</span><Button size="sm" onClick={makeCode}>New code</Button></div>
            <p class="muted small">Valid for 10 minutes. Give it to one person.</p>
          </div>
          <div class="panel" style="flex:1; min-width:280px">
            <h3 style="margin-top:0">Enter a code</h3>
            <div class="row" style="gap:8px; flex-wrap:nowrap"><input class="input mono" placeholder="K7F3-9Q2M" value={code()} onInput={(e) => setCode(e.currentTarget.value)} /><Button variant="accent" onClick={pair}>Pair</Button></div>
          </div>
        </div>
        <Show when={inbox().length > 0}>
          <h2>Invites for you</h2>
          <For each={inbox()}>
            {(i) => (
              <div class="panel row" style="justify-content:space-between">
                <div><b>{i.from.name}</b> invites you to <b>{i.server_name}</b> ({i.game_name})<Show when={i.message}><span class="muted"> · {i.message}</span></Show>
                  <div class="muted small mono">{i.join_args.join(" ")}</div></div>
                <div class="btn-row"><Button size="sm" variant="play" onClick={() => join(i)}>▶ Join</Button><Button size="sm" variant="ghost" onClick={() => dismiss(i)}>Dismiss</Button></div>
              </div>
            )}
          </For>
        </Show>
        <h2>Paired peers</h2>
        <div class="row" style="margin-bottom:10px"><span class="muted small">Invite to:</span>
          <select class="input" style="width:auto" onChange={(e) => setTarget(e.currentTarget.value)}><option value="">(no server)</option><For each={servers()}>{(s) => <option value={s.id}>{s.name}</option>}</For></select>
        </div>
        <Show when={peers() && peers()!.length > 0} fallback={<div class="empty">No peers yet.</div>}>
          <table class="list">
            <thead><tr><th>Peer</th><th>Status</th><th>Seen</th><th></th></tr></thead>
            <tbody>
              <For each={peers()}>
                {(p) => (
                  <tr>
                    <td><b>{p.member.name}</b><div class="muted small mono">{p.member.pubkey.slice(0, 16)}…</div></td>
                    <td><Badge kind={p.online ? "on" : "off"} dot>{p.status}{p.game_id ? ` · ${p.game_id}` : ""}</Badge></td>
                    <td>{p.last_seen ? `${fmtSince(p.last_seen)} ago` : "never"}</td>
                    <td><Button size="sm" variant="accent" onClick={() => invite(p.member.pubkey)}>Invite</Button></td>
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
