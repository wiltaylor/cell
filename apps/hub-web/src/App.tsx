import { A, RouteSectionProps } from "@solidjs/router";
import { createResource, createSignal, onMount, Show } from "solid-js";
import { api, signIn, token } from "./api";
import type { Member } from "./types";

// One toast at a time, used by every page.
const [toast, setToast] = createSignal<{ text: string; err?: boolean } | null>(null);
let toastTimer: number | undefined;
export function notify(text: string, err = false) {
  setToast({ text, err });
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => setToast(null), err ? 6000 : 3000);
}
export function fail(e: unknown) {
  notify(e instanceof Error ? e.message : String(e), true);
}

// The signed-in member, shared by the layout and the pages.
const [session, setSession] = createSignal<Member | null>(null);
export { session };
export async function refreshSession() {
  if (!token()) {
    const s = await signIn().catch(() => null);
    if (!s) return setSession(null);
  }
  setSession(await api.me().catch(() => null));
}

export function Layout(props: RouteSectionProps) {
  const [info] = createResource(api.info);
  onMount(refreshSession);
  return (
    <div class="app">
      <header class="topbar">
        <div class="brand"><span class="dot" /> {info()?.name ?? "Cell Hub"}</div>
        <nav>
          <A href="/" end activeClass="active">Library</A>
          <A href="/servers" activeClass="active">Servers</A>
          <Show when={session()}><A href="/peers" activeClass="active">Peers</A></Show>
          <Show when={session()?.role === "admin"}>
            <A href="/upload" activeClass="active">Upload</A>
            <A href="/members" activeClass="active">Members</A>
          </Show>
        </nav>
        <div class="spacer" />
        <Show when={info()}>
          <span class="muted small">{info()!.games} games · {info()!.running}/{info()!.servers} servers running</span>
        </Show>
        <nav>
          <A href="/profile" activeClass="active">
            <Show when={session()} fallback="Sign in">{session()!.name} · {session()!.role}</Show>
          </A>
        </nav>
      </header>
      <main class="main">{props.children}</main>
      <Show when={toast()}>
        <div class={`toast ${toast()!.err ? "err" : ""}`}>{toast()!.text}</div>
      </Show>
    </div>
  );
}
