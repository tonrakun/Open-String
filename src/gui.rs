//! 4.3's GUI: rather than a native toolkit (which would mean either a
//! heavyweight embedded WebView runtime or a second large GUI dependency
//! tree, both at odds with 6.2's single-binary/low-footprint goals), this
//! starts a small local HTTP server and opens the system's default browser
//! to it. The page is a single embedded HTML/CSS/JS bundle (no build step,
//! no extra runtime) that polls a small JSON API for the same
//! `dashboard::DashboardSnapshot` data the TUI renders (4.3's "TUIと機能等
//! 価"), and renders the same confirmation-dialog flow as a modal.

use crate::auth::{AnthropicApiKeyProvider, AuthProvider, validate_api_key_format};
use crate::dashboard::{self, DashboardSnapshot, PendingAction};
use crate::permission::PermissionLevel;
use crate::session::{FileWorkspaceRegistry, WorkspaceRegistry};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct GuiState {
    workspace: Option<PathBuf>,
    pending: Mutex<HashMap<u64, (String, PendingAction)>>,
    next_id: AtomicU64,
    shutdown: AtomicBool,
}

pub fn run(workspace: Option<&Path>) -> Result<(), String> {
    let server = tiny_http::Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
    let url = format!("http://{}/", server.server_addr());
    println!("Open String GUI listening at {url}");
    open_in_browser(&url);

    let state = Arc::new(GuiState {
        workspace: workspace.map(Path::to_path_buf),
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        shutdown: AtomicBool::new(false),
    });

    loop {
        if state.shutdown.load(Ordering::SeqCst) {
            break;
        }
        match server.recv_timeout(Duration::from_millis(500)) {
            Ok(Some(request)) => {
                let state = state.clone();
                std::thread::spawn(move || handle_request(request, &state));
            }
            Ok(None) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

fn open_in_browser(url: &str) {
    let result = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .status()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    };
    if let Err(e) = result {
        eprintln!("warning: could not open a browser automatically ({e}); open {url} manually");
    }
}

fn handle_request(mut request: tiny_http::Request, state: &GuiState) {
    let method = request.method().as_str().to_string();
    let url = request.url().to_string();
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);
    let json_body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);

    let (status, content_type, payload) = route(&method, &url, &json_body, state);
    let header = tiny_http::Header::from_bytes(b"Content-Type", content_type.as_bytes())
        .expect("static content-type values are always valid header bytes");
    let response = tiny_http::Response::from_string(payload)
        .with_status_code(status)
        .with_header(header);
    let _ = request.respond(response);
}

fn route(method: &str, url: &str, body: &Value, state: &GuiState) -> (u16, &'static str, String) {
    match (method, url) {
        ("GET", "/") => (200, "text/html; charset=utf-8", INDEX_HTML.to_string()),
        ("GET", "/api/state") => (200, "application/json", snapshot_json(state).to_string()),
        ("POST", "/api/setup/api-key") => json_response(setup_api_key(state, body)),
        ("POST", "/api/setup/permission") => json_response(setup_permission(state, body)),
        ("POST", "/api/setup/workspace") => json_response(setup_workspace(state, body)),
        ("POST", "/api/settings/permission") => json_response(settings_permission(state, body)),
        ("POST", "/api/settings/extension") => {
            json_response(settings_extension_enabled(state, body))
        }
        ("POST", "/api/settings/extension/remove") => {
            json_response(settings_extension_remove(state, body))
        }
        ("POST", "/api/settings/logout") => json_response(settings_logout(state)),
        ("POST", "/api/confirm") => json_response(confirm(state, body)),
        ("POST", "/api/shutdown") => {
            state.shutdown.store(true, Ordering::SeqCst);
            json_response(json!({"ok": true}))
        }
        _ => (
            404,
            "application/json",
            json!({"error": "not found"}).to_string(),
        ),
    }
}

fn json_response(value: Value) -> (u16, &'static str, String) {
    (200, "application/json", value.to_string())
}

fn snapshot_json(state: &GuiState) -> Value {
    let snapshot: DashboardSnapshot = dashboard::gather(state.workspace.as_deref());
    let pending = state
        .pending
        .lock()
        .expect("gui pending-confirmation lock")
        .iter()
        .next()
        .map(|(id, (summary, _))| json!({"id": id, "summary": summary}));

    json!({
        "permissionLevel": snapshot.permission_level.as_str(),
        "authConfigured": snapshot.auth_configured,
        "sessions": snapshot.sessions.iter().map(|s| json!({
            "id": s.id,
            "label": s.label,
            "active": s.is_active(),
        })).collect::<Vec<_>>(),
        "workspaces": snapshot.workspaces.iter().map(|w| json!({
            "name": w.name,
            "path": w.path.display().to_string(),
            "current": snapshot.current_workspace.as_ref().is_some_and(|c| c.path == w.path),
        })).collect::<Vec<_>>(),
        "extensions": snapshot.extensions.iter().map(|e| json!({
            "name": e.name,
            "command": e.command,
            "args": e.args,
            "enabled": e.enabled,
            "requiredPermissionLevel": e.required_permission_level.map(|l| l.as_str()),
        })).collect::<Vec<_>>(),
        "skills": snapshot.skills.iter().map(|s| json!({
            "name": s.name,
            "description": s.description,
        })).collect::<Vec<_>>(),
        "health": snapshot.health.items.iter().map(|item| json!({
            "name": item.name,
            "severity": severity_str(item.severity),
            "message": item.message,
        })).collect::<Vec<_>>(),
        "tokenUsage": snapshot.token_usage.as_ref().map(|u| json!({
            "used": u.used,
            "window": u.window,
            "percent": u.percent(),
        })),
        "recentAuditLog": snapshot.recent_audit_log,
        "pendingConfirmation": pending,
    })
}

