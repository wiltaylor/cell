import { A, RouteSectionProps } from "@solidjs/router";
import { createResource, createSignal, Show } from "solid-js";
import { api } from "./api";

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

export function Layout(props: RouteSectionProps) {
  const [info] = createResource(api.info);
  return (
    <div class="app">
      <header class="topbar">
        <div class="brand"><span class="dot" /> {info()?.name ?? "Cell Hub"}</div>
        <nav>
          <A href="/" end activeClass="active">Library</A>
          <A href="/servers" activeClass="active">Servers</A>
          <A href="/upload" activeClass="active">Upload</A>
        </nav>
        <div class="spacer" />
        <Show when={info()}>
          <span class="muted small">{info()!.games} games · {info()!.running}/{info()!.servers} servers running</span>
        </Show>
        <nav><A href="/settings" activeClass="active">Settings</A></nav>
      </header>
      <main class="main">{props.children}</main>
      <Show when={toast()}>
        <div class={`toast ${toast()!.err ? "err" : ""}`}>{toast()!.text}</div>
      </Show>
    </div>
  );
}
