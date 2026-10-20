//! Identity, members, invites, sessions, presence and peers.
//!
//! Files under the data directory: `identity.json` (the hub's own
//! profile), `members.json`, `invites.json`, `sessions.json`, `peers.json`.
//! Presence, challenges, peer codes and game invites live in memory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use cell_hub_api::{
    Challenge, GameInvite, HubIdentity, InviteCode, Member, Peer, PeerCode, Presence, Session,
};
use cell_identity::{
    fingerprint, invite_code, login_message, normalize_code, pair_message, random_token, verify,
    Profile,
};
use serde::{de::DeserializeOwned, Serialize};

use crate::store::now;

const SESSION_DAYS: u64 = 30;
const INVITE_SECS: u64 = 3600;
const PEER_CODE_SECS: u64 = 600;
const CHALLENGE_SECS: u64 = 120;
const ONLINE_SECS: u64 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Member,
    Operator,
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "admin" => Some(Role::Admin),
            "operator" => Some(Role::Operator),
            "member" => Some(Role::Member),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Operator => "operator",
            Role::Member => "member",
        }
    }
}

/// Who is calling, after the token was checked.
#[derive(Debug, Clone)]
pub struct Caller {
    pub member: Member,
    pub role: Role,
}

impl Caller {
    pub fn can_control(&self, server_id: &str) -> bool {
        self.role == Role::Admin
            || (self.role == Role::Operator && self.member.servers.iter().any(|s| s == server_id))
    }
}

