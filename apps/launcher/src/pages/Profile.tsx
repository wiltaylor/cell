import { createResource, createSignal, Show } from "solid-js";
import { Badge, Button, fmtTime } from "@cell/ui";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { fail, me, notify, profile, refetchMe, refetchProfile, settings } from "../store";
import type { HubIdentity, PairResponse, ProfileView } from "../types";

export function ProfilePage() {
  const [name, setName] = createSignal("");
  const [pass, setPass] = createSignal("");
  const [code, setCode] = createSignal("");
  const [hub] = createResource(() => settings().hub_url, async (url) => (url ? await invoke<HubIdentity>("hub_identity", { url }).catch(() => null) : null));

  const create = async () => {
    if (!name().trim()) return notify("choose a name", true);
    try { await invoke<ProfileView>("profile_create", { name: name() }); notify("profile created"); refetchProfile(); } catch (e) { fail(e); }
  };
  const doImport = async () => {
    const file = await open({ multiple: false, filters: [{ name: "Cell profile", extensions: ["cellprofile", "json"] }] });
    if (!file) return;
    if (!pass()) return notify("enter the passphrase of the export file", true);
    try { await invoke("profile_import", { file, passphrase: pass() }); setPass(""); notify("profile imported"); refetchProfile(); refetchMe(); } catch (e) { fail(e); }
  };
  const doExport = async () => {
    if (!pass()) return notify("choose a passphrase for the export file", true);
    const file = await save({ defaultPath: `${profile()!.name}.cellprofile` });
    if (!file) return;
    try { await invoke("profile_export", { file, passphrase: pass() }); setPass(""); notify(`exported to ${file}`); } catch (e) { fail(e); }
  };
  const forget = async () => {
    if (!confirm("Remove the profile from this machine? Export it first if you need it elsewhere.")) return;
    try { await invoke("profile_forget"); refetchProfile(); refetchMe(); } catch (e) { fail(e); }
  };
  const pair = async () => {
    try { const r = await invoke<PairResponse>("hub_pair", { url: settings().hub_url, code: code() }); notify(`paired with ${r.hub.name} as ${r.member.role}`); setCode(""); refetchMe(); } catch (e) { fail(e); }
  };

  return (
    <>
      <h1>Profile</h1>
      <p class="muted">Your profile is a keypair on this machine. It is your identity on every hub and with every peer. No account and no password to sign in; export it to keep a copy or move it.</p>
      <Show when={profile()} fallback={
        <div class="row" style="align-items:flex-start">
          <div class="panel" style="flex:1; min-width:300px">
            <h3 style="margin-top:0">New profile</h3>
            <label class="field">Display name<input class="input" value={name()} onInput={(e) => setName(e.currentTarget.value)} /></label>
            <div class="btn-row" style="margin-top:12px"><Button variant="accent" onClick={create}>Create</Button></div>
          </div>
          <div class="panel" style="flex:1; min-width:300px">
            <h3 style="margin-top:0">Import a profile</h3>
            <label class="field">Passphrase of the export file<input class="input" type="password" value={pass()} onInput={(e) => setPass(e.currentTarget.value)} /></label>
            <div class="btn-row" style="margin-top:12px"><Button onClick={doImport}>Pick file and import…</Button></div>
          </div>
        </div>
      }>
        {(p) => (
          <div class="row" style="align-items:flex-start">
            <div class="panel" style="flex:1; min-width:320px">
              <h2 style="margin-top:0">{p().name}</h2>
              <dl class="kv">
                <dt>Fingerprint</dt><dd class="mono">{p().fingerprint}</dd>
                <dt>Public key</dt><dd class="mono small" style="word-break:break-all">{p().pubkey}</dd>
                <dt>Created</dt><dd>{fmtTime(p().created_at)}</dd>
                <dt>Stored at</dt><dd class="mono small">{p().path}</dd>
              </dl>
              <h3>Export</h3>
              <div class="row" style="gap:8px; flex-wrap:nowrap">
                <input class="input" type="password" placeholder="passphrase for the file" value={pass()} onInput={(e) => setPass(e.currentTarget.value)} />
                <Button onClick={doExport}>Export…</Button>
              </div>
              <h3>Danger</h3>
              <Button size="sm" variant="danger" onClick={forget}>Forget profile on this machine</Button>
            </div>
            <div class="panel" style="flex:1; min-width:320px">
              <h3 style="margin-top:0">Hub</h3>
              <Show when={hub()} fallback={<p class="muted"><Show when={settings().hub_url} fallback={<>No hub connected. A hub is optional; set one on the Hub page when you want to download games or play with peers.</>}>Cannot reach the hub at {settings().hub_url}. Is it running?</Show></p>}>
                <dl class="kv">
                  <dt>Name</dt><dd>{hub()!.name}</dd>
                  <dt>Fingerprint</dt><dd class="mono">{hub()!.fingerprint}</dd>
                  <dt>You</dt><dd><Show when={me()} fallback={<Badge kind="warn">not paired</Badge>}><Badge kind="on" dot>{me()!.role}</Badge></Show></dd>
                </dl>
                <Show when={hub()!.needs_first_admin}><p><Badge kind="warn">no admin yet</Badge> The first invite code is in the hub's log; pairing with it makes you the admin.</p></Show>
                <Show when={!me()}>
                  <label class="field" style="margin-top:12px">Invite code<input class="input mono" placeholder="K7F3-9Q2M" value={code()} onInput={(e) => setCode(e.currentTarget.value)} /></label>
                  <div class="btn-row" style="margin-top:12px"><Button variant="accent" onClick={pair}>Pair with this hub</Button></div>
                </Show>
              </Show>
            </div>
          </div>
        )}
      </Show>
    </>
  );
}
