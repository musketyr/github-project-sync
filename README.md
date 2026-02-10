# GitHub Project Board Auto-Sync

A lightweight Rust webhook relay that automatically manages a GitHub Projects V2 board. When issues or PRs are opened/closed in tracked repositories, this service updates the project board accordingly.

**Live at:** `https://gh-sync.orany.cz`

## What It Does

| GitHub Event | Action |
|---|---|
| Issue opened | → Added to project board as **Todo** |
| Issue closed | → Moved to **Done** |
| PR opened | → Added to project board as **Todo** |
| PR merged | → Moved to **Done** |

Only tracks specific repositories (`pikarama`, `brick-directory`) — all other repos are silently ignored.

## Architecture

```
GitHub Webhook ──► Traefik Proxy ──► This Service ──► GitHub GraphQL API
                   (Coolify)         (Rust/Axum)       (Projects V2)
```

The flow for each webhook:

1. **Signature validation** — HMAC-SHA256 using shared secret
2. **Event parsing** — Deserialize JSON payload with serde
3. **Repository filtering** — Only process whitelisted repos
4. **Node ID lookup** — REST API call to get the item's GraphQL node ID
5. **Project mutation** — GraphQL `addProjectV2ItemById` to add the item
6. **Status update** — GraphQL field query + `updateProjectV2ItemFieldValue`

## Rust Patterns & Library Choices

### Why Axum?

