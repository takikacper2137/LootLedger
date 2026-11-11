//! Small direct-host transport for team sessions.
//!
//! The transport uses one JSON request per TCP connection. It intentionally avoids
//! exposing the local replay folder or raw replay files. A host keeps the server running
//! while the session is active; participants connect to the host's reachable address.
use crate::team::ReplayContribution;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use uuid::Uuid;

const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_PARTICIPANTS: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Request {
    Join {
        join_code: String,
        player_id: String,
        player_name: String,
    },
    Upload {
        session_id: String,
        participant_token: String,
        contribution: ReplayContribution,
    },
    Snapshot {
        session_id: String,
        participant_token: String,
    },
    Leave {
        session_id: String,
        participant_token: String,
    },
    End {
        session_id: String,
        owner_token: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCredentials {
    pub session_id: String,
    pub join_code: String,
    pub owner_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipantCredentials {
    pub session_id: String,
    pub participant_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session_id: String,
    pub owner_id: String,
    pub owner_name: String,
    pub active: bool,
    pub participants: Vec<Participant>,
    pub matches: Vec<MatchSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub player_id: String,
    pub player_name: String,
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchSnapshot {
    pub match_session_id: String,
    pub contributions: usize,
    pub players: Vec<PlayerSnapshot>,
    pub unique_openings: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSnapshot {
    pub player_id: String,
    pub player_name: String,
    pub openings: usize,
}

#[derive(Debug)]
struct Member {
    name: String,
    token: String,
    connected: bool,
}

#[derive(Debug)]
struct Session {
    id: String,
    join_code: String,
    owner_id: String,
    owner_name: String,
    owner_token: String,
    members: HashMap<String, Member>,
    contributions: HashMap<(String, String), ReplayContribution>,
    active: bool,
}

#[derive(Default)]
struct State {
    session: Option<Session>,
}

pub struct TeamServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    state: Arc<Mutex<State>>,
}

impl TeamServer {
    /// Bind to the requested address. Use 0.0.0.0 to accept connections from other PCs.
    pub fn start(address: SocketAddr) -> Result<Self, String> {
        let listener = TcpListener::bind(address).map_err(|e| format!("Could not start team server: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(State::default()));
        let local_state = state.clone();
        let thread_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("lootledger-team-server".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let state = state.clone();
                            let _ = thread::Builder::new()
                                .name("lootledger-team-client".into())
                                .spawn(move || handle_connection(stream, state));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(30));
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { address, stop, thread: Some(thread), state: local_state })
    }

    pub fn address(&self) -> SocketAddr { self.address }

    /// Create the one host-owned session supported by this MVP.
    pub fn create_session(&self, owner_id: &str, owner_name: &str) -> Result<SessionCredentials, String> {
        if owner_id.trim().is_empty() || owner_id.len() > 256 || owner_name.len() > 256 {
            return Err("Invalid host identity".into());
        }
        let mut state = self.state.lock().map_err(|_| "Session state unavailable".to_owned())?;
        if state.session.as_ref().is_some_and(|s| s.active) {
            return Err("A team session is already active on this PC".into());
        }
        let credentials = SessionCredentials {
            session_id: Uuid::new_v4().to_string(),
            join_code: Uuid::new_v4().simple().to_string(),
            owner_token: Uuid::new_v4().to_string(),
        };
        let mut members = HashMap::new();
        members.insert(owner_id.to_owned(), Member {
            name: owner_name.to_owned(),
            token: credentials.owner_token.clone(),
            connected: true,
        });
        state.session = Some(Session {
            id: credentials.session_id.clone(),
            join_code: credentials.join_code.clone(),
            owner_id: owner_id.to_owned(),
            owner_name: owner_name.to_owned(),
            owner_token: credentials.owner_token.clone(),
            members,
            contributions: HashMap::new(),
            active: true,
        });
        Ok(credentials)
    }


}

impl Drop for TeamServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle_connection(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let cloned = match stream.try_clone() { Ok(s) => s, Err(_) => return };
    let mut reader = BufReader::new(cloned);
    let mut line = String::new();
    if reader.take((MAX_REQUEST_BYTES + 1) as u64).read_line(&mut line).is_err()
        || line.len() > MAX_REQUEST_BYTES
    {
        write_response(&mut stream, json!({"ok":false,"error":"Request too large or unreadable"}));
        return;
    }
    let response = match serde_json::from_str::<Request>(&line) {
        Ok(request) => process_request(request, &state),
        Err(_) => json!({"ok":false,"error":"Invalid request"}),
    };
    write_response(&mut stream, response);
}

fn write_response(stream: &mut TcpStream, value: Value) {
    if let Ok(mut bytes) = serde_json::to_vec(&value) {
        bytes.push(b'\n');
        let _ = stream.write_all(&bytes);
    }
}

fn process_request(request: Request, state: &Arc<Mutex<State>>) -> Value {
    let Ok(mut guard) = state.lock() else { return json!({"ok":false,"error":"Session lock unavailable"}) };
    match request {
        Request::Join { join_code, player_id, player_name } => {
            let Some(session) = guard.session.as_mut().filter(|s| s.active && s.join_code == join_code) else {
                return json!({"ok":false,"error":"Invalid or closed session code"});
            };
            if player_id.trim().is_empty() || player_id.len() > 256 || player_name.len() > 256 {
                return json!({"ok":false,"error":"Invalid player identity"});
            }
            if !session.members.contains_key(&player_id) && session.members.len() >= MAX_PARTICIPANTS {
                return json!({"ok":false,"error":"Session is full"});
            }
            let token = Uuid::new_v4().to_string();
            session.members.insert(player_id, Member { name: player_name, token: token.clone(), connected: true });
            json!({"ok":true,"credentials":{"sessionId":session.id,"participantToken":token}})
        }
        Request::Upload { session_id, participant_token, contribution } => {
            if let Err(e) = contribution.validate() { return json!({"ok":false,"error":e}); }
            let Some(session) = guard.session.as_mut().filter(|s| s.active && s.id == session_id) else {
                return json!({"ok":false,"error":"Session is not active"});
            };
            let Some(member) = session.members.get(&contribution.submitted_by) else {
                return json!({"ok":false,"error":"Player has not joined this session"});
            };
            if member.token != participant_token {
                return json!({"ok":false,"error":"Invalid participant token"});
            }
            session.contributions.insert((contribution.match_session_id.clone(), contribution.submitted_by.clone()), contribution);
            json!({"ok":true})
        }
        Request::Snapshot { session_id, participant_token } => {
            let Some(session) = guard.session.as_ref().filter(|s| s.active && s.id == session_id) else {
                return json!({"ok":false,"error":"Session is not active"});
            };
            if !session.members.values().any(|m| m.token == participant_token) {
                return json!({"ok":false,"error":"Invalid participant token"});
            }
            json!({"ok":true,"snapshot":snapshot(session)})
        }
        Request::Leave { session_id, participant_token } => {
            let Some(session) = guard.session.as_mut().filter(|s| s.active && s.id == session_id) else {
                return json!({"ok":false,"error":"Session is not active"});
            };
            let Some((_, member)) = session.members.iter_mut().find(|(_, m)| m.token == participant_token) else {
                return json!({"ok":false,"error":"Invalid participant token"});
            };
            member.connected = false;
            json!({"ok":true})
        }
        Request::End { session_id, owner_token } => {
            let Some(session) = guard.session.as_mut().filter(|s| s.active && s.id == session_id) else {
                return json!({"ok":false,"error":"Session is not active"});
            };
            if session.owner_token != owner_token {
                return json!({"ok":false,"error":"Only the session owner can end it"});
            }
            session.active = false;
            json!({"ok":true})
        }
    }
}

fn snapshot(session: &Session) -> SessionSnapshot {
    let participants = session.members.iter().map(|(id, member)| Participant {
        player_id: id.clone(), player_name: member.name.clone(), connected: member.connected,
    }).collect();
    let mut by_match: HashMap<String, Vec<&ReplayContribution>> = HashMap::new();
    for contribution in session.contributions.values() {
        by_match.entry(contribution.match_session_id.clone()).or_default().push(contribution);
    }
    let mut matches = by_match.into_iter().map(|(match_session_id, contributions)| {
        let mut players: HashMap<String, (String, usize)> = HashMap::new();
        for contribution in &contributions {
            // Count only "me" events from the replay recorded by that player. Other
            // perspectives are retained for later corroboration, not added twice.
            let count = contribution.events.iter().filter(|event| event.by == "me").count();
            let name = contribution.owner.name.clone().unwrap_or_else(|| contribution.submitted_by.clone());
            players.insert(contribution.submitted_by.clone(), (name, count));
        }
        let unique_openings = players.values().map(|(_, count)| *count).sum();
        let players = players.into_iter().map(|(player_id, (player_name, openings))| PlayerSnapshot {
            player_id, player_name, openings,
        }).collect();
        MatchSnapshot { match_session_id, contributions: contributions.len(), players, unique_openings }
    }).collect::<Vec<_>>();
    matches.sort_by(|a, b| a.match_session_id.cmp(&b.match_session_id));
    SessionSnapshot {
        session_id: session.id.clone(), owner_id: session.owner_id.clone(),
        owner_name: session.owner_name.clone(), active: session.active, participants, matches,
    }
}
