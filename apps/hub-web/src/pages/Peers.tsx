import { createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { Badge, Button, fmtSince } from "@cell/ui";
import { fingerprint } from "@cell/ui/src/identity";
import { api } from "../api";
import { fail, notify } from "../App";

export function Peers() {
  const [peers, { refetch }] = createResource(api.peers);
  const [servers] = createResource(api.servers);
  const [inbox, { refetch: refetchInbox }] = createResource(api.inbox);
  const [myCode, setMyCode] = createSignal("");
  const [code, setCode] = createSignal("");
  const [target, setTarget] = createSignal<string>("");
  const timer = setInterval(() => { refetch(); refetchInbox(); api.presence("online").catch(() => {}); }, 15000);
  onCleanup(() => clearInterval(timer));
  api.presence("online").catch(() => {});
  const makeCode = async () => { try { setMyCode((await api.peerCode()).code); } catch (e) { fail(e); } };
  const pair = async () => { try { const m = await api.pairPeer(code()); notify(`paired with ${m.name}`); setCode(""); refetch(); } catch (e) { fail(e); } };
  const invite = async (to: string) => {
    try { await api.sendInvite(to, target() || null, ""); notify("invite sent"); } catch (e) { fail(e); }
  };
  return (
    <>
      <h1>Peers</h1>
      <p class="muted">Pair with people on this hub to see when they are online and invite them to a server. Pairing is a code exchange: one side makes a code, the other enters it.</p>
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
      <Show when={inbox() && inbox()!.length > 0}>
        <h2>Invites for you</h2>
        <For each={inbox()}>
          {(i) => (
            <div class="panel row" style="justify-content:space-between">
              <div><b>{i.from.name}</b> invites you to <b>{i.server_name}</b> ({i.game_name}) <Show when={i.message}><span class="muted">· {i.message}</span></Show>
                <div class="muted small mono">join: {i.join_args.join(" ") || "see the server page"}</div></div>
              <Button size="sm" variant="ghost" onClick={() => api.dismiss(i.id).then(() => refetchInbox())}>Dismiss</Button>
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
          <thead><tr><th>Peer</th><th>Fingerprint</th><th>Status</th><th>Seen</th><th></th></tr></thead>
          <tbody>
            <For each={peers()}>
              {(p) => (
                <tr>
                  <td><b>{p.member.name}</b></td>
                  <td class="mono small">{fingerprint(p.member.pubkey)}</td>
                  <td><Badge kind={p.online ? "on" : "off"} dot>{p.status}{p.game_id ? ` · ${p.game_id}` : ""}</Badge></td>
                  <td>{p.last_seen ? `${fmtSince(p.last_seen)} ago` : "never"}</td>
                  <td><div class="btn-row"><Button size="sm" variant="accent" onClick={() => invite(p.member.pubkey)}>Invite</Button><Button size="sm" variant="ghost" onClick={() => api.removePeer(p.member.pubkey).then(() => refetch())}>Unpair</Button></div></td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </>
  );
}
