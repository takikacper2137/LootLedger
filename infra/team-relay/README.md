# LootLedger Team Relay

Cloudflare Worker + Durable Objects relay for LootLedger team sessions. The
relay stores parsed replay contributions only; raw replay files and local file
paths are never uploaded.

## Deploy

1. Install Node.js.
2. From this directory run `npx wrangler login`.
3. Run `npx wrangler deploy`.
4. Copy the resulting `https://...workers.dev` URL.

In LootLedger, enter that URL in the **Relay HTTPS URL** field when creating
or joining a team session. The host shares the relay URL and the complete
invite code (`session-id:join-code`) with teammates. Each teammate enters the
same relay URL and invite code.

No router port-forwarding is needed. The Windows desktop client makes outbound
HTTPS requests to the relay. The relay is not hosted on the session owner's PC.

## API

- `POST /v1/sessions` with JSON fields `ownerId` and `ownerName` creates a
  session. The response contains `sessionId`, `joinCode`, and `ownerToken`.
- `POST /v1/sessions/{sessionId}/request` accepts the tagged JSON request
  format used by the Rust team-session protocol.

The Windows desktop client uses this API for creating, joining, refreshing,
leaving, ending, and uploading parsed replay contributions.

## Security and limits

HTTPS is provided by Cloudflare. Session IDs, join codes, and tokens are
cryptographically random. Requests are size-limited, sessions are isolated
using Durable Objects, and the relay never receives raw replay files. Do not
share owner tokens. The current MVP supports up to 16 participants per session.
