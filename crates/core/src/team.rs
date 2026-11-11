//! Team-session wire types and validation.
//!
//! Only parsed replay facts are sent to the session host. Local replay paths and raw
//! replay bytes are deliberately not part of this protocol.
use crate::model::{Opening, Player, Replay};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_EVENTS_PER_REPLAY: usize = 20_000;
pub const MAX_PLAYERS_PER_REPLAY: usize = 100;

/// A minimal projection of a locally parsed replay for the team-session protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayContribution {
    pub protocol_version: u16,
    pub match_session_id: String,
    pub submitted_by: String,
    pub duration_seconds: f64,
    pub timestamp: Option<String>,
    pub playlist: Option<String>,
    pub placement: Option<i64>,
    pub total_players: Option<i64>,
    pub eliminations: Option<i64>,
    pub owner: Player,
    pub teammates: Vec<Player>,
    pub events: Vec<Opening>,
}

impl ReplayContribution {
    /// Build a network-safe contribution. The local filename/path and raw replay are omitted.
    pub fn from_replay(replay: &Replay) -> Result<Self, String> {
        replay.validate()?;
        let match_session_id = replay
            .session_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "Replay has no shared match session ID".to_owned())?;
        let owner = replay
            .owner
            .clone()
            .ok_or_else(|| "Replay has no owner identity".to_owned())?;
        let submitted_by = owner
            .epic_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "Replay owner has no Epic account ID".to_owned())?
            .to_owned();

        let contribution = Self {
            protocol_version: PROTOCOL_VERSION,
            match_session_id: match_session_id.to_owned(),
            submitted_by,
            duration_seconds: replay.length_seconds,
            timestamp: replay.timestamp.clone(),
            playlist: replay.playlist.clone(),
            placement: replay.placement,
            total_players: replay.total_players,
            eliminations: replay.eliminations,
            owner,
            teammates: replay.teammates.clone(),
            events: replay.events.clone(),
        };
        contribution.validate()?;
        Ok(contribution)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err("Unsupported team-session protocol version".into());
        }
        if self.match_session_id.trim().is_empty() || self.match_session_id.len() > 256 {
            return Err("Invalid match session ID".into());
        }
        if self.submitted_by.trim().is_empty() || self.submitted_by.len() > 256 {
            return Err("Invalid submitting player ID".into());
        }
        if self.owner.epic_id.as_deref() != Some(self.submitted_by.as_str()) {
            return Err("Contribution owner does not match submitting player".into());
        }
        if !self.duration_seconds.is_finite()
            || !(0.0..=6.0 * 60.0 * 60.0).contains(&self.duration_seconds)
        {
            return Err("Invalid match duration".into());
        }
        if self.events.len() > MAX_EVENTS_PER_REPLAY {
            return Err("Replay contains too many events".into());
        }
        if self.teammates.len() > MAX_PLAYERS_PER_REPLAY {
            return Err("Replay contains too many players".into());
        }
        for event in &self.events {
            if !event.t.is_finite()
                || event.t < 0.0
                || event.t > self.duration_seconds + 2.0
                || event.cls.len() > 512
                || event.player.as_ref().is_some_and(|s| s.len() > 256)
                || event.player_id.as_ref().is_some_and(|s| s.len() > 256)
            {
                return Err("Invalid replay event in contribution".into());
            }
        }
        for player in std::iter::once(&self.owner).chain(self.teammates.iter()) {
            if player.epic_id.as_ref().is_some_and(|s| s.len() > 256)
                || player.name.as_ref().is_some_and(|s| s.len() > 256)
            {
                return Err("Invalid player information in contribution".into());
            }
        }
        Ok(())
    }
}

/// A candidate identity for the *same player's* opening seen in different perspectives.
///
/// This is intentionally not exposed as a globally unique event ID: Fortnite replay actor
/// IDs are local to each recording. Callers must retain the source perspective and treat
/// matching fingerprints as corroboration, not proof that two events are identical.
pub fn opening_fingerprint(
    match_session_id: &str,
    player_epic_id: &str,
    event: &Opening,
) -> String {
    let time_bucket = (event.t * 4.0).round() as i64; // 250 ms corroboration window
    format!(
        "{}:{}:{}:{}:{}",
        match_session_id,
        player_epic_id,
        time_bucket,
        event.cls.to_ascii_lowercase(),
        event.kind
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_ignores_local_actor_ids() {
        let a = Opening {
            t: 12.01,
            cls: "Tiered_Chest_Athena_C".into(),
            kind: "opened".into(),
            by: "me".into(),
            player: None,
            player_id: Some("epic-123".into()),
            pawn: Some(4),
            actor: 100,
            episode: 0,
        };
        let mut b = a.clone();
        b.t = 12.04;
        b.actor = 900;
        b.episode = 7;
        assert_eq!(
            opening_fingerprint("match-1", "epic-123", &a),
            opening_fingerprint("match-1", "epic-123", &b)
        );
    }

    #[test]
    fn fingerprint_keeps_distinct_players_separate() {
        let event = Opening {
            t: 12.0,
            cls: "Tiered_Chest_Athena_C".into(),
            kind: "opened".into(),
            by: "me".into(),
            player: None,
            player_id: Some("epic-123".into()),
            pawn: None,
            actor: 1,
            episode: 0,
        };
        assert_ne!(
            opening_fingerprint("match-1", "epic-123", &event),
            opening_fingerprint("match-1", "epic-456", &event)
        );
    }
}
