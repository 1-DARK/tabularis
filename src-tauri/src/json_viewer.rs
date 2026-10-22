use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonViewerSession {
    pub value: serde_json::Value,
    pub original_value: serde_json::Value,
    pub col_name: String,
    pub read_only: bool,
    pub cell_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonViewerSavedPayload {
    pub session_id: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Copy)]
pub struct WindowBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Default)]
pub struct JsonViewerStore {
    pub sessions: Mutex<HashMap<String, JsonViewerSession>>,
    pub cell_index: Mutex<HashMap<String, String>>,
    pub last_bounds: Mutex<Option<WindowBounds>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn insert_and_retrieve_session() {
        let store = JsonViewerStore::default();
        {
            let mut guard = store.sessions.lock().unwrap();
            guard.insert(
                "sess-1".into(),
                JsonViewerSession {
                    value: json!({"key": "value"}),
                    original_value: json!({"key": "value"}),
                    col_name: "metadata".into(),
                    read_only: false,
                    cell_key: None,
                },
            );
        }
        let guard = store.sessions.lock().unwrap();
        let session = guard.get("sess-1").unwrap();
        assert_eq!(session.col_name, "metadata");
        assert_eq!(session.value, json!({"key": "value"}));
        assert!(!session.read_only);
    }

    #[test]
    fn cell_index_tracks_active_session() {
        let store = JsonViewerStore::default();
        {
            let mut sessions = store.sessions.lock().unwrap();
            sessions.insert(
                "sess-2".into(),
                JsonViewerSession {
                    value: json!([1, 2, 3]),
                    original_value: json!([1, 2, 3]),
                    col_name: "tags".into(),
                    read_only: false,
                    cell_key: Some("pk:42:tags".into()),
                },
            );
            let mut index = store.cell_index.lock().unwrap();
            index.insert("pk:42:tags".into(), "sess-2".into());
        }
        let index = store.cell_index.lock().unwrap();
        assert_eq!(index.get("pk:42:tags"), Some(&"sess-2".to_string()));
    }

    #[test]
    fn bounds_round_trip() {
        let store = JsonViewerStore::default();
        {
            let mut bounds = store.last_bounds.lock().unwrap();
            *bounds = Some(WindowBounds {
                x: 100,
                y: 200,
                width: 800,
                height: 600,
            });
        }
        let bounds = store.last_bounds.lock().unwrap();
        let b = bounds.unwrap();
        assert_eq!(b.x, 100);
        assert_eq!(b.y, 200);
        assert_eq!(b.width, 800);
        assert_eq!(b.height, 600);
    }

    #[test]
    fn missing_session_returns_none() {
        let store = JsonViewerStore::default();
        let guard = store.sessions.lock().unwrap();
        assert!(guard.get("no-such-session").is_none());
    }
}
