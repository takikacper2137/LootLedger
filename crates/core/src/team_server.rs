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

const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
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
        let listener =
            TcpListener::bind(address).map_err(|e| format!("Could not start team server: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| e.to_string())?;
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

        Ok(Self {
            address,
            stop,
            thread: Some(thread),
            state: local_state,
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn snapshot_local(
        &self,
        credentials: &SessionCredentials,
    ) -> Result<SessionSnapshot, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "Session state unavailable".to_owned())?;
        let session = state
            .session
            .as_ref()
            .filter(|s| s.id == credentials.session_id && s.active)
            .ok_or_else(|| "Session is not active".to_owned())?;
        if session.owner_token != credentials.owner_token {
            return Err("Invalid host credentials".into());
        }
        Ok(snapshot(session))
    }

    pub fn upload_local(
        &self,
        credentials: &SessionCredentials,
        contribution: ReplayContribution,
    ) -> Result<(), String> {
        let response = process_request(
            Request::Upload {
                session_id: credentials.session_id.clone(),
                participant_token: credentials.owner_token.clone(),
                contribution,
            },
            &self.state,
        );
        if response["ok"] == true {
            Ok(())
        } else {
            Err(response["error"]
                .as_str()
                .unwrap_or("Could not upload replay")
                .to_owned())
        }
    }

    pub fn end_session(&self, credentials: &SessionCredentials) -> Result<(), String> {
        let response = process_request(
            Request::End {
                session_id: credentials.session_id.clone(),
                owner_token: credentials.owner_token.clone(),
            },
            &self.state,
        );
        if response["ok"] == true {
            Ok(())
        } else {
            Err(response["error"]
                .as_str()
                .unwrap_or("Could not end session")
                .to_owned())
        }
    }

    /// Create the one host-owned session supported by this MVP.
    pub fn create_session(
        &self,
        owner_id: &str,
        owner_name: &str,
    ) -> Result<SessionCredentials, String> {
        if owner_id.trim().is_empty() || owner_id.len() > 256 || owner_name.len() > 256 {
            return Err("Invalid host identity".into());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Session state unavailable".to_owned())?;
        if state.session.as_ref().is_some_and(|s| s.active) {
            return Err("A team session is already active on this PC".into());
        }

        let credentials = SessionCredentials {
            session_id: Uuid::new_v4().to_string(),
            join_code: Uuid::new_v4().simple().to_string(),
            owner_token: Uuid::new_v4().to_string(),
        };
        let mut members = HashMap::new();
        members.insert(
            owner_id.to_owned(),
            Member {
                name: owner_name.to_owned(),
                token: credentials.owner_token.clone(),
                connected: true,
            },
        );
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
    let cloned = match stream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let mut reader = BufReader::new(cloned);
    let mut line = String::new();
    if reader
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_line(&mut line)
        .is_err()
        || line.len() > MAX_REQUEST_BYTES
    {
        write_response(
            &mut stream,
            json!({"ok": false, "error": "Request too large or unreadable"}),
        );
        return;
    }

    let response = match serde_json::from_str::<Request>(&line) {
        Ok(request) => process_request(request, &state),
        Err(_) => json!({"ok": false, "error": "Invalid request"}),
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
    let Ok(mut guard) = state.lock() else {
        return json!({"ok": false, "error": "Session lock unavailable"});
    };

    match request {
        Request::Join {
            join_code,
            player_id,
            player_name,
        } => {
            let Some(session) = guard
                .session
                .as_mut()
                .filter(|s| s.active && s.join_code == join_code)
            else {
                return json!({"ok": false, "error": "Invalid or closed session code"});
            };
            if player_id.trim().is_empty() || player_id.len() > 256 || player_name.len() > 256 {
                return json!({"ok": false, "error": "Invalid player identity"});
            }
            if session.members.contains_key(&player_id) {
                return json!({"ok": false, "error": "This player has already joined the session"});
            }
            if session.members.len() >= MAX_PARTICIPANTS {
                return json!({"ok": false, "error": "Session is full"});
            }

            let token = Uuid::new_v4().to_string();
            session.members.insert(
                player_id,
                Member {
                    name: player_name,
                    token: token.clone(),
                    connected: true,
                },
            );
            json!({
                "ok": true,
                "credentials": {
                    "sessionId": session.id,
                    "participantToken": token
                }
            })
        }
        Request::Upload {
            session_id,
            participant_token,
            contribution,
        } => {
            if let Err(error) = contribution.validate() {
                return json!({"ok": false, "error": error});
            }
            let Some(session) = guard
                .session
                .as_mut()
                .filter(|s| s.active && s.id == session_id)
            else {
                return json!({"ok": false, "error": "Session is not active"});
            };
            let Some(member) = session.members.get(&contribution.submitted_by) else {
                return json!({"ok": false, "error": "Player has not joined this session"});
            };
            if member.token != participant_token {
                return json!({"ok": false, "error": "Invalid participant token"});
            }

            session.contributions.insert(
                (
                    contribution.match_session_id.clone(),
                    contribution.submitted_by.clone(),
                ),
                contribution,
            );
            json!({"ok": true})
        }
        Request::Snapshot {
            session_id,
            participant_token,
        } => {
            let Some(session) = guard
                .session
                .as_ref()
                .filter(|s| s.active && s.id == session_id)
            else {
                return json!({"ok": false, "error": "Session is not active"});
            };
            if !session
                .members
                .values()
                .any(|member| member.token == participant_token)
            {
                return json!({"ok": false, "error": "Invalid participant token"});
            }
            json!({"ok": true, "snapshot": snapshot(session)})
        }
        Request::Leave {
            session_id,
            participant_token,
        } => {
            let Some(session) = guard
                .session
                .as_mut()
                .filter(|s| s.active && s.id == session_id)
            else {
                return json!({"ok": false, "error": "Session is not active"});
            };
            let Some((_, member)) = session
                .members
                .iter_mut()
                .find(|(_, member)| member.token == participant_token)
            else {
                return json!({"ok": false, "error": "Invalid participant token"});
            };
            member.connected = false;
            json!({"ok": true})
        }
        Request::End {
            session_id,
            owner_token,
        } => {
            let Some(session) = guard
                .session
                .as_mut()
                .filter(|s| s.active && s.id == session_id)
            else {
                return json!({"ok": false, "error": "Session is not active"});
            };
            if session.owner_token != owner_token {
                return json!({"ok": false, "error": "Only the session owner can end it"});
            }
            session.active = false;
            json!({"ok": true})
        }
    }
}

fn snapshot(session: &Session) -> SessionSnapshot {
    let participants = session
        .members
        .iter()
        .map(|(id, member)| Participant {
            player_id: id.clone(),
            player_name: member.name.clone(),
            connected: member.connected,
        })
        .collect();

    let mut by_match: HashMap<String, Vec<&ReplayContribution>> = HashMap::new();
    for contribution in session.contributions.values() {
        by_match
            .entry(contribution.match_session_id.clone())
            .or_default()
            .push(contribution);
    }

    let mut matches = by_match
        .into_iter()
        .map(|(match_session_id, contributions)| {
            let mut players: HashMap<String, (String, usize)> = HashMap::new();
            for contribution in &contributions {
                // Count only "me" events from the replay recorded by that player. Other
                // perspectives are retained for later corroboration, not added twice.
                let count = contribution
                    .events
                    .iter()
                    .filter(|event| {
                        event.by == "me" && crate::model::classify(&event.cls).1
                    })
                    .count();
                let name = contribution
                    .owner
                    .name
                    .clone()
                    .unwrap_or_else(|| contribution.submitted_by.clone());
                players.insert(contribution.submitted_by.clone(), (name, count));
            }
            let unique_openings = players.values().map(|(_, count)| *count).sum();
            let players = players
                .into_iter()
                .map(|(player_id, (player_name, openings))| PlayerSnapshot {
                    player_id,
                    player_name,
                    openings,
                })
                .collect();
            MatchSnapshot {
                match_session_id,
                contributions: contributions.len(),
                players,
                unique_openings,
            }
        })
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| a.match_session_id.cmp(&b.match_session_id));

    SessionSnapshot {
        session_id: session.id.clone(),
        owner_id: session.owner_id.clone(),
        owner_name: session.owner_name.clone(),
        active: session.active,
        participants,
        matches,
    }
}

/// Send one request to a direct-host session server.
pub fn send_request(address: SocketAddr, request: &Request) -> Result<Value, String> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
        .map_err(|e| format!("Could not connect to host: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| e.to_string())?;

    let mut bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("Request is too large".into());
    }
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .map_err(|e| format!("Could not send request: {e}"))?;

    let mut response = String::new();
    BufReader::new(stream)
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_line(&mut response)
        .map_err(|e| format!("Could not read host response: {e}"))?;
    if response.len() > MAX_REQUEST_BYTES {
        return Err("Host response is too large".into());
    }
    let value: Value = serde_json::from_str(&response)
        .map_err(|e| format!("Invalid host response: {e}"))?;
    if value["ok"] == true {
        Ok(value)
    } else {
        Err(value["error"]
            .as_str()
            .unwrap_or("Host rejected the request")
            .to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participants_can_join_but_cannot_end_the_host_session() {
        let server = TeamServer::start("127.0.0.1:0".parse().unwrap()).unwrap();
        let host = server.create_session("host-epic", "Host").unwrap();
        let joined = send_request(
            server.address(),
            &Request::Join {
                join_code: host.join_code.clone(),
                player_id: "friend-epic".into(),
                player_name: "Friend".into(),
            },
        )
        .unwrap();
        let participant: ParticipantCredentials =
            serde_json::from_value(joined["credentials"].clone()).unwrap();

        let attempt = send_request(
            server.address(),
            &Request::End {
                session_id: participant.session_id,
                owner_token: participant.participant_token,
            },
        );
        assert!(
            attempt.is_err(),
            "a participant must not be able to end the session"
        );

        let snapshot = server.snapshot_local(&host).unwrap();
        assert_eq!(snapshot.participants.len(), 2);
        assert!(snapshot.active);
        server.end_session(&host).unwrap();
        assert!(server.snapshot_local(&host).is_err());
    }

    #[test]
    fn invalid_join_codes_are_rejected() {
        let server = TeamServer::start("127.0.0.1:0".parse().unwrap()).unwrap();
        let _host = server.create_session("host-epic", "Host").unwrap();
        let result = send_request(
            server.address(),
            &Request::Join {
                join_code: "not-the-code".into(),
                player_id: "friend-epic".into(),
                player_name: "Friend".into(),
            },
        );
        assert!(result.is_err());
    }
}
