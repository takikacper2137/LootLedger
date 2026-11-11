import test from "node:test";
import assert from "node:assert/strict";
import { processRequest, makeSnapshot, isChest } from "./src/index.js";

function session() {
  return {
    id: "12345678-1234-4234-8234-123456789abc",
    joinCode: "join-secret",
    ownerId: "host-epic",
    ownerName: "Host",
    ownerToken: "host-token",
    members: {
      "host-epic": { name: "Host", token: "host-token", connected: true, owner: true }
    },
    contributions: {},
    active: true
  };
}

function contribution(playerId = "host-epic", name = "Host") {
  return {
    protocolVersion: 1,
    matchSessionId: "match-123",
    submittedBy: playerId,
    durationSeconds: 120,
    owner: { epicId: playerId, name },
    teammates: [],
    events: [
      { t: 10, cls: "Tiered_Chest_Athena_C", kind: "opened", by: "me" },
      { t: 20, cls: "AmmoBox_Athena_C", kind: "opened", by: "me" },
      { t: 30, cls: "Chest_Athena_C", kind: "opened", by: "team" }
    ]
  };
}

test("only a matching session id can upload or read a snapshot", () => {
  const s = session();
  const wrongSession = processRequest(s, {
    action: "upload", session_id: "another-session", participant_token: "host-token",
    contribution: contribution()
  });
  assert.equal(wrongSession.status, 403);

  const wrongSnapshot = processRequest(s, {
    action: "snapshot", session_id: "another-session", participant_token: "host-token"
  });
  assert.equal(wrongSnapshot.status, 403);
});

test("joining, uploading and snapshot aggregation work", () => {
  const s = session();
  const joined = processRequest(s, {
    action: "join", join_code: "join-secret", player_id: "friend-epic", player_name: "Friend"
  });
  assert.equal(joined.status, 200);
  const token = joined.body.credentials.participantToken;

  const upload = processRequest(s, {
    action: "upload", session_id: s.id, participant_token: token,
    contribution: contribution("friend-epic", "Friend")
  });
  assert.equal(upload.status, 200);

  const snapshot = makeSnapshot(s);
  assert.equal(snapshot.matches.length, 1);
  assert.equal(snapshot.matches[0].contributions, 1);
  assert.equal(snapshot.matches[0].uniqueOpenings, 1);
  assert.equal(snapshot.matches[0].players[0].playerName, "Friend");
});

test("a disconnected participant cannot upload or leave twice", () => {
  const s = session();
  const joined = processRequest(s, {
    action: "join", join_code: "join-secret", player_id: "friend-epic", player_name: "Friend"
  });
  const token = joined.body.credentials.participantToken;
  assert.equal(processRequest(s, {
    action: "leave", session_id: s.id, participant_token: token
  }).status, 200);

  assert.equal(processRequest(s, {
    action: "upload", session_id: s.id, participant_token: token,
    contribution: contribution("friend-epic", "Friend")
  }).status, 403);
  assert.equal(processRequest(s, {
    action: "leave", session_id: s.id, participant_token: token
  }).status, 403);
});

test("only the owner can end the session", () => {
  const s = session();
  assert.equal(processRequest(s, {
    action: "end", session_id: s.id, owner_token: "not-the-owner"
  }).status, 403);
  assert.equal(processRequest(s, {
    action: "end", session_id: s.id, owner_token: s.ownerToken
  }).status, 200);
  assert.equal(s.active, false);
});

test("chest classification matches the Rust model's ordered rules", () => {
  assert.equal(isChest("Tiered_Chest_Athena_C"), true);
  assert.equal(isChest("RareChest_Athena_C"), true);
  assert.equal(isChest("AmmoBox_Athena_C"), false);
  assert.equal(isChest("FoodBox_Athena_C"), false);
  assert.equal(isChest("FishingBarrel_Athena_C"), false);
  assert.equal(isChest("OtherContainer_Athena_C"), false);
});
