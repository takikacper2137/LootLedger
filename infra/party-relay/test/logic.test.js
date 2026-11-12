import test from "node:test";
import assert from "node:assert/strict";
import { LIMITS, sync, validRoom } from "../src/logic.js";

const player = (key, name, me, total) => ({ key, name, me, counts: { Chest: { opened: total, broken: 0 } } });
// A match summary as one recording sees it: `me` is the recorder.
const match = (session, owner, players) => ({
  key: `${session}:${owner}`,
  meta: { key: `${session}:${owner}`, players },
});
const request = (who, keys, offer, digest = "d1") => ({ v: 1, who, keys, digest, offer });
const NOW = 1_800_000_000_000;

test("room codes are 12 to 40 letters or digits", () => {
  assert.ok(validRoom("abcdef123456"));
  assert.ok(!validRoom("short"));
  assert.ok(!validRoom("has-a-dash-in-it-here"));
  assert.ok(!validRoom("x".repeat(41)));
  assert.ok(!validRoom(undefined));
});

test("a friend gets the matches they were in, and only those", () => {
  const state = { members: {} };
  const kacper = request("K", ["K"], [
    match("s1", "K", [player("K", "Kacper", true, 6), player("O", "Ola", false, 7)]),
    match("s2", "K", [player("K", "Kacper", true, 3), player("X", "Stranger", false, 2)]),
  ]);
  const first = sync(state, kacper, NOW);
  assert.equal(first.status, 200);
  assert.deepEqual(first.body.offer, []);
  assert.equal(first.body.have, "d1");

  const ola = request("O", ["O"], [match("s1", "O", [player("O", "Ola", true, 2), player("K", "Kacper", false, 1)])]);
  const reply = sync(state, ola, NOW + 1000);
  assert.deepEqual(reply.body.offer.map((o) => o.key), ["s1:K"]);
  assert.equal(reply.body.offer[0].meta.players[1].counts.Chest.opened, 7);
  assert.equal(reply.body.offer[0].received, new Date(NOW + 1000).toISOString());
  // Kacper's next sync brings Ola's side of s1.
  const again = sync(state, request("K", ["K"], null), NOW + 2000);
  assert.deepEqual(again.body.offer.map((o) => o.key), ["s1:O"]);
});

test("you never get your own recordings back, and a stranger's matches stay out", () => {
  const state = { members: {} };
  sync(state, request("K", ["K"], [match("s1", "K", [player("K", "K", true, 1), player("O", "O", false, 1)])]), NOW);
  const own = sync(state, request("K", ["K"], null), NOW + 1);
  assert.deepEqual(own.body.offer, []);
  const stranger = sync(state, request("Z", ["Z"], [match("s9", "Z", [player("Z", "Z", true, 1)])], "dz"), NOW + 2);
  assert.deepEqual(stranger.body.offer, []);
});

test("without a match summary list the room keeps what it has, if the digest matches", () => {
  const state = { members: {} };
  sync(state, request("K", ["K"], [match("s1", "K", [player("K", "K", true, 1)])], "d1"), NOW);
  const same = sync(state, request("K", ["K"], null, "d1"), NOW + 5);
  assert.equal(same.body.have, "d1");
  assert.equal(state.members.K.updated, NOW + 5);
  // A changed digest with no list: the room says what it holds so the client sends the list next.
  const stale = sync(state, request("K", ["K"], null, "d2"), NOW + 6);
  assert.equal(stale.body.have, "d1");
  // A room that forgot this person says so too.
  const forgot = sync({ members: {} }, request("K", ["K"], null, "d1"), NOW);
  assert.equal(forgot.body.have, null);
});

test("a person who has not synced for a day drops out of the room", () => {
  const state = { members: {} };
  sync(state, request("K", ["K"], [match("s1", "K", [player("K", "K", true, 1), player("O", "O", false, 1)])]), NOW);
  const later = sync(state, request("O", ["O"], []), NOW + LIMITS.ttlMs + 1);
  assert.deepEqual(later.body.offer, []);
  assert.deepEqual(Object.keys(state.members), ["O"]);
});

test("the room holds at most sixteen people", () => {
  const state = { members: {} };
  for (let i = 0; i < LIMITS.members; i++) assert.equal(sync(state, request(`p${i}`, [`p${i}`], []), NOW).status, 200);
  const full = sync(state, request("late", ["late"], []), NOW);
  assert.equal(full.status, 409);
  // Someone already inside can still sync.
  assert.equal(sync(state, request("p0", ["p0"], []), NOW).status, 200);
});

test("malformed requests are refused", () => {
  const bad = [
    null,
    { v: 2 },
    { v: 1, who: "", keys: ["a"], digest: "d" },
    { v: 1, who: "a", keys: [], digest: "d" },
    { v: 1, who: "a", keys: ["a"], digest: "" },
    request("a", ["a"], "not a list"),
    request("a", ["a"], [{ key: "s:a", meta: { key: "other", players: [] } }]),
    request("a", ["a"], [{ key: "s:a", meta: { key: "s:a", players: "no" } }]),
    request("a", ["a"], Array.from({ length: LIMITS.offers + 1 }, (_, i) => match(`s${i}`, "a", []))),
  ];
  for (const b of bad) assert.equal(sync({ members: {} }, b, NOW).status, 400, JSON.stringify(b)?.slice(0, 60));
});

test("everyone in the room is listed with their name and how long ago they were seen", () => {
  const state = { members: {} };
  sync(state, { ...request("K", ["K"], []), name: "Kacper" }, NOW);
  const reply = sync(state, { ...request("O", ["O"], []), name: "Ola" }, NOW + 30_000);
  assert.deepEqual(reply.body.members, [
    { name: "Kacper", seen: 30, me: false },
    { name: "Ola", seen: 0, me: true },
  ]);
});
