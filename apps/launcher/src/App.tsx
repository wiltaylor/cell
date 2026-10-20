import { A, RouteSectionProps } from "@solidjs/router";
import { createResource, For, onMount, Show } from "solid-js";
import { art, inbox, library, loadSettings, me, profile, toast } from "./store";

function SideIcon(props: { path: string }) {
  const [src] = createResource(() => props.path, (p) => art(p, "icon"));
  return <Show when={src()} fallback={<span class="dot-ico" />}><img src={src()} alt="" /></Show>;
}

export function Layout(props: RouteSectionProps) {
  onMount(loadSettings);
  return (
    <div class="launcher">
      <aside class="sidebar">
        <div class="brand"><span class="dot" style="width:10px;height:10px;border-radius:50%;background:var(--accent);box-shadow:0 0 12px var(--accent)" /> Cell Launcher</div>
        <A href="/" end activeClass="active">Library</A>
        <A href="/hub" activeClass="active">Hub</A>
        <A href="/servers" activeClass="active">Servers</A>
        <A href="/peers" activeClass="active">Peers<Show when={inbox().length}><span class="badge on" style="margin-left:auto">{inbox().length}</span></Show></A>
        <A href="/profile" activeClass="active"><Show when={profile()} fallback="Profile">{profile()!.name}<Show when={me()}><span class="muted small" style="margin-left:auto">{me()!.role}</span></Show></Show></A>
        <A href="/settings" activeClass="active">Settings</A>
        <div class="section">Games</div>
        <div class="games">
          <For each={library() ?? []}>
            {(g) => <A href={`/game/${encodeURIComponent(g.id)}`} activeClass="active"><SideIcon path={g.path} />{g.name}</A>}
          </For>
        </div>
        <div class="foot">{library()?.length ?? 0} installed</div>
      </aside>
      <main class="content">{props.children}</main>
      <Show when={toast()}><div class={`toast ${toast()!.err ? "err" : ""}`}>{toast()!.text}</div></Show>
    </div>
  );
}
