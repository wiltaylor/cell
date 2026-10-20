import { createResource, createSignal } from "solid-js";
import { Button } from "@cell/ui";
import { api, setToken, token } from "../api";
import { notify } from "../App";

export function Settings() {
  const [info] = createResource(api.info);
  const [t, setT] = createSignal(token());
  return (
    <>
      <h1>Settings</h1>
      <div class="panel" style="max-width:560px">
        <label class="field">Admin token
          <input class="input" type="password" value={t()} onInput={(e) => setT(e.currentTarget.value)} placeholder={info()?.auth_required ? "required for uploads and server control" : "this hub needs no token"} />
        </label>
        <div class="btn-row" style="margin-top:12px">
          <Button variant="accent" onClick={() => { setToken(t()); notify("token saved"); }}>Save</Button>
        </div>
        <p class="muted small">Hub {info()?.name} v{info()?.version}. The token is kept in this browser only.</p>
      </div>
    </>
  );
}
