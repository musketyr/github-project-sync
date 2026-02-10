# GitHub Project Board Auto-Sync

Rust webhook relay that automatically manages Vlad's GitHub project board.

## What it does

- Listens for GitHub webhooks at `/webhook/github`
- Validates HMAC-SHA256 signatures
- When issues/PRs are opened → adds to project board as "Todo"
- When issues are closed or PRs are merged → moves to "Done"
- Only tracks: `pikarama`, `brick-directory` repos

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `GITHUB_TOKEN` | ✅ | - | GitHub PAT with `project` scope |
| `WEBHOOK_SECRET` | ✅ | - | Shared secret for webhook validation |
| `PROJECT_ID` | ❌ | `PVT_kwHOAAoTtc4BO2oX` | GitHub Project V2 node ID |
| `PORT` | ❌ | `3000` | Listen port |

## Endpoints

- `GET /health` → `{"status": "ok"}`
- `POST /webhook/github` → Webhook receiver

## Setup

```bash
# Build
cargo build --release

# Run
GITHUB_TOKEN=ghp_... WEBHOOK_SECRET=your_secret ./target/release/github-project-sync

# Docker
docker build -t github-project-sync .
docker run -p 3000:3000 -e GITHUB_TOKEN=... -e WEBHOOK_SECRET=... github-project-sync
```

## Webhook Configuration

In each GitHub repo (Settings → Webhooks):
- **Payload URL:** `https://<your-domain>/webhook/github`
- **Content type:** `application/json`
- **Secret:** Same as `WEBHOOK_SECRET`
- **Events:** Issues, Pull requests

## Webhook Secret

```
bc6d1020419a993238c3486104905abdee3c20d0
```
