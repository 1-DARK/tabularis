use crate::results_window::*;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use urlencoding::encode;

/// Open (or focus) the detached results window for a given tab. Result data is
/// streamed from the main window via `resultsSyncEvent(tabId)` in
/// `src/utils/resultsWindowSync.ts`, so no payload is passed here.
#[tauri::command]
pub async fn open_results_window(
    app: AppHandle,
    store: tauri::State<'_, std::sync::Arc<ResultsWindowStore>>,
    tab_id: String,
    title: Option<String>,
) -> Result<(), String> {
    let label = window_label(&tab_id);

    // If it already exists, just bring it to the front.
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }

    let window_title = title.unwrap_or_else(|| "Query Results".to_string());

    let remembered = store
        .bounds
        .lock()
        .map_err(|e| format!("Failed to acquire bounds lock: {}", e))?
        .get(&tab_id)
        .copied();

    let url = format!("/results-window?tab={}", encode(&tab_id));
    let mut builder = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .title(&window_title)
        .min_inner_size(500.0, 300.0)
        .decorations(crate::transport::desktop::window_decorations::native_decorations_enabled())
        .background_color(tauri::webview::Color(2, 6, 23, 255));

    builder = match remembered {
        Some(b) => builder
            .inner_size(b.width as f64, b.height as f64)
            .position(b.x as f64, b.y as f64),
        None => builder.inner_size(900.0, 600.0).center(),
    };

    match builder.build() {
        Err(e) => Err(format!("Failed to create results window: {}", e)),
        Ok(window) => {
            let app_handle = app.clone();
            let captured_label = label.clone();
            let captured_tab_id = tab_id.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { .. } = event {
                    if let Some(win) = app_handle.get_webview_window(&captured_label) {
                        // Save the inner size: it is restored via `.inner_size(...)`
                        // below, so saving `outer_size()` would grow the window by
                        // the decoration height on each detach→close→reopen cycle.
                        if let (Ok(pos), Ok(size)) = (win.outer_position(), win.inner_size()) {
                            let store = app_handle.state::<std::sync::Arc<ResultsWindowStore>>();
                            if let Ok(mut bounds) = store.bounds.lock() {
                                bounds.insert(
                                    captured_tab_id.clone(),
                                    WindowBounds {
                                        x: pos.x,
                                        y: pos.y,
                                        width: size.width,
                                        height: size.height,
                                    },
                                );
                            };
                        }
                    }
                    // Let the main window re-attach this tab's results panel.
                    let _ = app_handle.emit(
                        "results-window:closed",
                        serde_json::json!({ "tabId": captured_tab_id }),
                    );
                }
            });
            Ok(())
        }
    }
}

/// Programmatically close a tab's detached results window (used by the
/// "Re-attach" button and when the bound tab is closed).
#[tauri::command]
pub async fn close_results_window(app: AppHandle, tab_id: String) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(&window_label(&tab_id)) {
        window
            .close()
            .map_err(|e| format!("Failed to close results window: {}", e))?;
    }
    Ok(())
}