fn read_json<T: DeserializeOwned + Default>(p: &Path) -> T {
    std::fs::read(p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_json<T: Serialize>(p: &Path, v: &T) -> Result<()> {
    let tmp = p.with_extension("json.partial");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

struct Live {
    presence: HashMap<String, (Presence, u64)>,
    challenges: HashMap<String, (String, u64)>,
    peer_codes: HashMap<String, (String, u64)>,
    inbox: HashMap<String, Vec<GameInvite>>,
}

pub struct Identity {
    root: PathBuf,
    pub profile: Profile,
    pub hub_pubkey: String,
    live: Mutex<Live>,
}

impl Identity {
    pub fn open(root: &Path, hub_name: &str) -> Result<Self> {
        let path = root.join("identity.json");
        let profile = if path.exists() {
            Profile::load(&path)?
        } else {
            let p = Profile::generate(hub_name);
            p.save(&path)?;
            p
        };
        let hub_pubkey = profile.pubkey()?;
        Ok(Identity {
            root: root.to_path_buf(),
            profile,
            hub_pubkey,
            live: Mutex::new(Live {
                presence: HashMap::new(),
                challenges: HashMap::new(),
                peer_codes: HashMap::new(),
                inbox: HashMap::new(),
            }),
        })
    }

    pub fn hub_identity(&self) -> HubIdentity {
        HubIdentity {
            pubkey: self.hub_pubkey.clone(),
            fingerprint: fingerprint(&self.hub_pubkey),
            name: self.profile.name.clone(),
            needs_first_admin: !self.members().iter().any(|m| m.role == "admin"),
        }
    }

    // ---- members and invites --------------------------------------------------------

    pub fn members(&self) -> Vec<Member> {
        read_json::<Vec<Member>>(&self.root.join("members.json"))
    }

    fn save_members(&self, m: &[Member]) -> Result<()> {
        write_json(&self.root.join("members.json"), &m)
    }

    pub fn member(&self, pubkey: &str) -> Option<Member> {
        self.members().into_iter().find(|m| m.pubkey == pubkey)
    }

    pub fn invites(&self) -> Vec<InviteCode> {
        let t = now();
        read_json::<Vec<InviteCode>>(&self.root.join("invites.json"))
            .into_iter()
            .filter(|i| i.expires_at > t)
            .collect()
    }

    fn save_invites(&self, v: &[InviteCode]) -> Result<()> {
        write_json(&self.root.join("invites.json"), &v)
    }

    pub fn create_invite(&self, role: Role, servers: Vec<String>) -> Result<InviteCode> {
        let mut list = self.invites();
        let inv = InviteCode {
            code: invite_code(),
            role: role.as_str().into(),
            servers,
            created_at: now(),
            expires_at: now() + INVITE_SECS,
        };
        list.push(inv.clone());
        self.save_invites(&list)?;
        Ok(inv)
    }

    /// Make sure a fresh hub has a way in: an admin invite, printed once.
    pub fn ensure_first_admin_invite(&self) -> Result<Option<InviteCode>> {
        if self.members().iter().any(|m| m.role == "admin") {
            return Ok(None);
        }
        if let Some(i) = self.invites().into_iter().find(|i| i.role == "admin") {
            return Ok(Some(i));
        }
        Ok(Some(self.create_invite(Role::Admin, Vec::new())?))
    }

    pub fn pair(&self, req: &cell_hub_api::PairRequest) -> Result<Member> {
        let code = normalize_code(&req.code);
        let mut invites = self.invites();
        let Some(pos) = invites.iter().position(|i| i.code == code) else {
            bail!("unknown or expired invite code");
        };
        verify(
            &req.pubkey,
            &pair_message(&self.hub_pubkey, &code),
            &req.signature,
        )
        .context("pairing signature")?;
        let inv = invites.remove(pos);
        self.save_invites(&invites)?;
        let name = req.name.trim();
        if name.is_empty() {
            bail!("a profile name is required");
        }
        let mut members = self.members();
        members.retain(|m| m.pubkey != req.pubkey);
        let m = Member {
            pubkey: req.pubkey.clone(),
            name: name.to_string(),
            avatar: req.avatar.clone(),
            role: inv.role,
            servers: inv.servers,
            added_at: now(),
        };
        members.push(m.clone());
        self.save_members(&members)?;
        Ok(m)
    }

    pub fn update_member(
        &self,
        pubkey: &str,
        role: Option<Role>,
        servers: Option<Vec<String>>,
    ) -> Result<Member> {
        let mut members = self.members();
        let m = members
            .iter_mut()
            .find(|m| m.pubkey == pubkey)
            .context("no such member")?;
        if let Some(r) = role {
            m.role = r.as_str().into();
        }
        if let Some(s) = servers {
            m.servers = s;
        }
        let out = m.clone();
        self.save_members(&members)?;
        Ok(out)
    }

    pub fn remove_member(&self, pubkey: &str) -> Result<()> {
        let mut members = self.members();
        let before = members.len();
        members.retain(|m| m.pubkey != pubkey);
        if members.len() == before {
            bail!("no such member");
        }
        self.save_members(&members)?;
        let mut sessions = self.sessions();
        sessions.retain(|_, (pk, _)| pk != pubkey);
        self.save_sessions(&sessions)?;
        Ok(())
    }

    // ---- sessions -----------------------------------------------------------------------

    fn sessions(&self) -> HashMap<String, (String, u64)> {
        let t = now();
        read_json::<HashMap<String, (String, u64)>>(&self.root.join("sessions.json"))
            .into_iter()
            .filter(|(_, (_, exp))| *exp > t)
            .collect()
    }

    fn save_sessions(&self, s: &HashMap<String, (String, u64)>) -> Result<()> {
        write_json(&self.root.join("sessions.json"), s)
    }

    pub fn challenge(&self, pubkey: &str) -> Result<Challenge> {
        if self.member(pubkey).is_none() {
            bail!("this profile is not paired with the hub");
        }
        let nonce = random_token();
        let expires_at = now() + CHALLENGE_SECS;
        let mut live = self.live.lock().unwrap();
        live.challenges.retain(|_, (_, exp)| *exp > now());
        live.challenges
            .insert(nonce.clone(), (pubkey.to_string(), expires_at));
        Ok(Challenge { nonce, expires_at })
    }

    pub fn login(&self, req: &cell_hub_api::LoginRequest) -> Result<Session> {
        {
            let mut live = self.live.lock().unwrap();
            match live.challenges.remove(&req.nonce) {
                Some((pk, exp)) if pk == req.pubkey && exp > now() => {}
                _ => bail!("unknown or expired challenge"),
            }
        }
        verify(
            &req.pubkey,
            &login_message(&self.hub_pubkey, &req.nonce),
            &req.signature,
        )
        .context("login signature")?;
        let member = self.member(&req.pubkey).context("not a member")?;
        let token = random_token();
        let expires_at = now() + SESSION_DAYS * 86400;
        let mut sessions = self.sessions();
        sessions.insert(token.clone(), (req.pubkey.clone(), expires_at));
        self.save_sessions(&sessions)?;
        Ok(Session {
            token,
            expires_at,
            member,
        })
    }

    pub fn caller_for_token(&self, token: &str) -> Option<Caller> {
        let sessions = self.sessions();
        let (pubkey, _) = sessions.get(token)?;
        let member = self.member(pubkey)?;
        let role = Role::parse(&member.role)?;
        Some(Caller { member, role })
    }

    pub fn logout(&self, token: &str) -> Result<()> {
        let mut sessions = self.sessions();
        sessions.remove(token);
        self.save_sessions(&sessions)
    }

    // ---- presence and peers -----------------------------------------------------------------

    pub fn set_presence(&self, pubkey: &str, p: Presence) {
        let mut live = self.live.lock().unwrap();
        live.presence.insert(pubkey.to_string(), (p, now()));
    }

    fn peers_map(&self) -> HashMap<String, Vec<String>> {
        read_json(&self.root.join("peers.json"))
    }

    pub fn peer_code(&self, pubkey: &str) -> PeerCode {
        let mut live = self.live.lock().unwrap();
        live.peer_codes.retain(|_, (_, exp)| *exp > now());
        let code = invite_code();
        let expires_at = now() + PEER_CODE_SECS;
        live.peer_codes
            .insert(code.clone(), (pubkey.to_string(), expires_at));
        PeerCode { code, expires_at }
    }

    pub fn pair_peer(&self, pubkey: &str, code: &str) -> Result<Member> {
        let code = normalize_code(code);
        let other = {
            let mut live = self.live.lock().unwrap();
            match live.peer_codes.remove(&code) {
                Some((pk, exp)) if exp > now() => pk,
                _ => bail!("unknown or expired peer code"),
            }
        };
        if other == pubkey {
            bail!("that is your own code");
        }
        let other_member = self
            .member(&other)
            .context("the other member left the hub")?;
        let mut map = self.peers_map();
        for (a, b) in [(pubkey, other.as_str()), (other.as_str(), pubkey)] {
            let list = map.entry(a.to_string()).or_default();
            if !list.iter().any(|x| x == b) {
                list.push(b.to_string());
            }
        }
        write_json(&self.root.join("peers.json"), &map)?;
        Ok(other_member)
    }

    pub fn remove_peer(&self, pubkey: &str, other: &str) -> Result<()> {
        let mut map = self.peers_map();
        for (a, b) in [(pubkey, other), (other, pubkey)] {
            if let Some(list) = map.get_mut(a) {
                list.retain(|x| x != b);
            }
        }
        write_json(&self.root.join("peers.json"), &map)
    }

    pub fn peers(&self, pubkey: &str) -> Vec<Peer> {
        let map = self.peers_map();
        let friends = map.get(pubkey).cloned().unwrap_or_default();
        let members = self.members();
        let live = self.live.lock().unwrap();
        friends
            .iter()
            .filter_map(|pk| members.iter().find(|m| &m.pubkey == pk).cloned())
            .map(|m| {
                let p = live.presence.get(&m.pubkey);
                let online = p.map(|(_, t)| now() - *t < ONLINE_SECS).unwrap_or(false);
                Peer {
                    online,
                    status: if online {
                        p.map(|(x, _)| x.status.clone()).unwrap_or_default()
                    } else {
                        "offline".into()
                    },
                    game_id: if online {
                        p.and_then(|(x, _)| x.game_id.clone())
                    } else {
                        None
                    },
                    last_seen: p.map(|(_, t)| *t),
                    member: m,
                }
            })
            .collect()
    }

    pub fn is_peer(&self, a: &str, b: &str) -> bool {
        self.peers_map()
            .get(a)
            .map(|l| l.iter().any(|x| x == b))
            .unwrap_or(false)
    }

    pub fn push_invite(&self, to: &str, invite: GameInvite) {
        let mut live = self.live.lock().unwrap();
        let box_ = live.inbox.entry(to.to_string()).or_default();
        box_.retain(|i| now() - i.created_at < 3600);
        box_.push(invite);
    }

    pub fn inbox(&self, pubkey: &str) -> Vec<GameInvite> {
        let live = self.live.lock().unwrap();
        live.inbox.get(pubkey).cloned().unwrap_or_default()
    }

    pub fn dismiss(&self, pubkey: &str, id: &str) {
        let mut live = self.live.lock().unwrap();
        if let Some(b) = live.inbox.get_mut(pubkey) {
            b.retain(|i| i.id != id);
        }
    }
}
