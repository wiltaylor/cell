// Mirrors crates/cell-hub-api.
export interface LayerInfo { name: string; role: string; placement: string; file: string | null; size: number; available: boolean }
export interface GrantSummary { network: boolean; home: string; display: string[]; audio: string[]; input: string[]; gpu: boolean; nvidia: string; dbus: string }
export interface Game {
  id: string; name: string; version: string; arch: string; comment: string | null; categories: string[]; protocols: string[];
  commands: string[]; has_server: boolean; grants: GrantSummary; file: string; size: number; layers: LayerInfo[]; art: string[]; uploaded_at: number;
}
export interface ServerInstance {
  id: string; game_id: string; game_name: string; name: string; state: "running" | "stopped"; pid: number | null;
  started_at: number | null; created_at: number; log_seq: number; command: string;
}
export interface LogLine { seq: number; ts: number; stream: "out" | "err" | "hub"; text: string }
export interface LogPage { lines: LogLine[]; seq: number }
export interface Backup { name: string; size: number; created_at: number }
export interface FileEntry { name: string; is_dir: boolean; size: number }
export interface HubInfo { name: string; version: string; games: number; servers: number; running: number; auth_required: boolean }