[Axum](https://github.com/tokio-rs/axum) is Tokio's official web framework. Compared to alternatives:

- **vs actix-web** — Axum is lighter, uses Tower middleware ecosystem, and has simpler state management via extractors
- **vs warp** — Axum has more intuitive routing and better error messages

Key patterns used:

```rust
// Shared state via Arc — zero-cost cloning across handler tasks
let state = Arc::new(AppState { ... });

// Axum extractors — the framework deserializes headers, body, state for you
async fn webhook(
    State(state): State<Arc<AppState>>,  // App state
    headers: HeaderMap,                    // HTTP headers
    body: Bytes,                          // Raw body (needed for HMAC)
) -> Result<impl IntoResponse, StatusCode> { ... }
```

### Why raw Bytes instead of Json<T>?

We need the **exact bytes** of the request body to verify the HMAC signature. If we used `Json<WebhookPayload>`, Axum would consume the body during deserialization and we couldn't verify the signature. So we:

1. Extract `Bytes` (raw body)
2. Verify HMAC against raw bytes
3. Then manually `serde_json::from_slice(&body)`

### HMAC Signature Verification

GitHub sends `X-Hub-Signature-256: sha256=<hex>`. We verify it with constant-time comparison to prevent timing attacks:

```rust
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    a.iter().zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
```

This XORs each byte pair and ORs into an accumulator. If any byte differs, the result is non-zero — but the loop **always runs to completion** regardless of where the mismatch is. This prevents attackers from measuring response time to guess the signature byte-by-byte.

### GitHub Projects V2 GraphQL API

GitHub Projects V2 only supports GraphQL (not REST). The workflow requires three API calls:

1. **REST:** Get the `node_id` of the issue/PR from its URL
2. **GraphQL mutation:** Add item to project → returns `item_id`
3. **GraphQL query + mutation:** Fetch the Status field's option IDs, then update

The Status field is a `SingleSelectField` with predefined options (Todo, In Progress, Done). We query the field schema dynamically so it works even if column names change.

### Error Handling Strategy

The service uses `Result<impl IntoResponse, StatusCode>` — Axum maps `Err(StatusCode)` directly to HTTP responses. This means:

- Bad signature → `401 Unauthorized`
- Malformed JSON → `400 Bad Request`
- GitHub API failure → `502 Bad Gateway`
- **Never panics** on bad input — all errors are caught and logged

### Tracing

Uses the `tracing` crate with `env_filter` for structured logging:

```rust
tracing_subscriber::fmt()
    .with_env_filter(
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "github_project_sync=info,tower_http=info".into()),
    )
    .init();
```

Set `RUST_LOG=github_project_sync=debug` for verbose output.

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `GITHUB_TOKEN` | ✅ | — | GitHub PAT with `project` and `repo` scopes |
| `WEBHOOK_SECRET` | ✅ | — | Shared HMAC secret (set in GitHub webhook config) |
| `PROJECT_ID` | ❌ | `PVT_kwHOAAoTtc4BO2oX` | GitHub Project V2 global node ID |
| `PORT` | ❌ | `3000` | HTTP listen port |
| `RUST_LOG` | ❌ | `info` | Log level filter |

## Endpoints

- **`GET /health`** — Returns `{"status": "ok"}` (for load balancers / uptime checks)
- **`POST /webhook/github`** — Receives GitHub webhooks

## Local Development

```bash
# Clone
git clone https://github.com/musketyr/github-project-sync.git
cd github-project-sync

# Build
cargo build --release

# Run
export GITHUB_TOKEN=ghp_your_token
export WEBHOOK_SECRET=your_secret
cargo run

# Test health
curl http://localhost:3000/health

# Test webhook (simulate GitHub)
BODY='{"action":"opened","issue":{"html_url":"https://github.com/musketyr/pikarama/issues/1","number":1,"title":"Test"},"repository":{"name":"pikarama","full_name":"musketyr/pikarama"}}'
SIG=$(echo -n "$BODY" | openssl dgst -sha256 -hmac "$WEBHOOK_SECRET" | awk '{print $2}')
curl -H "x-hub-signature-256: sha256=$SIG" \
     -H "x-github-event: issues" \
     -H "Content-Type: application/json" \
     -d "$BODY" http://localhost:3000/webhook/github
```

## Docker

```bash
# Build (native ARM64 on ARM64 hosts)
docker build -t github-project-sync .

# Run
docker run -p 3000:3000 \
  -e GITHUB_TOKEN=ghp_... \
  -e WEBHOOK_SECRET=your_secret \
  github-project-sync
```

The Dockerfile uses multi-stage builds:
1. **Builder stage** — `rust:1.83-bookworm`, compiles release binary with dependency caching
2. **Runtime stage** — `debian:bookworm-slim` (~80MB), only contains the binary + CA certs

### Dependency Caching Trick

```dockerfile
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs && cargo build --release
COPY src ./src
RUN touch src/main.rs && cargo build --release
```

This builds dependencies first with a dummy `main.rs`. Docker caches this layer. When only your source code changes, only the final `cargo build` re-runs (seconds instead of minutes).

## Deployment (Coolify)

1. Create a new application in Coolify (public Git repository)
2. Set the Git URL to `https://github.com/musketyr/github-project-sync.git`
3. Build pack: **Dockerfile**
4. Add environment variables (GITHUB_TOKEN, WEBHOOK_SECRET)
5. Set domain (e.g., `gh-sync.orany.cz`)
6. Deploy

## GitHub Webhook Setup

For each tracked repo (Settings → Webhooks → Add):

- **Payload URL:** `https://gh-sync.orany.cz/webhook/github`
- **Content type:** `application/json`
- **Secret:** *(same as WEBHOOK_SECRET)*
- **Events:** Select **Issues** and **Pull requests**

## Adding More Repositories

Edit the `ALLOWED_REPOS` constant in `src/main.rs`:

```rust
const ALLOWED_REPOS: &[&str] = &["pikarama", "brick-directory", "your-new-repo"];
```

To make this configurable at runtime, you could change it to read from an environment variable (left as an exercise).

## Project Structure

```
├── Cargo.toml          # Dependencies and build config
├── Dockerfile          # Multi-stage Docker build
├── README.md           # This file
└── src/
    └── main.rs         # Everything in one file (~300 lines)
```

Intentionally kept as a single file — for a service this small, splitting into modules adds complexity without benefit.

## Dependencies

| Crate | Purpose |
|-------|---------|
| `axum` | HTTP framework |
| `tokio` | Async runtime |
| `serde` / `serde_json` | JSON serialization |
| `reqwest` | HTTP client (for GitHub API calls) |
| `hmac` / `sha2` / `hex` | Webhook signature verification |
| `tracing` | Structured logging |
| `tower-http` | HTTP middleware (tracing layer) |

## License

MIT
