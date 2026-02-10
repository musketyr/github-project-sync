use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use hmac::{Hmac, Mac};
use reqwest::Client;
use sha2::Sha256;
use std::sync::Arc;
use tracing::{error, info, warn};

const ALLOWED_REPOS: &[&str] = &["pikarama", "brick-directory"];

#[derive(Clone)]
struct AppState {
    webhook_secret: String,
    github_token: String,
    project_id: String,
    http: Client,
}

#[derive(serde::Deserialize, Debug)]
struct WebhookPayload {
    action: String,
    issue: Option<Issue>,
    pull_request: Option<PullRequest>,
    repository: Option<Repository>,
}

#[derive(serde::Deserialize, Debug)]
struct Issue {
    html_url: String,
    number: u64,
    title: String,
}

#[derive(serde::Deserialize, Debug)]
struct PullRequest {
    html_url: String,
    number: u64,
    title: String,
    merged: Option<bool>,
}

#[derive(serde::Deserialize, Debug)]
struct Repository {
    name: String,
    full_name: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "github_project_sync=info,tower_http=info".into()),
        )
        .init();

    let state = Arc::new(AppState {
        webhook_secret: std::env::var("WEBHOOK_SECRET").expect("WEBHOOK_SECRET required"),
        github_token: std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN required"),
        project_id: std::env::var("PROJECT_ID")
            .unwrap_or_else(|_| "PVT_kwHOAAoTtc4BO2oX".to_string()),
        http: Client::new(),
    });

    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "3000".to_string())
        .parse()
        .expect("PORT must be a number");

    let app = Router::new()
        .route("/health", get(health))
        .route("/webhook/github", post(webhook))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    info!("Listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

async fn webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, StatusCode> {
    // Validate signature
    let sig = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let sig = sig.strip_prefix("sha256=").ok_or(StatusCode::UNAUTHORIZED)?;

    let mut mac = Hmac::<Sha256>::new_from_slice(state.webhook_secret.as_bytes())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    mac.update(&body);
    let expected = hex::encode(mac.finalize().into_bytes());

    if !constant_time_eq(sig.as_bytes(), expected.as_bytes()) {
        warn!("Invalid webhook signature");
        return Err(StatusCode::UNAUTHORIZED);
    }

    let event = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    let payload: WebhookPayload =
        serde_json::from_slice(&body).map_err(|e| {
            error!("Failed to parse payload: {e}");
            StatusCode::BAD_REQUEST
        })?;

    // Filter by repo
    let repo_name = payload
        .repository
        .as_ref()
        .map(|r| r.name.as_str())
        .unwrap_or("");

    if !ALLOWED_REPOS.contains(&repo_name) {
        info!("Ignoring event from repo: {repo_name}");
        return Ok(Json(serde_json::json!({"status": "ignored", "reason": "repo not tracked"})));
    }

    info!(event, action = %payload.action, repo = repo_name, "Processing webhook");

    match event {
        "issues" => handle_issue(&state, &payload).await,
        "pull_request" => handle_pr(&state, &payload).await,
        _ => {
            info!("Ignoring event type: {event}");
            Ok(Json(serde_json::json!({"status": "ignored"})))
        }
    }
}

async fn handle_issue(
    state: &AppState,
    payload: &WebhookPayload,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let issue = payload.issue.as_ref().ok_or(StatusCode::BAD_REQUEST)?;

    match payload.action.as_str() {
        "opened" => {
            info!(number = issue.number, title = %issue.title, "Adding issue to project");
            let item_id = add_to_project(state, &issue.html_url).await?;
            update_status(state, &item_id, "Todo").await?;
            Ok(Json(serde_json::json!({"status": "added", "item_id": item_id})))
        }
        "closed" => {
            info!(number = issue.number, title = %issue.title, "Moving issue to Done");
            let item_id = add_to_project(state, &issue.html_url).await?;
            update_status(state, &item_id, "Done").await?;
            Ok(Json(serde_json::json!({"status": "done", "item_id": item_id})))
        }
        _ => Ok(Json(serde_json::json!({"status": "ignored"}))),
    }
}

async fn handle_pr(
    state: &AppState,
    payload: &WebhookPayload,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let pr = payload.pull_request.as_ref().ok_or(StatusCode::BAD_REQUEST)?;

    match payload.action.as_str() {
        "opened" => {
            info!(number = pr.number, title = %pr.title, "Adding PR to project");
            let item_id = add_to_project(state, &pr.html_url).await?;
            update_status(state, &item_id, "Todo").await?;
            Ok(Json(serde_json::json!({"status": "added", "item_id": item_id})))
        }
        "closed" if pr.merged == Some(true) => {
            info!(number = pr.number, title = %pr.title, "Moving merged PR to Done");
            let item_id = add_to_project(state, &pr.html_url).await?;
            update_status(state, &item_id, "Done").await?;
            Ok(Json(serde_json::json!({"status": "done", "item_id": item_id})))
        }
        _ => Ok(Json(serde_json::json!({"status": "ignored"}))),
    }
}

/// Add an item to the project via GraphQL, returns the item ID
async fn add_to_project(state: &AppState, content_url: &str) -> Result<String, StatusCode> {
    // First get the node ID of the issue/PR from the URL
    let query = r#"mutation($projectId: ID!, $contentId: ID!) {
        addProjectV2ItemById(input: {projectId: $projectId, contentId: $contentId}) {
            item { id }
        }
    }"#;

    // We need the node_id. Get it from the REST API first.
    let node_id = get_node_id(state, content_url).await?;

    let body = serde_json::json!({
        "query": query,
        "variables": {
            "projectId": state.project_id,
            "contentId": node_id
        }
    });

    let resp = state
        .http
        .post("https://api.github.com/graphql")
        .header("Authorization", format!("Bearer {}", state.github_token))
        .header("User-Agent", "github-project-sync")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            error!("GraphQL request failed: {e}");
            StatusCode::BAD_GATEWAY
        })?;

    let json: serde_json::Value = resp.json().await.map_err(|e| {
        error!("Failed to parse GraphQL response: {e}");
        StatusCode::BAD_GATEWAY
    })?;

    if let Some(errors) = json.get("errors") {
        error!("GraphQL errors: {errors}");
    }

    let item_id = json["data"]["addProjectV2ItemById"]["item"]["id"]
        .as_str()
        .ok_or_else(|| {
            error!("No item ID in response: {json}");
            StatusCode::BAD_GATEWAY
        })?
        .to_string();

    info!(item_id, "Item added to project");
    Ok(item_id)
}