fn severity_str(severity: crate::health::Severity) -> &'static str {
    match severity {
        crate::health::Severity::Fatal => "fatal",
        crate::health::Severity::Warning => "warning",
        crate::health::Severity::Info => "info",
    }
}

fn str_field<'a>(body: &'a Value, field: &str) -> Option<&'a str> {
    body.get(field).and_then(Value::as_str)
}

fn setup_api_key(state: &GuiState, body: &Value) -> Value {
    let Some(api_key) = str_field(body, "apiKey") else {
        return json!({"ok": false, "message": "missing apiKey"});
    };
    if !validate_api_key_format(api_key) {
        return json!({"ok": false, "message": "that doesn't look like a valid Anthropic API key"});
    }
    match AnthropicApiKeyProvider::for_workspace(state.workspace.as_deref()).store(api_key) {
        Ok(()) => json!({"ok": true, "message": "API key stored."}),
        Err(e) => json!({"ok": false, "message": format!("failed to store API key: {e}")}),
    }
}

fn setup_permission(state: &GuiState, body: &Value) -> Value {
    let Some(level) = str_field(body, "level").and_then(PermissionLevel::parse) else {
        return json!({"ok": false, "message": "missing or invalid level"});
    };
    match crate::permission_store_for(state.workspace.as_deref()).and_then(|store| {
        store
            .set(level)
            .map_err(|e| format!("failed to set permission level: {e}"))
    }) {
        Ok(()) => json!({"ok": true, "message": format!("Permission level set to {level}.")}),
        Err(e) => json!({"ok": false, "message": e}),
    }
}

fn setup_workspace(_state: &GuiState, body: &Value) -> Value {
    let Some(path) = str_field(body, "path").filter(|p| !p.trim().is_empty()) else {
        return json!({"ok": true, "message": "skipped"});
    };
    let name = str_field(body, "name")
        .filter(|n| !n.trim().is_empty())
        .map(str::to_string);
    match FileWorkspaceRegistry::new().and_then(|r| r.create(Path::new(path), name)) {
        Ok(workspace) => json!({
            "ok": true,
            "message": format!("Workspace \"{}\" registered.", workspace.name),
        }),
        Err(e) => json!({"ok": false, "message": format!("failed to create workspace: {e}")}),
    }
}

fn stage_or_apply(
    state: &GuiState,
    operation: &str,
    action: PendingAction,
    summary: String,
) -> Value {
    let level = dashboard::gather(state.workspace.as_deref()).permission_level;
    if dashboard::requires_confirmation(level, operation) {
        let id = state.next_id.fetch_add(1, Ordering::SeqCst);
        state
            .pending
            .lock()
            .expect("gui pending-confirmation lock")
            .insert(id, (summary.clone(), action));
        json!({"pending": true, "id": id, "summary": summary})
    } else {
        let message = dashboard::apply_pending_action(state.workspace.as_deref(), action);
        json!({"pending": false, "message": message})
    }
}

fn settings_permission(state: &GuiState, body: &Value) -> Value {
    let Some(level) = str_field(body, "level").and_then(PermissionLevel::parse) else {
        return json!({"pending": false, "message": "missing or invalid level"});
    };
    stage_or_apply(
        state,
        &format!("permission set {level}"),
        PendingAction::SetPermissionLevel(level),
        format!("Change permission level to {level}?"),
    )
}

fn settings_extension_enabled(state: &GuiState, body: &Value) -> Value {
    let Some(name) = str_field(body, "name") else {
        return json!({"pending": false, "message": "missing name"});
    };
    let enabled = body
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    stage_or_apply(
        state,
        &format!("toggle extension {name}"),
        PendingAction::SetExtensionEnabled {
            name: name.to_string(),
            enabled,
        },
        format!(
            "{} extension \"{name}\"?",
            if enabled { "Enable" } else { "Disable" }
        ),
    )
}

fn settings_extension_remove(state: &GuiState, body: &Value) -> Value {
    let Some(name) = str_field(body, "name") else {
        return json!({"pending": false, "message": "missing name"});
    };
    stage_or_apply(
        state,
        &format!("delete extension {name}"),
        PendingAction::RemoveExtension(name.to_string()),
        format!("Remove extension \"{name}\"? This cannot be undone."),
    )
}

fn settings_logout(state: &GuiState) -> Value {
    stage_or_apply(
        state,
        "logout",
        PendingAction::Logout,
        "Log out (remove the stored Anthropic API key)?".to_string(),
    )
}

fn confirm(state: &GuiState, body: &Value) -> Value {
    let Some(id) = body.get("id").and_then(Value::as_u64) else {
        return json!({"ok": false, "message": "missing id"});
    };
    let entry = state
        .pending
        .lock()
        .expect("gui pending-confirmation lock")
        .remove(&id);
    let Some((_, action)) = entry else {
        return json!({"ok": false, "message": "no such pending confirmation"});
    };
    let confirmed = body
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if confirmed {
        let message = dashboard::apply_pending_action(state.workspace.as_deref(), action);
        json!({"ok": true, "message": message})
    } else {
        json!({"ok": true, "message": "Declined."})
    }
}

const INDEX_HTML: &str = include_str!("gui/index.html");
