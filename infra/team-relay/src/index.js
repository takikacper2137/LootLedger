const MAX_REQUEST_BYTES = 8 * 1024 * 1024;
const MAX_PARTICIPANTS = 16;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (request.method === "POST" && url.pathname === "/v1/sessions") {
      const body = await readJson(request);
      if (!body || !validIdentity(body.ownerId, body.ownerName)) return json({ ok: false, error: "Invalid host identity" }, 400);
      const sessionId = crypto.randomUUID();
      const stub = env.SESSIONS.get(env.SESSIONS.idFromName(sessionId));
      return stub.fetch("https://session.internal/create", {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ sessionId, ...body }),
      });
    }
    const match = url.pathname.match(/^\/v1\/sessions\/([0-9a-f-]{36})\/request$/i);
    if (request.method === "POST" && match) {
      const stub = env.SESSIONS.get(env.SESSIONS.idFromName(match[1]));
      return stub.fetch("https://session.internal/request", {
        method: "POST", headers: { "content-type": "application/json" }, body: request.body,
      });
    }
    return json({ ok: false, error: "Not found" }, 404);
  },
};

export class SessionRoom {
  constructor(state) { this.state = state; this.storage = state.storage; }

  async fetch(request) {
    return this.state.blockConcurrencyWhile(async () => {
      if (request.method !== "POST") return json({ ok: false, error: "Method not allowed" }, 405);
      const body = await readJson(request);
      if (!body) return json({ ok: false, error: "Invalid or oversized JSON request" }, 400);

      if (new URL(request.url).pathname.endsWith("/create")) {
        if (await this.storage.get("session")) return json({ ok: false, error: "Session already exists" }, 409);
        if (!validIdentity(body.ownerId, body.ownerName) || !isUuid(body.sessionId)) return json({ ok: false, error: "Invalid host identity" }, 400);
        const session = {
          id: body.sessionId, joinCode: randomToken(), ownerId: body.ownerId, ownerName: body.ownerName,
          ownerToken: randomToken(),
          members: { [body.ownerId]: { name: body.ownerName, token: null, connected: true, owner: true } },
          contributions: {}, active: true,
        };
        session.members[body.ownerId].token = session.ownerToken;
        await this.storage.put("session", session);
        return json({ ok: true, credentials: { sessionId: session.id, joinCode: session.joinCode, ownerToken: session.ownerToken } });
      }

      const session = await this.storage.get("session");
      if (!session || !session.active) return json({ ok: false, error: "Session is not active" }, 404);
      const result = processRequest(session, body);
      if (result.status === 200) await this.storage.put("session", session);
      return json(result.body, result.status);
    });
  }
}

function processRequest(session, request) {
  switch (request.action) {
    case "join": {
      const { join_code, player_id, player_name } = request;
      if (join_code !== session.joinCode) return fail("Invalid or closed session code", 403);
      if (!validIdentity(player_id, player_name)) return fail("Invalid player identity", 400);
      const existing = session.members[player_id];
      if (existing?.connected) return fail("This player is already connected", 409);
      if (!existing && Object.keys(session.members).length >= MAX_PARTICIPANTS) return fail("Session is full", 409);
      const token = randomToken();
      session.members[player_id] = { name: player_name, token, connected: true, owner: false };
      return ok({ credentials: { sessionId: session.id, participantToken: token } });
    }
    case "upload": {
      const c = request.contribution;
      if (!c || typeof c !== "object" || !validText(c.match_session_id, 256) || !validText(c.submitted_by, 256) ||
          !Array.isArray(c.events) || c.events.length > 100000) return fail("Invalid replay contribution", 400);
      const member = session.members[c.submitted_by];
      if (!member || member.token !== request.participant_token) return fail("Invalid participant token", 403);
      if (JSON.stringify(c).length > MAX_REQUEST_BYTES - 1024) return fail("Contribution is too large", 413);
      session.contributions[`${c.match_session_id}:${c.submitted_by}`] = c;
      return ok({});
    }
    case "snapshot":
      if (!validMemberToken(session, request.participant_token)) return fail("Invalid participant token", 403);
      return ok({ snapshot: makeSnapshot(session) });
    case "leave": {
      const member = Object.values(session.members).find(m => m.token === request.participant_token);
      if (!member || member.owner) return fail("Invalid participant token", 403);
      member.connected = false;
      return ok({});
    }
    case "end":
      if (request.session_id !== session.id || request.owner_token !== session.ownerToken) return fail("Only the session owner can end it", 403);
      session.active = false;
      return ok({});
    default: return fail("Invalid request", 400);
  }
}

function makeSnapshot(session) {
  const participants = Object.entries(session.members).map(([id, member]) => ({ playerId: id, playerName: member.name, connected: member.connected }));
  const byMatch = new Map();
  for (const c of Object.values(session.contributions)) {
    const list = byMatch.get(c.match_session_id) || [];
    list.push(c); byMatch.set(c.match_session_id, list);
  }
  const matches = [...byMatch.entries()].map(([matchSessionId, contributions]) => {
    const players = contributions.map(c => ({
      playerId: c.submitted_by, playerName: c.owner?.name || c.submitted_by,
      openings: c.events.filter(e => e.by === "me" && isChest(e.cls)).length,
    }));
    return { matchSessionId, contributions: contributions.length, players, uniqueOpenings: players.reduce((sum, p) => sum + p.openings, 0) };
  }).sort((a, b) => a.matchSessionId.localeCompare(b.matchSessionId));
  return { sessionId: session.id, ownerId: session.ownerId, ownerName: session.ownerName, active: session.active, participants, matches };
}

function isChest(cls) {
  const text = typeof cls === "string" ? cls.toLowerCase() : "";
  if (["ammo", "foodbox", "fishing"].some(p => text.includes(p))) return false;
  return ["rarechest", "buriedchest", "spriteposters", "booty_chest", "relic", "extraction", "lootcannon", "tiered_chest", "normalchest", "factionchest", "chest"].some(p => text.includes(p));
}
function validMemberToken(session, token) { return typeof token === "string" && Object.values(session.members).some(m => m.token === token); }
function validIdentity(id, name) { return validText(id, 256) && id.trim().length > 0 && validText(name, 256) && name.trim().length > 0; }
function validText(value, max) { return typeof value === "string" && value.length <= max; }
function isUuid(value) { return typeof value === "string" && /^[0-9a-f-]{36}$/i.test(value); }
function randomToken() { return crypto.randomUUID().replaceAll("-", "") + crypto.randomUUID().replaceAll("-", ""); }
async function readJson(request) {
  const length = Number(request.headers.get("content-length") || 0);
  if (length > MAX_REQUEST_BYTES) return null;
  const text = await request.text();
  if (new TextEncoder().encode(text).length > MAX_REQUEST_BYTES) return null;
  try { return JSON.parse(text); } catch { return null; }
}
function ok(body) { return { status: 200, body: { ok: true, ...body } }; }
function fail(error, status) { return { status, body: { ok: false, error } }; }
function json(body, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json; charset=utf-8", "cache-control": "no-store" } });
}