/// Get the node_id from a GitHub URL like https://github.com/owner/repo/issues/1
async fn get_node_id(state: &AppState, html_url: &str) -> Result<String, StatusCode> {
    // Convert html_url to API URL
    let api_url = html_url
        .replace("https://github.com/", "https://api.github.com/repos/")
        .replace("/pull/", "/pulls/");

    let resp = state
        .http
        .get(&api_url)
        .header("Authorization", format!("Bearer {}", state.github_token))
        .header("User-Agent", "github-project-sync")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| {
            error!("REST API request failed: {e}");
            StatusCode::BAD_GATEWAY
        })?;

    let json: serde_json::Value = resp.json().await.map_err(|e| {
        error!("Failed to parse REST response: {e}");
        StatusCode::BAD_GATEWAY
    })?;

    json["node_id"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            error!("No node_id in response: {json}");
            StatusCode::BAD_GATEWAY
        })
}

/// Update the Status field on a project item
async fn update_status(state: &AppState, item_id: &str, status: &str) -> Result<(), StatusCode> {
    // First, get the Status field ID and option IDs
    let field_query = r#"query($projectId: ID!) {
        node(id: $projectId) {
            ... on ProjectV2 {
                fields(first: 20) {
                    nodes {
                        ... on ProjectV2SingleSelectField {
                            id
                            name
                            options { id name }
                        }
                    }
                }
            }
        }
    }"#;

    let body = serde_json::json!({
        "query": field_query,
        "variables": { "projectId": state.project_id }
    });

    let resp = state
        .http
        .post("https://api.github.com/graphql")
        .header("Authorization", format!("Bearer {}", state.github_token))
        .header("User-Agent", "github-project-sync")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to fetch project fields: {e}");
            StatusCode::BAD_GATEWAY
        })?;

    let json: serde_json::Value = resp.json().await.map_err(|e| {
        error!("Failed to parse fields response: {e}");
        StatusCode::BAD_GATEWAY
    })?;

    let fields = json["data"]["node"]["fields"]["nodes"]
        .as_array()
        .ok_or_else(|| {
            error!("No fields found: {json}");
            StatusCode::BAD_GATEWAY
        })?;

    // Find the Status field
    let mut field_id = None;
    let mut option_id = None;

    for field in fields {
        if field["name"].as_str() == Some("Status") {
            field_id = field["id"].as_str().map(|s| s.to_string());
            if let Some(options) = field["options"].as_array() {
                for opt in options {
                    if opt["name"].as_str() == Some(status) {
                        option_id = opt["id"].as_str().map(|s| s.to_string());
                        break;
                    }
                }
            }
            break;
        }
    }

    let field_id = field_id.ok_or_else(|| {
        error!("Status field not found");
        StatusCode::BAD_GATEWAY
    })?;

    let option_id = option_id.ok_or_else(|| {
        error!("Status option '{status}' not found");
        StatusCode::BAD_GATEWAY
    })?;

    // Update the field
    let mutation = r#"mutation($projectId: ID!, $itemId: ID!, $fieldId: ID!, $optionId: String!) {
        updateProjectV2ItemFieldValue(input: {
            projectId: $projectId
            itemId: $itemId
            fieldId: $fieldId
            value: { singleSelectOptionId: $optionId }
        }) {
            projectV2Item { id }
        }
    }"#;

    let body = serde_json::json!({
        "query": mutation,
        "variables": {
            "projectId": state.project_id,
            "itemId": item_id,
            "fieldId": field_id,
            "optionId": option_id
        }
    });

    let resp = state
        .http
        .post("https://api.github.com/graphql")
        .header("Authorization", format!("Bearer {}", state.github_token))
        .header("User-Agent", "github-project-sync")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to update status: {e}");
            StatusCode::BAD_GATEWAY
        })?;

    let json: serde_json::Value = resp.json().await.map_err(|e| {
        error!("Failed to parse update response: {e}");
        StatusCode::BAD_GATEWAY
    })?;

    if let Some(errors) = json.get("errors") {
        error!("Status update errors: {errors}");
        return Err(StatusCode::BAD_GATEWAY);
    }

    info!(item_id, status, "Status updated");
    Ok(())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}
