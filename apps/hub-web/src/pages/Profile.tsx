import { createResource, createSignal, Show } from "solid-js";
import { Badge, Button, fmtTime } from "@cell/ui";
import { clearProfile, createProfile, download, exportProfile, fingerprint, importProfile, loadProfile, publicKey } from "@cell/ui/src/identity";
import { api, pairWithHub, signIn, token } from "../api";
import { fail, notify, refreshSession } from "../App";

export function Profile() {
  const [profile, setProfile] = createSignal(loadProfile());
  const [pubkey] = createResource(profile, async (p) => (p ? await publicKey(p) : ""));
  const [hub] = createResource(api.identity);
  const [me, { refetch: refetchMe }] = createResource(() => token(), async (t) => (t ? await api.me().catch(() => null) : null));
  const [name, setName] = createSignal("");
  const [code, setCode] = createSignal("");
  const [pass, setPass] = createSignal("");
  let fileInput!: HTMLInputElement;

  const create = async () => {
    if (!name().trim()) return notify("choose a name", true);
    setProfile(await createProfile(name().trim()));
    notify("profile created");
  };
  const doImport = async () => {
    const f = fileInput.files?.[0];
    if (!f) return notify("pick an export file", true);
    try { setProfile(await importProfile(await f.text(), pass())); setPass(""); notify("profile imported"); await signIn(); refreshSession(); refetchMe(); } catch (e) { fail(e); }
  };
  const doExport = async () => {
    if (!pass()) return notify("choose a passphrase for the export file", true);
    download(`${profile()!.name}.cellprofile`, await exportProfile(profile()!, pass()));
    setPass("");
  };
  const pair = async () => {
    try { const r = await pairWithHub(code()); notify(`paired with ${r.hub.name} as ${r.member.role}`); setCode(""); refreshSession(); refetchMe(); } catch (e) { fail(e); }
  };
  const forget = () => {
    if (!confirm("Remove the profile from this browser? Export it first if you need it elsewhere.")) return;
    clearProfile(); setProfile(null); api.logout().catch(() => {}); refreshSession();
  };

  return (
    <>
      <h1>Profile</h1>
      <p class="muted">Your profile is a keypair kept in this browser. It is your identity on every hub and with every peer. No account, no password; export it to keep a copy.</p>
      <Show when={profile()} fallback={
        <div class="row" style="align-items:flex-start">
          <div class="panel" style="flex:1; min-width:300px">
            <h3 style="margin-top:0">New profile</h3>
            <label class="field">Display name<input class="input" value={name()} onInput={(e) => setName(e.currentTarget.value)} /></label>
            <div class="btn-row" style="margin-top:12px"><Button variant="accent" onClick={create}>Create</Button></div>
          </div>
          <div class="panel" style="flex:1; min-width:300px">
            <h3 style="margin-top:0">Import a profile</h3>
            <input type="file" accept=".cellprofile,.json" ref={fileInput} />
            <label class="field" style="margin-top:10px">Passphrase<input class="input" type="password" value={pass()} onInput={(e) => setPass(e.currentTarget.value)} /></label>
            <div class="btn-row" style="margin-top:12px"><Button onClick={doImport}>Import</Button></div>
          </div>
        </div>
      }>
        {(p) => (
          <div class="row" style="align-items:flex-start">
            <div class="panel" style="flex:1; min-width:320px">
              <h2 style="margin-top:0">{p().name}</h2>
              <dl class="kv">
                <dt>Fingerprint</dt><dd class="mono">{pubkey() ? fingerprint(pubkey()!) : "…"}</dd>
                <dt>Public key</dt><dd class="mono small" style="word-break:break-all">{pubkey()}</dd>
                <dt>Created</dt><dd>{fmtTime(p().created_at)}</dd>
                <dt>On this hub</dt>
                <dd><Show when={me()} fallback={<Badge kind="warn">not paired</Badge>}><Badge kind="on" dot>{me()!.role}</Badge></Show></dd>
              </dl>
              <h3>Export</h3>
              <p class="muted small">The file is encrypted with this passphrase. Import it in the launcher or another browser.</p>
              <div class="row" style="gap:8px; flex-wrap:nowrap">
                <input class="input" type="password" placeholder="passphrase" value={pass()} onInput={(e) => setPass(e.currentTarget.value)} />
                <Button onClick={doExport}>Export</Button>
              </div>
              <h3>Danger</h3>
              <Button size="sm" variant="danger" onClick={forget}>Forget profile in this browser</Button>
            </div>
            <div class="panel" style="flex:1; min-width:320px">
              <h3 style="margin-top:0">This hub</h3>
              <Show when={hub()}>
                <dl class="kv">
                  <dt>Name</dt><dd>{hub()!.name}</dd>
                  <dt>Fingerprint</dt><dd class="mono">{hub()!.fingerprint}</dd>
                </dl>
                <Show when={hub()!.needs_first_admin}><p><Badge kind="warn">no admin yet</Badge> The first invite code is in the hub's log. Pairing with it makes you the admin.</p></Show>
              </Show>
              <Show when={!me()}>
                <label class="field" style="margin-top:12px">Invite code<input class="input mono" placeholder="K7F3-9Q2M" value={code()} onInput={(e) => setCode(e.currentTarget.value)} /></label>
                <div class="btn-row" style="margin-top:12px"><Button variant="accent" onClick={pair}>Pair with this hub</Button></div>
              </Show>
              <Show when={me()}>
                <p class="muted small" style="margin-top:12px">Signed in as <b>{me()!.name}</b> ({me()!.role}).</p>
                <Button size="sm" variant="ghost" onClick={() => { api.logout().then(() => { refreshSession(); refetchMe(); }); }}>Sign out</Button>
              </Show>
            </div>
          </div>
        )}
      </Show>
    </>
  );
}
