export interface GrantSummary { network: boolean; home: string; display: string[]; audio: string[]; input: string[]; gpu: boolean; nvidia: string; dbus: string }
export interface LocalGame {
  id: string; name: string; version: string; comment: string | null; categories: string[]; commands: string[]; has_server: boolean;
  path: string; file: string; size: number; total_size: number; art: string[]; missing_layers: string[]; grants: GrantSummary; installed_at: number;
}
export interface LayerInfo { name: string; role: string; placement: string; file: string | null; size: number; available: boolean }
export interface HubGame {
  id: string; name: string; version: string; arch: string; comment: string | null; categories: string[]; protocols: string[];
  commands: string[]; has_server: boolean; grants: GrantSummary; file: string; size: number; layers: LayerInfo[]; art: string[]; uploaded_at: number;
}
export interface ServerInstance {
  id: string; game_id: string; game_name: string; name: string; state: "running" | "stopped"; pid: number | null;
  started_at: number | null; created_at: number; log_seq: number; command: string; join_args: string[];
}
export interface ProfileView { name: string; pubkey: string; fingerprint: string; created_at: number; path: string }
export interface HubIdentity { pubkey: string; fingerprint: string; name: string; needs_first_admin: boolean }
export interface Member { pubkey: string; name: string; avatar: string | null; role: string; servers: string[]; added_at: number }
export interface Peer { member: Member; online: boolean; status: string; game_id: string | null; last_seen: number | null }
export interface PairResponse { member: Member; hub: HubIdentity }
export interface GameInvite { id: string; from: Member; server_id: string | null; server_name: string; game_id: string; game_name: string; join_args: string[]; message: string; created_at: number }
export interface Settings { games_dir: string; hub_url: string; hub_token: string }
export interface Progress { id: string; file: string; done: number; total: number; finished: boolean; error: string | null }
