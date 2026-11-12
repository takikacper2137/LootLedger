// The rules of a party room, with no Cloudflare in them so they can be tested on their own.
//
// A room holds, for each person in it, the recent match summaries from that person's LootLedger. When a person
// syncs they hand in their own summaries and get back the other people's summaries of matches they were in.
// The server never sees replay files, only nicknames, hashed player ids and chest counts.

export const LIMITS = {
  bodyBytes: 512 * 1024,
  members: 16,
  offers: 60,
  keys: 8,
  players: 8,
  ttlMs: 24 * 60 * 60 * 1000,
};

export function validRoom(code) {
  return typeof code === "string" && /^[A-Za-z0-9]{12,40}$/.test(code);
}

const text = (v, max) => typeof v === "string" && v.length > 0 && v.length <= max;

function validOffer(item) {
  return (
    item &&
    typeof item === "object" &&
    text(item.key, 200) &&
    item.meta &&
    typeof item.meta === "object" &&
    item.meta.key === item.key &&
    Array.isArray(item.meta.players) &&
    item.meta.players.length <= LIMITS.players
  );
}

function fail(status, error) {
  return { status, body: { ok: false, error }, changed: false };
}

/**
 * Handle one sync. `state` is `{ members: { [who]: { digest, updated, keys, offer } } }` and is changed in place;
 * `changed` tells the caller whether it must be stored again.
 *
 * request: { v: 1, who, keys: [playerKey…], digest, offer: [{key, meta}…] | null }
 *  - `offer` may be left out (null) when `digest` is what the room already holds for this person.
 * reply:   { ok: true, have: digest the room holds for this person | null, offer: [{key, received, meta}…] }
 */
export function sync(state, request, now) {
  if (!request || typeof request !== "object" || request.v !== 1) {
    return fail(400, "This server needs a newer LootLedger");
  }
  const { who, keys, digest, offer } = request;
  if (
    !text(who, 64) ||
    !Array.isArray(keys) ||
    keys.length < 1 ||
    keys.length > LIMITS.keys ||
    !keys.every((k) => text(k, 64)) ||
    !text(digest, 80)
  ) {
    return fail(400, "Invalid request");
  }
  if (offer != null) {
    if (!Array.isArray(offer) || offer.length > LIMITS.offers || !offer.every(validOffer)) {
      return fail(400, "Invalid match summaries");
    }
  }

  let changed = false;
  for (const [id, member] of Object.entries(state.members)) {
    if (now - member.updated > LIMITS.ttlMs) {
      delete state.members[id];
      changed = true;
    }
  }

  const mine = state.members[who];
  if (!mine && Object.keys(state.members).length >= LIMITS.members) {
    return fail(409, "This room is full");
  }
  if (offer != null) {
    state.members[who] = { digest, updated: now, keys, offer };
    changed = true;
  } else if (mine && mine.digest === digest) {
    mine.updated = now;
    mine.keys = keys;
    changed = true;
  }

  const wanted = new Set(keys);
  const received = new Date(now).toISOString();
  const out = [];
  for (const [id, member] of Object.entries(state.members)) {
    if (id === who) continue;
    for (const item of member.offer) {
      if (item.meta.players.some((p) => p && wanted.has(p.key))) {
        out.push({ key: item.key, received, meta: item.meta });
      }
    }
  }
  return {
    status: 200,
    body: { ok: true, have: state.members[who]?.digest ?? null, offer: out },
    changed,
  };
}
