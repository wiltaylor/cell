import { createSignal, For, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { Button } from "@cell/ui";
import { upload } from "../api";
import { fail, notify } from "../App";

interface Item { file: File; progress: number; done: boolean; error?: string }

export function Upload() {
  const nav = useNavigate();
  const [items, setItems] = createSignal<Item[]>([]);
  const [busy, setBusy] = createSignal(false);
  const pick = (files: FileList | null) => {
    if (!files) return;
    setItems(Array.from(files).map((file) => ({ file, progress: 0, done: false })));
  };
  const update = (i: number, patch: Partial<Item>) => setItems((l) => l.map((x, j) => (j === i ? { ...x, ...patch } : x)));
  const go = async () => {
    setBusy(true);
    // Layers first so a game is complete when it appears.
    const order = [...items().keys()].sort((a, b) => Number(items()[a].file.name.endsWith(".cell")) - Number(items()[b].file.name.endsWith(".cell")));
    let lastGame: string | null = null;
    for (const i of order) {
      const it = items()[i];
      const path = it.file.name.endsWith(".cell") ? "/api/games" : "/api/layers";
      try {
        const res = (await upload(path, it.file, (p) => update(i, { progress: p }))) as { id?: string };
        update(i, { done: true, progress: 1 });
        if (res.id) lastGame = res.id;
      } catch (e) {
        update(i, { error: e instanceof Error ? e.message : String(e) });
        fail(e);
      }
    }
    setBusy(false);
    if (lastGame) { notify("uploaded"); nav(`/games/${lastGame}`); }
  };
  return (
    <>
      <h1>Upload</h1>
      <p class="muted">Pick a <code>.cell</code> and, when it uses a referenced runtime, the <code>runtime-*.layer</code> file beside it. Sidecar layers go in the game's <code>.cell.d</code> directory; upload those with <code>cell-hub upload</code>.</p>
      <div class="panel">
        <input type="file" multiple accept=".cell,.layer" onChange={(e) => pick(e.currentTarget.files)} />
        <Show when={items().length}>
          <table class="list" style="margin-top:12px">
            <tbody>
              <For each={items()}>
                {(it) => (
                  <tr>
                    <td class="mono">{it.file.name}</td>
                    <td style="width:40%"><div class="progress"><div style={{ width: `${Math.round(it.progress * 100)}%` }} /></div></td>
                    <td>{it.error ? <span style="color:var(--red)">{it.error}</span> : it.done ? "done" : `${Math.round(it.progress * 100)}%`}</td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
          <div class="btn-row" style="margin-top:12px"><Button variant="accent" onClick={go} disabled={busy()}>Upload</Button></div>
        </Show>
      </div>
    </>
  );
}
