# LootLedger Team Relay

Cloudflare Worker + Durable Objects relay for team sessions. It stores only
parsed replay contributions; it does not receive raw replay files or local paths.

## Deploy

1. Install Node.js and Wrangler.
2. From this directory run: npx wrangler login
3. Run: npx wrangler deploy
4. Copy the resulting workers.dev HTTPS URL. The desktop app will use this
   endpoint after its client-side relay integration is completed.

No router port-forwarding is needed. Clients make outbound HTTPS requests.
Durable Objects isolate and persist each session.

## API

- POST /v1/sessions with JSON fields ownerId and ownerName creates a session.
  The response contains sessionId, joinCode, and ownerToken.
- POST /v1/sessions/{sessionId}/request accepts the same tagged JSON request
  format used by the current Rust team protocol.

The desktop client is not wired to this service yet. The existing direct TCP
transport remains unchanged until that integration is completed.

## Security

HTTPS is provided by Cloudflare. Session IDs, join codes, and tokens are
cryptographically random. Requests are size-limited and sessions are isolated.
Do not share owner tokens.
