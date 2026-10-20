// Mirrors crates/cell-hub-api.
export interface LayerInfo { name: string; role: string; placement: string; file: string | null; size: number; available: boolean }
export interface GrantSummary { network: boolean; home: string; display: string[]; audio: string[]; input: string[]; gpu: boolean; nvidia: string; dbus: string }
export interface Game {
  id: string; name: string; version: string; arch: string; comment: string | null; categories: string[]; protocols: string[];
  commands: string[]; has_server: boolean; grants: GrantSummary; file: string; size: number; layers: LayerInfo[]; art: string[]; uploaded_at: number;
}
export interface ServerInstance {
  id: string; game_id: string; game_name: string; name: string; state: "running" | "stopped"; pid: number | null;
  started_at: number | null; created_at: number; log_seq: number; command: string; join_args: string[];
}
export interface HubIdentity { pubkey: string; fingerprint: string; name: string; needs_first_admin: boolean }
export interface Member { pubkey: string; name: string; avatar: string | null; role: "admin" | "operator" | "member"; servers: string[]; added_at: number }
export interface PairResponse { member: Member; hub: HubIdentity }
export interface Challenge { nonce: string; expires_at: number }
export interface Session { token: string; expires_at: number; member: Member }
export interface InviteCode { code: string; role: string; servers: string[]; created_at: number; expires_at: number }
export interface Peer { member: Member; online: boolean; status: string; game_id: string | null; last_seen: number | null }
export interface PeerCode { code: string; expires_at: number }
export interface GameInvite { id: string; from: Member; server_id: string | null; server_name: string; game_id: string; game_name: string; join_args: string[]; message: string; created_at: number }
export interface LogLine { seq: number; ts: number; stream: "out" | "err" | "hub"; text: string }
export interface LogPage { lines: LogLine[]; seq: number }
export interface Backup { name: string; size: number; created_at: number }
export interface FileEntry { name: string; is_dir: boolean; size: number }
export interface HubInfo { name: string; version: string; games: number; servers: number; running: number; auth_required: boolean; pubkey: string; fingerprint: string }
