import { LIMITS, sync, validRoom } from "./logic.js";

const json = (body, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store" },
  });

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/") {
      return new Response("LootLedger party room server is running.\n", {
        headers: { "content-type": "text/plain; charset=utf-8" },
      });
    }
    const match = url.pathname.match(/^\/v1\/rooms\/([^/]+)\/sync$/);
    if (request.method !== "POST" || !match) return json({ ok: false, error: "Not found" }, 404);
    if (!validRoom(match[1])) {
      return json({ ok: false, error: "The room code must be 12 to 40 letters or digits" }, 400);
    }
    const body = await request.text();
    if (body.length > LIMITS.bodyBytes) return json({ ok: false, error: "Request too large" }, 413);
    const room = env.ROOMS.get(env.ROOMS.idFromName(match[1]));
    return room.fetch("https://room.internal/sync", { method: "POST", body });
  },
};

// One Durable Object per room code. Rooms that nobody has used for a day delete themselves.
export class Room {
  constructor(state) {
    this.state = state;
  }

  async fetch(request) {
    return this.state.blockConcurrencyWhile(async () => {
      let body;
      try {
        body = JSON.parse(await request.text());
      } catch {
        return json({ ok: false, error: "Invalid request" }, 400);
      }
      const now = Date.now();
      const state = (await this.state.storage.get("state")) ?? { members: {} };
      const result = sync(state, body, now);
      if (result.changed) {
        await this.state.storage.put("state", state);
        await this.state.storage.setAlarm(now + LIMITS.ttlMs);
      }
      return json(result.body, result.status);
    });
  }

  async alarm() {
    await this.state.storage.deleteAll();
  }
}
