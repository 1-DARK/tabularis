use crate::json_viewer::*;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use ulid::Ulid;
use urlencoding::encode;

#[tauri::command]
pub async fn open_json_viewer_window(
    app: AppHandle,
    store: State<'_, std::sync::Arc<JsonViewerStore>>,
    value: serde_json::Value,
    original_value: Option<serde_json::Value>,
    col_name: String,
    row_label: Option<String>,
    read_only: bool,
    cell_key: Option<String>,
) -> Result<String, String> {
    let original_value = original_value.unwrap_or_else(|| value.clone());
    if let Some(key) = cell_key.as_deref() {
        let existing_id = {
            let cell_index = store
                .cell_index
                .lock()
                .map_err(|e| format!("Failed to acquire cell index lock: {}", e))?;
            cell_index.get(key).cloned()
        };
        if let Some(existing_id) = existing_id {
            let existing_label = format!("json-viewer-{}", existing_id);
            if let Some(window) = app.get_webview_window(&existing_label) {
                let _ = window.unminimize();
                let _ = window.set_focus();
                let mut sessions = store
                    .sessions
                    .lock()
                    .map_err(|e| format!("Failed to acquire session store lock: {}", e))?;
                if let Some(session) = sessions.get_mut(&existing_id) {
                    session.value = value;
                }
                return Ok(existing_id);
            }
            let mut cell_index = store
                .cell_index
                .lock()
                .map_err(|e| format!("Failed to acquire cell index lock: {}", e))?;
            cell_index.remove(key);
            let mut sessions = store
                .sessions
                .lock()
                .map_err(|e| format!("Failed to acquire session store lock: {}", e))?;
            sessions.remove(&existing_id);
        }
    }

    let session_id = Ulid::new().to_string();
    let window_label = format!("json-viewer-{}", session_id);

    {
        let mut guard = store
            .sessions
            .lock()
            .map_err(|e| format!("Failed to acquire session store lock: {}", e))?;
        guard.insert(
            session_id.clone(),
            JsonViewerSession {
                value,
                original_value,
                col_name: col_name.clone(),
                read_only,
                cell_key: cell_key.clone(),
            },
        );
    }
    if let Some(key) = cell_key.as_deref() {
        let mut cell_index = store
            .cell_index
            .lock()
            .map_err(|e| format!("Failed to acquire cell index lock: {}", e))?;
        cell_index.insert(key.to_string(), session_id.clone());
    }

    let url = format!("/json-viewer?session={}", encode(&session_id));
    let title = match row_label.as_deref().filter(|s| !s.is_empty()) {
        Some(label) => format!("{} \u{00b7} {} \u{2014} JSON Viewer", col_name, label),
        None => format!("{} \u{2014} JSON Viewer", col_name),
    };

    let remembered = store
        .last_bounds
        .lock()
        .map_err(|e| format!("Failed to acquire bounds lock: {}", e))?
        .clone();

    let mut builder = WebviewWindowBuilder::new(&app, &window_label, WebviewUrl::App(url.into()))
        .title(&title)
        .min_inner_size(600.0, 400.0)
        .decorations(crate::transport::desktop::window_decorations::native_decorations_enabled())
        .background_color(tauri::webview::Color(2, 6, 23, 255));

    builder = match remembered {
        Some(b) => builder
            .inner_size(b.width as f64, b.height as f64)
            .position(b.x as f64, b.y as f64),
        None => builder.inner_size(900.0, 700.0).center(),
    };

    let build_result = builder.build();

    match build_result {
        Err(e) => {
            let mut sessions = store
                .sessions
                .lock()
                .map_err(|e| format!("Failed to acquire session store lock: {}", e))?;
            sessions.remove(&session_id);
            if let Some(key) = cell_key.as_deref() {
                let mut cell_index = store
                    .cell_index
                    .lock()
                    .map_err(|e| format!("Failed to acquire cell index lock: {}", e))?;
                cell_index.remove(key);
            }
            Err(format!("Failed to create JSON viewer window: {}", e))
        }
        Ok(window) => {
            let app_handle = app.clone();
            let captured_label = window_label.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { .. } = event {
                    if let Some(win) = app_handle.get_webview_window(&captured_label) {
                        if let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) {
                            let store = app_handle.state::<std::sync::Arc<JsonViewerStore>>();
                            let mut bounds = match store.last_bounds.lock() {
                                Ok(g) => g,
                                Err(_) => return,
                            };
                            *bounds = Some(WindowBounds {
                                x: pos.x,
                                y: pos.y,
                                width: size.width,
                                height: size.height,
                            });
                        }
                    }
                }
            });
            Ok(session_id)
        }
    }
}

#[tauri::command]
pub async fn get_json_viewer_session(
    store: State<'_, std::sync::Arc<JsonViewerStore>>,
    session_id: String,
) -> Result<JsonViewerSession, String> {
    let guard = store
        .sessions
        .lock()
        .map_err(|e| format!("Failed to acquire session store lock: {}", e))?;
    guard
        .get(&session_id)
        .cloned()
        .ok_or_else(|| format!("JSON viewer session '{}' not found", session_id))
}

#[tauri::command]
pub async fn complete_json_viewer_session(
    app: AppHandle,
    store: State<'_, std::sync::Arc<JsonViewerStore>>,
    session_id: String,
    value: serde_json::Value,
) -> Result<(), String> {
    let cell_key = {
        let mut guard = store
            .sessions
            .lock()
            .map_err(|e| format!("Failed to acquire session store lock: {}", e))?;
        guard.remove(&session_id).and_then(|s| s.cell_key)
    };
    if let Some(key) = cell_key {
        let mut cell_index = store
            .cell_index
            .lock()
            .map_err(|e| format!("Failed to acquire cell index lock: {}", e))?;
        if cell_index
            .get(&key)
            .map(|v| v == &session_id)
            .unwrap_or(false)
        {
            cell_index.remove(&key);
        }
    }

    app.emit(
        "json-viewer:saved",
        JsonViewerSavedPayload { session_id, value },
    )
    .map_err(|e| format!("Failed to emit json-viewer:saved: {}", e))?;

    Ok(())
}
