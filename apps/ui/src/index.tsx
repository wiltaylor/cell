import { JSX, Show, splitProps } from "solid-js";

export function Badge(props: { kind?: "on" | "off" | "warn" | "err"; dot?: boolean; children: JSX.Element }) {
  return (
    <span class={`badge ${props.kind ?? ""}`}>
      <Show when={props.dot}><span class="dot" /></Show>
      {props.children}
    </span>
  );
}

export function Button(props: JSX.ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "play" | "accent" | "ghost" | "danger"; size?: "sm" }) {
  const [local, rest] = splitProps(props, ["variant", "size", "class", "children"]);
  return (
    <button class={`btn ${local.variant ?? ""} ${local.size ?? ""} ${local.class ?? ""}`} {...rest}>
      {local.children}
    </button>
  );
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KiB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MiB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}

export function fmtTime(secs?: number | null): string {
  if (!secs) return "";
  return new Date(secs * 1000).toLocaleString();
}

export function fmtSince(secs?: number | null): string {
  if (!secs) return "";
  const d = Math.max(0, Math.floor(Date.now() / 1000 - secs));
  if (d < 60) return `${d}s`;
  if (d < 3600) return `${Math.floor(d / 60)}m`;
  if (d < 86400) return `${Math.floor(d / 3600)}h ${Math.floor((d % 3600) / 60)}m`;
  return `${Math.floor(d / 86400)}d ${Math.floor((d % 86400) / 3600)}h`;
}

/** Portrait cover with fallbacks: portrait art, then icon, then initials. */
export function Cover(props: { src?: string; icon?: string; name: string }) {
  const initials = () => props.name.split(/\s+/).map((w) => w[0]).join("").slice(0, 3).toUpperCase();
  return (
    <div class="cover">
      <Show when={props.src} fallback={
        <Show when={props.icon} fallback={<div class="fallback">{initials()}</div>}>
          <img class="icon" src={props.icon} alt="" />
        </Show>
      }>
        <img src={props.src} alt="" loading="lazy" />
      </Show>
    </div>
  );
}
