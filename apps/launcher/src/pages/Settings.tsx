import { createSignal } from "solid-js";
import { Button } from "@cell/ui";
import { open } from "@tauri-apps/plugin-dialog";
import { fail, notify, refetchLibrary, saveSettings, settings } from "../store";

export function SettingsPage() {
  const [dir, setDir] = createSignal(settings().games_dir);
  const [url, setUrl] = createSignal(settings().hub_url);
  const [token, setToken] = createSignal(settings().hub_token);
  const pick = async () => {
    const d = await open({ directory: true, defaultPath: dir() });
    if (d) setDir(d as string);
  };
  const save = async () => {
    try { await saveSettings({ games_dir: dir(), hub_url: url(), hub_token: token() }); notify("saved"); refetchLibrary(); } catch (e) { fail(e); }
  };
  return (
    <>
      <h1>Settings</h1>
      <div class="panel" style="max-width:640px; display:grid; gap:14px">
        <label class="field">Games folder
          <div class="row" style="gap:8px; flex-wrap:nowrap">
            <input class="input" value={dir()} onInput={(e) => setDir(e.currentTarget.value)} />
            <Button variant="ghost" onClick={pick}>Browse…</Button>
          </div>
        </label>
        <label class="field">Hub URL
          <input class="input" value={url()} onInput={(e) => setUrl(e.currentTarget.value)} placeholder="http://hub.example:8787" />
        </label>
        <label class="field">Hub admin token (for uploads)
          <input class="input" type="password" value={token()} onInput={(e) => setToken(e.currentTarget.value)} />
        </label>
        <div class="btn-row"><Button variant="accent" onClick={save}>Save</Button></div>
        <p class="muted small">The games folder holds <code>.cell</code> files and their shared <code>runtime-*.layer</code> files. Saves live in <code>~/.local/share/cell/apps/</code>.</p>
      </div>
    </>
  );
}
