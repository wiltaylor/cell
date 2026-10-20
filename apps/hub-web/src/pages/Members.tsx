import { createResource, createSignal, For, Show } from "solid-js";
import { Badge, Button, fmtTime } from "@cell/ui";
import { fingerprint } from "@cell/ui/src/identity";
import { api } from "../api";
import { fail, notify } from "../App";

export function Members() {
  const [members, { refetch }] = createResource(api.members);
  const [invites, { refetch: refetchInvites }] = createResource(api.invites);
  const [servers] = createResource(api.servers);
  const [role, setRole] = createSignal("member");
  const [pick, setPick] = createSignal<string[]>([]);
  const create = async () => {
    try { const i = await api.createInvite(role(), role() === "operator" ? pick() : []); notify(`invite ${i.code} (1 hour)`); refetchInvites(); } catch (e) { fail(e); }
  };
  const setMemberRole = async (pubkey: string, r: string) => {
    try { await api.updateMember(pubkey, r); refetch(); } catch (e) { fail(e); }
  };
  const remove = async (pubkey: string, name: string) => {
    if (!confirm(`Remove ${name} from the hub?`)) return;
    try { await api.removeMember(pubkey); refetch(); } catch (e) { fail(e); }
  };
  return (
    <>
      <h1>Members</h1>
      <p class="muted">Everyone paired with this hub. Invite codes are single use and expire after an hour.</p>
      <div class="panel">
        <h3 style="margin-top:0">New invite</h3>
        <div class="row">
          <select class="input" style="width:auto" value={role()} onChange={(e) => setRole(e.currentTarget.value)}>
            <option value="member">member: play, see servers, pair with peers</option>
            <option value="operator">operator: control chosen servers</option>
            <option value="admin">admin: everything</option>
          </select>
          <Show when={role() === "operator"}>
            <select class="input" style="width:auto" multiple onChange={(e) => setPick(Array.from(e.currentTarget.selectedOptions).map((o) => o.value))}>
              <For each={servers()}>{(s) => <option value={s.id}>{s.name}</option>}</For>
            </select>
          </Show>
          <Button variant="accent" onClick={create}>Make invite</Button>
        </div>
        <Show when={invites() && invites()!.length > 0}>
          <table class="list" style="margin-top:12px">
            <thead><tr><th>Code</th><th>Role</th><th>Servers</th><th>Expires</th></tr></thead>
            <tbody><For each={invites()}>{(i) => <tr><td class="mono"><b>{i.code}</b></td><td>{i.role}</td><td class="mono small">{i.servers.join(", ")}</td><td>{fmtTime(i.expires_at)}</td></tr>}</For></tbody>
          </table>
        </Show>
      </div>
      <Show when={members.error}><div class="empty">{String(members.error)}</div></Show>
      <Show when={members()}>
        <table class="list" style="margin-top:16px">
          <thead><tr><th>Member</th><th>Fingerprint</th><th>Role</th><th>Servers</th><th>Since</th><th></th></tr></thead>
          <tbody>
            <For each={members()}>
              {(m) => (
                <tr>
                  <td><b>{m.name}</b></td>
                  <td class="mono small">{fingerprint(m.pubkey)}</td>
                  <td>
                    <select class="input" style="width:auto" value={m.role} onChange={(e) => setMemberRole(m.pubkey, e.currentTarget.value)}>
                      <option value="member">member</option><option value="operator">operator</option><option value="admin">admin</option>
                    </select>
                  </td>
                  <td class="mono small">{m.servers.join(", ") || <Badge kind="off">—</Badge>}</td>
                  <td>{fmtTime(m.added_at)}</td>
                  <td><Button size="sm" variant="danger" onClick={() => remove(m.pubkey, m.name)}>Remove</Button></td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </>
  );
}
