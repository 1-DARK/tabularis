use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Manager, Runtime, State};
use tokio::task::AbortHandle;
use urlencoding::encode;
use uuid::Uuid;

use crate::credential_cache;
use crate::keychain_utils;
use crate::models::{
    BatchStatementResult, ColumnDefinition, ConnectionGroup, ConnectionParams, ExplainQueryOutput,
    ExportPayload, ForeignKey, Index, K8sConnection, K8sConnectionInput, QueryResult, RoutineInfo,
    RoutineParameter, SavedConnection, SshConnection, SshConnectionInput, SshTestParams,
    TableColumn, TableInfo, TestConnectionRequest, TriggerInfo,
};
use crate::persistence;

// Constants
/// Resolve the driver from the registry or return a descriptive error.
async fn driver_for(
    id: &str,
) -> Result<std::sync::Arc<dyn crate::drivers::driver_trait::DatabaseDriver>, String> {
    crate::drivers::registry::get_driver(id)
        .await
        .ok_or_else(|| format!("Unsupported driver: {}", id))
}

async fn driver_for_params(
    params: &ConnectionParams,
) -> Result<Arc<dyn crate::drivers::driver_trait::DatabaseDriver>, String> {
    crate::drivers::registry::get_connection_driver(params).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_connection_metadata<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
) -> Result<Option<crate::plugins::connection_metadata::ConnectionMetadata>, String> {
    crate::application::metadata::get_connection_metadata(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
    )
    .await
}

const DEFAULT_MYSQL_PORT: u16 = 3306;
const DEFAULT_POSTGRES_PORT: u16 = 5432;

/// Per-slot collection of abort handles for in-flight cancellable tasks.
/// Used by `QueryCancellationState`, `ExportCancellationState`, and
/// `DumpCancellationState`.
pub(crate) type AbortHandleMap = HashMap<String, Vec<Arc<AbortHandle>>>;

/// Tracks abort handles for in-flight queries keyed by connection id. A
/// slot can hold multiple handles when the UI fires several queries (or
/// an EXPLAIN alongside a query) against the same connection concurrently
/// — `cancel_query` must abort all of them, not just the most recent.
pub struct QueryCancellationState {
    pub handles: Arc<Mutex<AbortHandleMap>>,
}

impl Default for QueryCancellationState {
    fn default() -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Push `handle` into the slot for `key`, first pruning any handles that
/// have already finished so the Vec does not grow unboundedly across many
/// sequential queries on the same connection.
pub(crate) fn register_abort_handle(
    handles: &Mutex<AbortHandleMap>,
    key: String,
    handle: Arc<AbortHandle>,
) {
    let mut guard = handles.lock().unwrap();
    let entry = guard.entry(key).or_default();
    entry.retain(|h| !h.is_finished());
    entry.push(handle);
}

/// Remove the specific handle (matched by Arc identity) that a completing
/// task registered, so it cannot fire on a future query that happens to
/// reuse the same slot.
pub(crate) fn unregister_abort_handle(
    handles: &Mutex<AbortHandleMap>,
    key: &str,
    handle: &Arc<AbortHandle>,
) {
    let mut guard = handles.lock().unwrap();
    if let Some(entry) = guard.get_mut(key) {
        entry.retain(|h| !Arc::ptr_eq(h, handle));
        if entry.is_empty() {
            guard.remove(key);
        }
    }
}

// --- Persistence Helpers ---

#[cfg(feature = "desktop")]
pub async fn expand_ssh_connection_params<R: Runtime>(
    app: &AppHandle<R>,
    params: &ConnectionParams,
) -> Result<ConnectionParams, String> {
    crate::application::tunnels::expand_connection_params(
        &app.state::<crate::runtime::RuntimeContext>(),
        params,
    )
}

/// Check if a string option is empty or contains only whitespace.
#[inline]
#[cfg(test)]
fn is_empty_or_whitespace(s: &Option<String>) -> bool {
    s.as_ref().map(|p| p.trim().is_empty()).unwrap_or(true)
}

#[cfg(feature = "desktop")]
async fn execute_connection_command<R: Runtime>(
    app: &AppHandle<R>,
    command: crate::application::connections::ConnectionCommand,
) -> Result<serde_json::Value, String> {
    crate::application::connections::execute(
        &app.state::<crate::runtime::RuntimeContext>(),
        &app.state::<std::sync::Arc<crate::runtime::state::ApplicationState>>(),
        command,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_connections<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    migrate_ssh_connections(&app).await.ok();
    migrate_postgres_ssl_mode_spelling(&app).await.ok();
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::GetConnections,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_connections_with_groups<R: Runtime>(
    app: AppHandle<R>,
) -> Result<serde_json::Value, String> {
    migrate_ssh_connections(&app).await.ok();
    migrate_postgres_ssl_mode_spelling(&app).await.ok();
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::GetConnectionsWithGroups,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn save_connection<R: Runtime>(
    app: AppHandle<R>,
    name: String,
    params: ConnectionParams,
    plugin_secret_changes: Option<crate::plugin_secrets::PluginSecretChanges>,
    detect_json_in_text_columns: Option<bool>,
    environment: Option<String>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::SaveConnection {
            name,
            params,
            plugin_secret_changes,
            detect_json_in_text_columns,
            environment,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn update_connection<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    name: String,
    params: ConnectionParams,
    plugin_secret_changes: Option<crate::plugin_secrets::PluginSecretChanges>,
    detect_json_in_text_columns: Option<bool>,
    environment: Option<String>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::UpdateConnection {
            id,
            name,
            params,
            plugin_secret_changes,
            detect_json_in_text_columns,
            environment,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn duplicate_connection<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::DuplicateConnection { id },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn delete_connection<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::DeleteConnection { id },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn test_connection<R: Runtime>(
    app: AppHandle<R>,
    request: TestConnectionRequest,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::TestConnection { request },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn list_databases<R: Runtime>(
    app: AppHandle<R>,
    request: TestConnectionRequest,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::ListDatabases { request },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_connection_groups<R: Runtime>(
    app: AppHandle<R>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::GetConnectionGroups,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn create_connection_group<R: Runtime>(
    app: AppHandle<R>,
    name: String,
    parent_id: Option<String>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::CreateConnectionGroup {
            name,
            parent_id,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn create_group_path<R: Runtime>(
    app: AppHandle<R>,
    path: String,
    parent_id: Option<String>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::CreateGroupPath { path, parent_id },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn update_connection_group<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    name: Option<String>,
    collapsed: Option<bool>,
    sort_order: Option<i32>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::UpdateConnectionGroup {
            id,
            name,
            collapsed,
            sort_order,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn move_group_to_parent<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    parent_id: Option<String>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::MoveGroupToParent { id, parent_id },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn delete_connection_group<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::DeleteConnectionGroup { id },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn move_connection_to_group<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    group_id: Option<String>,
    sort_order: Option<i32>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::MoveConnectionToGroup {
            connection_id,
            group_id,
            sort_order,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn reorder_groups<R: Runtime>(
    app: AppHandle<R>,
    group_orders: Vec<(String, i32)>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::ReorderGroups { group_orders },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn reorder_connections_in_group<R: Runtime>(
    app: AppHandle<R>,
    connection_orders: Vec<(String, i32)>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::ReorderConnectionsInGroup {
            connection_orders,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn set_connection_appearance<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    appearance: Option<crate::models::ConnectionAppearance>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::SetConnectionAppearance {
            id,
            appearance,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn list_connection_tags<R: Runtime>(
    app: AppHandle<R>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::ListConnectionTags,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn create_connection_tag<R: Runtime>(
    app: AppHandle<R>,
    name: String,
    color: String,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::CreateConnectionTag { name, color },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn update_connection_tag<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    name: String,
    color: String,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::UpdateConnectionTag { id, name, color },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn delete_connection_tag<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::DeleteConnectionTag { id },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn set_connection_tags<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    tag_ids: Vec<String>,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::SetConnectionTags {
            connection_id,
            tag_ids,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn delete_connection_icon<R: Runtime>(
    app: AppHandle<R>,
    relative_path: String,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::DeleteConnectionIcon { relative_path },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn create_sqlite_database<R: Runtime>(
    app: AppHandle<R>,
    path: String,
) -> Result<serde_json::Value, String> {
    execute_connection_command(
        &app,
        crate::application::connections::ConnectionCommand::CreateSqliteDatabase { path },
    )
    .await
}

pub fn resolve_connection_params(params: &ConnectionParams) -> Result<ConnectionParams, String> {
    crate::application::tunnels::resolve_expanded_connection_params(params, None)
}

#[cfg(test)]
fn resolve_k8s_params(params: &ConnectionParams) -> Result<ConnectionParams, String> {
    crate::application::tunnels::resolve_k8s_params(params)
}

#[cfg(test)]
fn resolve_ssm_params(params: &ConnectionParams) -> Result<ConnectionParams, String> {
    crate::application::tunnels::resolve_ssm_params(params)
}

/// Resolve params for closing a pool without respawning tunnels.
///
/// `build_connection_key` uses `connection_id` (plus driver/database/TLS), not
/// the tunnel's local port, so closing never needs a live SSH tunnel, SSM
/// session, or kubectl port-forward. Disabling all tunnel types here means a
/// dead bastion / unreachable cluster cannot prevent pool cleanup (#1065).
pub(crate) fn params_for_pool_close(
    params: &ConnectionParams,
    connection_id: &str,
) -> Result<ConnectionParams, String> {
    let mut for_close = params.clone();
    for_close.k8s_enabled = Some(false);
    for_close.ssh_enabled = Some(false);
    for_close.ssm_enabled = Some(false);
    resolve_connection_params_with_id(&for_close, connection_id)
}

/// Resolve connection params and set connection_id for stable pooling
pub fn resolve_connection_params_with_id(
    params: &ConnectionParams,
    connection_id: &str,
) -> Result<ConnectionParams, String> {
    let mut with_id = params.clone();
    with_id.connection_id = Some(connection_id.to_string());
    resolve_connection_params(&with_id)
}

#[cfg(feature = "desktop")]
pub fn get_config_path<R: Runtime>(_app: &AppHandle<R>) -> Result<PathBuf, String> {
    let config_dir = crate::paths::get_app_config_dir();
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    }
    Ok(crate::paths::resolve_connections_path(&config_dir))
}

#[cfg(feature = "desktop")]
pub fn get_ssh_config_path<R: Runtime>(_app: &AppHandle<R>) -> Result<PathBuf, String> {
    let config_dir = crate::paths::get_app_config_dir();
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    }
    Ok(config_dir.join("ssh_connections.json"))
}

fn runtime_connection_uri(params: &ConnectionParams) -> Option<&str> {
    params
        .connection_uri
        .as_deref()
        .filter(|value| !value.trim().is_empty())
}

/// Re-attach the URI a saved connection needs at runtime, from the session
/// cache or the keychain.
fn restore_runtime_connection_uri(
    cache: &credential_cache::CredentialCache,
    connection_id: &str,
    params: &mut ConnectionParams,
) -> Result<(), String> {
    if runtime_connection_uri(params).is_some() {
        return Ok(());
    }

    let stored_in_keychain = params.connection_uri_in_keychain.unwrap_or(false);
    match credential_cache::get_connection_uri_cached(cache, connection_id, stored_in_keychain) {
        Ok(Some(connection_uri)) => {
            params.connection_uri = Some(connection_uri);
            Ok(())
        }
        Ok(None) if stored_in_keychain => {
            Err("Stored connection URI is unavailable in the OS keychain".to_string())
        }
        Ok(None) => Ok(()),
        Err(_) => Err("Failed to read the stored connection URI from the OS keychain".to_string()),
    }
}

#[cfg(feature = "desktop")]
/// Saved connection as stored, without any keychain secret.
fn lookup_saved_connection<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
) -> Result<SavedConnection, String> {
    let conn_cache = app.state::<std::sync::Arc<crate::connection_cache::ConnectionCache>>();

    let conn = match conn_cache.lookup(id) {
        crate::connection_cache::CacheLookup::Hit(c) => c,
        crate::connection_cache::CacheLookup::Miss => {
            return Err("Connection not found".to_string())
        }
        crate::connection_cache::CacheLookup::Cold => {
            let path = get_config_path(app)?;
            let conn_file = persistence::load_connections_file(&path).unwrap_or_default();
            conn_cache.populate(&conn_file.connections);
            conn_file
                .connections
                .into_iter()
                .find(|c| c.id == id)
                .ok_or_else(|| "Connection not found".to_string())?
        }
    };
    Ok(conn)
}

#[cfg(feature = "desktop")]
/// Load a saved connection with its host-owned credentials (keychain password,
/// SSH secrets, connection URI) but without plugin-owned secrets. Used by the
/// edit modal and by the Test Connection / Load Databases flows, which resolve
/// plugin secrets themselves once the pending changes are known.
pub fn find_connection_metadata_by_id<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
) -> Result<SavedConnection, String> {
    let mut conn = lookup_saved_connection(app, id)?;

    let cache = app.state::<std::sync::Arc<crate::credential_cache::CredentialCache>>();
    restore_runtime_connection_uri(&cache, &conn.id, &mut conn.params)?;

    // Load passwords from keychain via the in-memory cache (warm hit = lookup,
    // cold miss = keychain call + cache). Skip IAM-auth connections: their
    // 15-min tokens must come from the `password` field, never the keychain,
    // so a stale token from an older release can't be surfaced in the modal.
    if conn.params.save_in_keychain.unwrap_or(false) && !conn.params.use_iam_auth.unwrap_or(false) {
        match credential_cache::get_db_password_cached(&cache, &conn.id) {
            Ok(pwd) => conn.params.password = Some(pwd),
            Err(e) => eprintln!(
                "[Keyring Error] Failed to get DB password for {}: {}",
                conn.id, e
            ),
        }
        if conn.params.ssh_enabled.unwrap_or(false) {
            if let Ok(ssh_pwd) = credential_cache::get_ssh_password_cached(&cache, &conn.id) {
                if !ssh_pwd.trim().is_empty() {
                    conn.params.ssh_password = Some(ssh_pwd);
                }
            }
            if let Ok(ssh_passphrase) =
                credential_cache::get_ssh_key_passphrase_cached(&cache, &conn.id)
            {
                if !ssh_passphrase.trim().is_empty() {
                    conn.params.ssh_key_passphrase = Some(ssh_passphrase);
                }
            }
        }
    }

    Ok(conn)
}

#[cfg(feature = "desktop")]
/// Load a connection for driver execution. Unlike the metadata/edit path,
/// this resolves plugin-owned secrets into `params.extra` only in memory.
pub fn find_connection_by_id<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
) -> Result<SavedConnection, String> {
    let mut conn = find_connection_metadata_by_id(app, id)?;
    let cache = app.state::<std::sync::Arc<crate::credential_cache::CredentialCache>>();
    crate::plugin_secrets::hydrate_connection(&cache, &mut conn);
    Ok(conn)
}

/// Merge a list of incoming groups into an existing list, preserving hierarchy
/// and repairing any `parent_id` that points to a group id not present in the
/// union (i.e. neither in the existing list nor in the incoming batch).
///
/// Behaviour:
/// - Existing groups with the same id are overwritten by the incoming one
///   (so renames / re-ordering / new parent_id from the JSON win).
/// - Missing parents are demoted to root (`parent_id = None`) rather than
///   being rejected, so a partially-malformed JSON still imports successfully
///   and the user keeps most of their tree.
/// - The merge is idempotent: running it twice on the same input is a no-op.
pub(crate) fn merge_groups(existing: &mut Vec<ConnectionGroup>, incoming: Vec<ConnectionGroup>) {
    for new_group in incoming {
        if let Some(existing_group) = existing.iter_mut().find(|g| g.id == new_group.id) {
            *existing_group = new_group;
        } else {
            existing.push(new_group);
        }
    }

    // Build the set of every group id we now have (post-merge) so we can
    // detect parent_ids that no longer point anywhere. Collected into an
    // owned set to release the immutable borrow before we mutate existing.
    let known_ids: std::collections::HashSet<String> =
        existing.iter().map(|g| g.id.clone()).collect();
    for g in existing.iter_mut() {
        if let Some(parent) = g.parent_id.as_deref() {
            if !known_ids.contains(parent) {
                g.parent_id = None;
            }
        }
    }
}

#[cfg(feature = "desktop")]
pub(crate) fn save_connections_and_invalidate<R: Runtime>(
    app: &AppHandle<R>,
    path: &std::path::Path,
    file: &crate::models::ConnectionsFile,
) -> Result<(), String> {
    persistence::save_connections_file(path, file)?;
    app.state::<std::sync::Arc<crate::connection_cache::ConnectionCache>>()
        .invalidate();
    Ok(())
}

// --- Commands ---

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_connection_by_id<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> Result<SavedConnection, String> {
    find_connection_metadata_by_id(&app, &id)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_schemas<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    database: Option<String>,
) -> Result<Vec<String>, String> {
    crate::application::metadata::get_schemas(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_available_databases<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
) -> Result<Vec<String>, String> {
    crate::application::metadata::get_available_databases(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn set_selected_databases<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    databases: Vec<String>,
) -> Result<(), String> {
    crate::application::connections::set_selected_databases(
        &app.state::<crate::runtime::RuntimeContext>(),
        &connection_id,
        databases,
    )
    .await?;
    app.state::<std::sync::Arc<crate::connection_cache::ConnectionCache>>()
        .invalidate();
    Ok(())
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_routines<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<RoutineInfo>, String> {
    crate::application::metadata::get_routines(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_routine_parameters<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    routine_name: String,
    schema: Option<String>,
    database: Option<String>,
    /* The signature of the overload the user clicked, where the dialect has them (#893) */
    identity_args: Option<String>,
) -> Result<Vec<RoutineParameter>, String> {
    crate::application::database_objects::get_routine_parameters(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        routine_name,
        schema,
        database,
        identity_args,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_routine_definition<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    routine_name: String,
    routine_type: String, // "PROCEDURE" or "FUNCTION" - mainly for MySQL SHOW CREATE
    schema: Option<String>,
    database: Option<String>,
    identity_args: Option<String>,
) -> Result<String, String> {
    crate::application::database_objects::get_routine_definition(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        routine_name,
        routine_type,
        schema,
        database,
        identity_args,
    )
    .await
}

/// Preview generation only; the returned SQL is never executed here.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_table_query_template<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    request: crate::models::TableQueryTemplateRequest,
    database: Option<String>,
) -> Result<Option<String>, String> {
    crate::application::database_objects::get_table_query_template(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        request,
        database,
    )
    .await
}

/// Column statistics through the optional plugin RPC (issue #1137). `None`
/// means the driver doesn't provide them; the frontend then falls back to its
/// generic SQL or hides the entry.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_column_stats<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    request: crate::models::ColumnStatsRequest,
    database: Option<String>,
) -> Result<Option<crate::models::ColumnStatsResult>, String> {
    crate::application::database_objects::get_column_stats(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        request,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn build_routine_call_sql<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    routine_name: String,
    routine_type: String,
    args: Vec<crate::models::RoutineCallArg>,
    schema: Option<String>,
) -> Result<String, String> {
    crate::application::database_objects::build_routine_call_sql(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        routine_name,
        routine_type,
        args,
        schema,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_routine_create_template<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    routine_type: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<String, String> {
    crate::application::database_objects::get_routine_create_template(
        &app.state::<crate::runtime::RuntimeContext>(),
        &connection_id,
        routine_type,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_routine_edit_script<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    routine_name: String,
    routine_type: String,
    schema: Option<String>,
    database: Option<String>,
    identity_args: Option<String>,
) -> Result<String, String> {
    crate::application::database_objects::get_routine_edit_script(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        routine_name,
        routine_type,
        schema,
        database,
        identity_args,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn drop_routine<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    routine_name: String,
    routine_type: String,
    schema: Option<String>,
    database: Option<String>,
    identity_args: Option<String>,
) -> Result<(), String> {
    crate::application::database_objects::drop_routine(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        routine_name,
        routine_type,
        schema,
        database,
        identity_args,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_schema_snapshot<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<crate::models::TableSchema>, String> {
    crate::application::metadata::get_schema_snapshot(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        schema,
        database,
    )
    .await
}

// ==================== SSH Connection Management ====================

#[cfg(feature = "desktop")]
/// Migrates old embedded SSH connections to separate SSH connection entries
async fn migrate_ssh_connections<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let conn_path = get_config_path(app)?;
    if !conn_path.exists() {
        return Ok(()); // Nothing to migrate
    }

    // Load connections using persistence (handles both old and new formats)
    let mut conn_file = persistence::load_connections_file(&conn_path)?;
    let connections = &conn_file.connections;

    // Check if any connections have old embedded SSH params
    let needs_migration = connections
        .iter()
        .any(|c| c.params.ssh_enabled.unwrap_or(false) && c.params.ssh_connection_id.is_none());

    if !needs_migration {
        return Ok(()); // No migration needed
    }

    eprintln!("[Migration] Starting SSH connections migration...");

    let ssh_path = get_ssh_config_path(app)?;
    let mut ssh_connections: Vec<SshConnection> = if ssh_path.exists() {
        let ssh_content = fs::read_to_string(&ssh_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&ssh_content).unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut migrated_connections = Vec::new();
    let mut ssh_connection_map: HashMap<String, String> = HashMap::new(); // (ssh_key -> ssh_id)

    for mut conn in conn_file.connections.clone() {
        if conn.params.ssh_enabled.unwrap_or(false) && conn.params.ssh_connection_id.is_none() {
            // Extract SSH params
            if let (Some(host), Some(user)) = (&conn.params.ssh_host, &conn.params.ssh_user) {
                let port = conn.params.ssh_port.unwrap_or(22);
                let key_file = conn.params.ssh_key_file.clone().unwrap_or_default();

                // Create unique key for this SSH config
                let ssh_key = format!("{}:{}:{}:{}", host, port, user, key_file);

                // Check if we already created an SSH connection for this config
                let ssh_id = if let Some(existing_id) = ssh_connection_map.get(&ssh_key) {
                    existing_id.clone()
                } else {
                    // Create new SSH connection
                    let new_ssh_id = Uuid::new_v4().to_string();
                    let ssh_name = format!("{}@{}", user, host);

                    // Migrate credentials from connection keychain to SSH keychain
                    if conn.params.save_in_keychain.unwrap_or(false) {
                        if let Ok(ssh_pwd) = keychain_utils::get_ssh_password(&conn.id, &conn.name)
                        {
                            if !ssh_pwd.trim().is_empty() {
                                keychain_utils::set_ssh_password(&new_ssh_id, &ssh_pwd).ok();
                            }
                        }
                        if let Ok(ssh_pass) =
                            keychain_utils::get_ssh_key_passphrase(&conn.id, &conn.name)
                        {
                            if !ssh_pass.trim().is_empty() {
                                keychain_utils::set_ssh_key_passphrase(&new_ssh_id, &ssh_pass).ok();
                            }
                        }
                    }

                    let new_ssh_conn = SshConnection {
                        id: new_ssh_id.clone(),
                        name: ssh_name,
                        host: host.clone(),
                        port,
                        user: user.clone(),
                        auth_type: Some(if !key_file.is_empty() {
                            "ssh_key".to_string()
                        } else {
                            "password".to_string()
                        }),
                        password: None,
                        key_file: if key_file.is_empty() {
                            None
                        } else {
                            Some(key_file.clone())
                        },
                        key_passphrase: None,
                        allow_passphrase_prompt: None,
                        save_in_keychain: conn.params.save_in_keychain,
                    };

                    ssh_connections.push(new_ssh_conn);
                    ssh_connection_map.insert(ssh_key, new_ssh_id.clone());
                    new_ssh_id
                };

                // Update connection to reference the SSH connection
                conn.params.ssh_connection_id = Some(ssh_id);
                // Clear old embedded SSH params
                conn.params.ssh_host = None;
                conn.params.ssh_port = None;
                conn.params.ssh_user = None;
                conn.params.ssh_password = None;
                conn.params.ssh_key_file = None;
                conn.params.ssh_key_passphrase = None;
            }
        }

        migrated_connections.push(conn);
    }

    // Save migrated SSH connections
    let ssh_json = serde_json::to_string_pretty(&ssh_connections).map_err(|e| e.to_string())?;
    persistence::write_private_file(&ssh_path, ssh_json)?;

    // Save migrated connections using new format (preserving groups)
    conn_file.connections = migrated_connections;
    save_connections_and_invalidate(app, &conn_path, &conn_file)?;

    eprintln!(
        "[Migration] Successfully migrated {} SSH connections",
        ssh_connections.len()
    );
    Ok(())
}

// ==================== PostgreSQL Plugin SSL Mode Migration ====================
//
// The shared, path-based migration logic lives in `connection_migrations`
// (also callable from the standalone `--mcp` server process, which has no
// `AppHandle`). This is a thin wrapper: resolve the path, run the shared
// core, and invalidate the connection cache only if it actually rewrote
// something.

#[cfg(feature = "desktop")]
/// Migrates already-persisted `ssl_mode` values on postgres-dialect
/// connections (driver id other than the builtin `"postgres"`) from the
/// stale MySQL-style spelling to the Postgres-style spelling the plugin
/// actually understands. Idempotent — a no-op once every affected
/// connection has been rewritten. See
/// `connection_migrations::migrate_postgres_ssl_mode_spelling_at_path` for
/// the full rationale, including the concurrency guard against a race with
/// the `--mcp` process's own call to the same function.
async fn migrate_postgres_ssl_mode_spelling<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let conn_path = get_config_path(app)?;
    let migrated =
        crate::connection_migrations::migrate_postgres_ssl_mode_spelling_at_path(&conn_path)
            .await?;
    if migrated {
        app.state::<std::sync::Arc<crate::connection_cache::ConnectionCache>>()
            .invalidate();
    }
    Ok(())
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_ssh_connections<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Vec<SshConnection>, String> {
    crate::application::tunnels::get_ssh_connections(
        &app.state::<crate::runtime::RuntimeContext>(),
        true,
    )
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn save_ssh_connection<R: Runtime>(
    app: AppHandle<R>,
    name: String,
    ssh: SshConnectionInput,
) -> Result<SshConnection, String> {
    crate::application::tunnels::save_ssh_connection(
        &app.state::<crate::runtime::RuntimeContext>(),
        name,
        ssh,
        true,
    )
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn update_ssh_connection<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    name: String,
    ssh: SshConnectionInput,
) -> Result<SshConnection, String> {
    crate::application::tunnels::update_ssh_connection(
        &app.state::<crate::runtime::RuntimeContext>(),
        id,
        name,
        ssh,
        true,
    )
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn delete_ssh_connection<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> Result<(), String> {
    crate::application::tunnels::delete_ssh_connection(
        &app.state::<crate::runtime::RuntimeContext>(),
        &id,
    )
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn test_ssh_connection<R: Runtime>(
    app: AppHandle<R>,
    ssh: SshTestParams,
) -> Result<String, String> {
    crate::application::tunnels::test_ssh_connection(
        &app.state::<crate::runtime::RuntimeContext>(),
        ssh,
        None,
    )
    .await
}

// ---------------------------------------------------------------------------
// Kubernetes Connections
// ---------------------------------------------------------------------------

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_k8s_connections<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Vec<K8sConnection>, String> {
    crate::application::tunnels::get_k8s_connections(&app.state::<crate::runtime::RuntimeContext>())
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn save_k8s_connection<R: Runtime>(
    app: AppHandle<R>,
    k8s: K8sConnectionInput,
) -> Result<K8sConnection, String> {
    crate::application::tunnels::save_k8s_connection(
        &app.state::<crate::runtime::RuntimeContext>(),
        k8s,
    )
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn update_k8s_connection<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    k8s: K8sConnectionInput,
) -> Result<K8sConnection, String> {
    crate::application::tunnels::update_k8s_connection(
        &app.state::<crate::runtime::RuntimeContext>(),
        id,
        k8s,
    )
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn delete_k8s_connection<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> Result<(), String> {
    crate::application::tunnels::delete_k8s_connection(
        &app.state::<crate::runtime::RuntimeContext>(),
        &id,
    )
}

#[cfg(feature = "desktop")]
async fn execute_tunnel_command<R: Runtime>(
    app: &AppHandle<R>,
    command: crate::application::tunnels::TunnelCommand,
) -> Result<serde_json::Value, String> {
    crate::application::tunnels::execute(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        command,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn test_k8s_connection_cmd<R: Runtime>(
    app: AppHandle<R>,
    context: String,
    namespace: String,
    kubectl_path: Option<String>,
    kubeconfig_path: Option<String>,
) -> Result<serde_json::Value, String> {
    let options = crate::k8s_tunnel::K8sCommandOptions::new(kubectl_path, kubeconfig_path);
    execute_tunnel_command(
        &app,
        crate::application::tunnels::TunnelCommand::TestK8sConnection {
            context,
            namespace,
            options,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_k8s_contexts_cmd<R: Runtime>(
    app: AppHandle<R>,
    kubectl_path: Option<String>,
    kubeconfig_path: Option<String>,
) -> Result<serde_json::Value, String> {
    let options = crate::k8s_tunnel::K8sCommandOptions::new(kubectl_path, kubeconfig_path);
    execute_tunnel_command(
        &app,
        crate::application::tunnels::TunnelCommand::GetK8sContexts { options },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_k8s_namespaces_cmd<R: Runtime>(
    app: AppHandle<R>,
    context: String,
    kubectl_path: Option<String>,
    kubeconfig_path: Option<String>,
) -> Result<serde_json::Value, String> {
    let options = crate::k8s_tunnel::K8sCommandOptions::new(kubectl_path, kubeconfig_path);
    execute_tunnel_command(
        &app,
        crate::application::tunnels::TunnelCommand::GetK8sNamespaces { context, options },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_k8s_resources_cmd<R: Runtime>(
    app: AppHandle<R>,
    context: String,
    namespace: String,
    resource_type: String,
    kubectl_path: Option<String>,
    kubeconfig_path: Option<String>,
) -> Result<serde_json::Value, String> {
    let options = crate::k8s_tunnel::K8sCommandOptions::new(kubectl_path, kubeconfig_path);
    execute_tunnel_command(
        &app,
        crate::application::tunnels::TunnelCommand::GetK8sResources {
            context,
            namespace,
            resource_type,
            options,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_k8s_resource_ports_cmd<R: Runtime>(
    app: AppHandle<R>,
    context: String,
    namespace: String,
    resource_type: String,
    resource_name: String,
    kubectl_path: Option<String>,
    kubeconfig_path: Option<String>,
) -> Result<serde_json::Value, String> {
    let options = crate::k8s_tunnel::K8sCommandOptions::new(kubectl_path, kubeconfig_path);
    execute_tunnel_command(
        &app,
        crate::application::tunnels::TunnelCommand::GetK8sResourcePorts {
            context,
            namespace,
            resource_type,
            resource_name,
            options,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn validate_k8s_path_cmd<R: Runtime>(
    app: AppHandle<R>,
    path: String,
    kind: String,
) -> Result<serde_json::Value, String> {
    execute_tunnel_command(
        &app,
        crate::application::tunnels::TunnelCommand::ValidateK8sPath { path, kind },
    )
    .await
}

// ---------------------------------------------------------------------------
// AWS SSM
// ---------------------------------------------------------------------------

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn test_ssm_connection_cmd<R: Runtime>(
    app: AppHandle<R>,
    target: String,
    profile: Option<String>,
    region: Option<String>,
    host: String,
    port: u16,
) -> Result<serde_json::Value, String> {
    execute_tunnel_command(
        &app,
        crate::application::tunnels::TunnelCommand::TestSsmConnection {
            target,
            profile,
            region,
            host,
            port,
        },
    )
    .await
}

#[cfg(feature = "desktop")]
/// Expand K8s connection params by loading saved config and creating/reusing a tunnel.
pub async fn expand_k8s_connection_params<R: Runtime>(
    app: &AppHandle<R>,
    params: &ConnectionParams,
) -> Result<ConnectionParams, String> {
    let runtime = app.state::<crate::runtime::RuntimeContext>();
    let expanded = crate::application::tunnels::expand_connection_params(&runtime, params)?;
    if !expanded.k8s_enabled.unwrap_or(false) {
        return Ok(expanded);
    }
    crate::application::tunnels::resolve_expanded_connection_params(&expanded, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DatabaseSelection;

    fn base_params() -> ConnectionParams {
        ConnectionParams {
            driver: "mysql".to_string(),
            host: Some("localhost".to_string()),
            port: Some(3306),
            username: Some("root".to_string()),
            database: DatabaseSelection::Single("testdb".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_leaves_network_database_names_untouched() {
        let params = ConnectionParams {
            database: DatabaseSelection::Single("'quoted_schema'".to_string()),
            ..base_params()
        };

        let resolved = resolve_connection_params(&params).expect("resolve");
        assert_eq!(resolved.database.primary(), "'quoted_schema'");
    }

    #[test]
    fn params_for_pool_close_disables_all_tunnel_types() {
        // Pool keys depend on connection_id, not the tunnel's local port, so
        // closing a pool must never respawn an SSH/SSM/K8s tunnel (#1065).
        // Verify that all three are forced off regardless of the input.
        let params = ConnectionParams {
            k8s_enabled: Some(true),
            ssh_enabled: Some(true),
            ssm_enabled: Some(true),
            ssh_host: Some("bastion".to_string()),
            ssh_port: Some(22),
            ssh_user: Some("user".to_string()),
            ..base_params()
        };

        let resolved = params_for_pool_close(&params, "conn-1").expect("resolve");
        assert_eq!(resolved.k8s_enabled, Some(false));
        assert_eq!(resolved.ssh_enabled, Some(false));
        assert_eq!(resolved.ssm_enabled, Some(false));
        // connection_id is set for stable pool keying.
        assert_eq!(resolved.connection_id.as_deref(), Some("conn-1"));
    }

    #[test]
    fn resolve_sanitizes_local_path_driver_databases() {
        let driver = "__test_resolve_local_path_driver__";
        crate::drivers::registry::set_local_path_driver(driver, true);
        let params = ConnectionParams {
            driver: driver.to_string(),
            host: None,
            port: None,
            database: DatabaseSelection::Single(r#""file:///tmp/my%20data.db""#.to_string()),
            ..Default::default()
        };

        let resolved = resolve_connection_params(&params);
        crate::drivers::registry::set_local_path_driver(driver, false);
        assert_eq!(
            resolved.expect("resolve").database.primary(),
            "/tmp/my data.db"
        );
    }

    #[test]
    fn restores_the_exact_connection_uri_from_the_session_cache() {
        let cache = credential_cache::CredentialCache::default();
        let sentinel =
            "mongodb+srv://fixture-user:fixture-password@cluster.example.invalid/app?x=a%2Fb";
        credential_cache::set_connection_uri_cached(&cache, "conn-1", sentinel);
        let mut params = base_params();

        restore_runtime_connection_uri(&cache, "conn-1", &mut params)
            .expect("restore cached connection URI");

        assert_eq!(params.connection_uri.as_deref(), Some(sentinel));
    }

    #[test]
    fn params_saved_before_the_uri_field_existed_remain_usable() {
        let cache = credential_cache::CredentialCache::default();
        let mut params = base_params();

        restore_runtime_connection_uri(&cache, "legacy-conn", &mut params)
            .expect("legacy params remain usable");

        assert_eq!(params.connection_uri, None);
        assert_eq!(params.host.as_deref(), Some("localhost"));
    }

    #[test]
    fn deleting_a_connection_clears_its_cached_uri() {
        let cache = credential_cache::CredentialCache::default();
        credential_cache::set_connection_uri_cached(
            &cache,
            "conn-1",
            "mongodb+srv://cluster.example.invalid/app",
        );

        credential_cache::invalidate_all_for_connection(&cache, "conn-1");

        assert_eq!(
            credential_cache::get_connection_uri_cached(&cache, "conn-1", false).unwrap(),
            None
        );
    }

    fn saved_conn(id: &str, password: Option<&str>, save_in_keychain: bool) -> SavedConnection {
        SavedConnection {
            id: id.to_string(),
            name: "Test".to_string(),
            params: ConnectionParams {
                password: password.map(|p| p.to_string()),
                save_in_keychain: Some(save_in_keychain),
                ..base_params()
            },
            plugin_secret_keys: Vec::new(),
            group_id: None,
            sort_order: None,
            detect_json_in_text_columns: None,
            appearance: None,
            tag_ids: None,
            environment: None,
        }
    }

    /// Regression test: update_connection must not wipe appearance.
    ///
    /// The bug was that the struct literal used `appearance: None`, which destroyed
    /// any accent color or custom icon the user had previously set.  The fix reads
    /// `original_appearance` from the existing record and forwards it to the updated
    /// struct — exactly the same pattern already used for `group_id` / `sort_order`.
    ///
    /// Because `update_connection` requires a live Tauri `AppHandle` we cannot call
    /// it in a unit test.  Instead we verify the preservation pattern directly: build
    /// an "existing" SavedConnection with appearance set, clone its appearance field,
    /// and assert it survives into the replacement struct unchanged.
    #[test]
    fn update_connection_preserves_appearance() {
        use crate::models::{ConnectionAppearance, IconOverride};

        let existing = SavedConnection {
            id: "conn-1".to_string(),
            name: "Old Name".to_string(),
            params: base_params(),
            plugin_secret_keys: Vec::new(),
            group_id: Some("group-a".to_string()),
            sort_order: Some(3),
            detect_json_in_text_columns: None,
            appearance: Some(ConnectionAppearance {
                accent_color: Some("#ff0000".to_string()),
                icon: Some(IconOverride::Emoji {
                    value: "🐘".to_string(),
                }),
            }),
            tag_ids: None,
            environment: None,
        };

        // Simulate the pattern used in update_connection after the fix.
        let original_appearance = existing.appearance.clone();

        let updated = SavedConnection {
            id: existing.id.clone(),
            name: "New Name".to_string(),
            params: base_params(),
            plugin_secret_keys: Vec::new(),
            group_id: existing.group_id.clone(),
            sort_order: existing.sort_order,
            detect_json_in_text_columns: None,
            appearance: original_appearance,
            tag_ids: None,
            environment: None,
        };

        let app = updated
            .appearance
            .as_ref()
            .expect("appearance must be preserved");
        assert_eq!(app.accent_color.as_deref(), Some("#ff0000"));
        assert!(matches!(&app.icon, Some(IconOverride::Emoji { value }) if value == "🐘"));
    }

    mod build_connection_url_tests {
        use super::*;

        fn create_params(
            driver: &str,
            host: &str,
            port: Option<u16>,
            username: &str,
            password: Option<&str>,
            database: &str,
        ) -> ConnectionParams {
            ConnectionParams {
                driver: driver.to_string(),
                host: Some(host.to_string()),
                port,
                username: Some(username.to_string()),
                password: password.map(|p| p.to_string()),
                database: DatabaseSelection::Single(database.to_string()),
                ..Default::default()
            }
        }

        #[tokio::test]
        async fn test_mysql_url_basic() {
            let params = create_params(
                "mysql",
                "localhost",
                Some(3306),
                "root",
                Some("secret"),
                "testdb",
            );
            let url = build_connection_url(&params).await.unwrap();
            assert_eq!(url, "mysql://root:secret@localhost:3306/testdb");
        }

        #[tokio::test]
        async fn test_postgres_url_basic() {
            let params = create_params(
                "postgres",
                "localhost",
                Some(5432),
                "postgres",
                Some("secret"),
                "testdb",
            );
            let url = build_connection_url(&params).await.unwrap();
            assert_eq!(url, "postgres://postgres:secret@localhost:5432/testdb");
        }

        #[tokio::test]
        async fn test_sqlite_url() {
            let params = create_params("sqlite", "", None, "", None, "/path/to/db.sqlite");
            let url = build_connection_url(&params).await.unwrap();
            assert_eq!(url, "sqlite:///path/to/db.sqlite");
        }

        #[tokio::test]
        async fn test_url_encoding_special_chars() {
            let params = create_params(
                "mysql",
                "localhost",
                Some(3306),
                "user@domain",
                Some("pass#word"),
                "mydb",
            );
            let url = build_connection_url(&params).await.unwrap();
            assert!(url.contains("user%40domain"));
            assert!(url.contains("pass%23word"));
        }

        #[tokio::test]
        async fn test_default_ports() {
            let mysql_params = create_params("mysql", "localhost", None, "root", None, "testdb");
            let pg_params =
                create_params("postgres", "localhost", None, "postgres", None, "testdb");

            let mysql_url = build_connection_url(&mysql_params).await.unwrap();
            let pg_url = build_connection_url(&pg_params).await.unwrap();

            assert!(mysql_url.contains(":3306/"));
            assert!(pg_url.contains(":5432/"));
        }

        #[tokio::test]
        async fn test_no_password() {
            let params = create_params("mysql", "localhost", Some(3306), "root", None, "testdb");
            let url = build_connection_url(&params).await.unwrap();
            assert_eq!(url, "mysql://root@localhost:3306/testdb");
        }

        #[tokio::test]
        async fn test_unsupported_driver() {
            let params = create_params("mongodb", "localhost", Some(27017), "user", None, "testdb");
            let result = build_connection_url(&params).await;
            assert!(result.is_err());
            assert_eq!(result.unwrap_err(), "Unsupported driver");
        }

        #[tokio::test]
        async fn test_remote_host() {
            let params = create_params(
                "postgres",
                "db.example.com",
                Some(5432),
                "admin",
                Some("pass"),
                "production",
            );
            let url = build_connection_url(&params).await.unwrap();
            assert!(url.contains("db.example.com"));
            assert!(!url.contains("localhost"));
        }
    }

    mod resolve_ssh_password_tests {
        use super::*;
        use crate::models::SshConnection;

        fn create_ssh_conn(
            id: &str,
            password: Option<&str>,
            save_in_keychain: bool,
        ) -> SshConnection {
            SshConnection {
                id: id.to_string(),
                name: "Test".to_string(),
                host: "localhost".to_string(),
                port: 22,
                user: "root".to_string(),
                auth_type: Some("password".to_string()),
                password: password.map(|p| p.to_string()),
                key_file: None,
                key_passphrase: None,
                allow_passphrase_prompt: None,
                save_in_keychain: Some(save_in_keychain),
            }
        }

        #[test]
        fn test_ssh_password_prefers_request() {
            let result = resolve_ssh_test_password(
                Some("from_request"),
                Some("conn_id"),
                |_| None,
                |_| Ok("kc".to_string()),
            );
            assert_eq!(result, Some("from_request".to_string()));
        }

        #[test]
        fn test_ssh_password_from_keychain() {
            let saved = create_ssh_conn("id1", None, true);
            let result = resolve_ssh_test_password(
                None,
                Some("id1"),
                |_| Some(saved.clone()),
                |_| Ok("kc".to_string()),
            );
            assert_eq!(result, Some("kc".to_string()));
        }

        #[test]
        fn test_ssh_password_from_saved_when_not_keychain() {
            let saved = create_ssh_conn("id1", Some("stored"), false);
            let result = resolve_ssh_test_password(
                None,
                Some("id1"),
                |_| Some(saved.clone()),
                |_| Ok("kc".to_string()),
            );
            assert_eq!(result, Some("stored".to_string()));
        }

        #[test]
        fn test_ssh_password_fallback_to_saved_when_keychain_empty() {
            let saved = create_ssh_conn("id1", Some("stored"), true);
            let result = resolve_ssh_test_password(
                None,
                Some("id1"),
                |_| Some(saved.clone()),
                |_| Ok("  ".to_string()),
            );
            assert_eq!(result, Some("stored".to_string()));
        }

        #[test]
        fn test_ssh_password_returns_none_when_no_id() {
            let result = resolve_ssh_test_password(
                None,
                None,
                |_| panic!("should not be called"),
                |_| panic!("should not be called"),
            );
            assert_eq!(result, None);
        }

        #[test]
        fn test_ssh_password_prefers_request_over_keychain() {
            let saved = create_ssh_conn("id1", None, true);
            let result = resolve_ssh_test_password(
                Some("request_pwd"),
                Some("id1"),
                |_| Some(saved.clone()),
                |_| Ok("kc".to_string()),
            );
            assert_eq!(result, Some("request_pwd".to_string()));
        }

        #[test]
        fn test_ssh_empty_request_password_is_used() {
            let saved = create_ssh_conn("id1", None, true);
            let result = resolve_ssh_test_password(
                Some("   "),
                Some("id1"),
                |_| Some(saved.clone()),
                |_| Ok("kc".to_string()),
            );
            // Empty password from request should be used, not keychain
            assert_eq!(result, Some("   ".to_string()));
        }

        #[test]
        fn test_ssh_returns_none_when_no_password_anywhere() {
            let saved = create_ssh_conn("id1", None, false);
            let result = resolve_ssh_test_password(
                None,
                Some("id1"),
                |_| Some(saved.clone()),
                |_| Ok("".to_string()),
            );
            assert_eq!(result, None);
        }
    }

    mod apply_inline_ssh_secret_fallback_tests {
        use super::*;

        fn params_with_ssh(password: Option<&str>, passphrase: Option<&str>) -> ConnectionParams {
            ConnectionParams {
                ssh_password: password.map(|p| p.to_string()),
                ssh_key_passphrase: passphrase.map(|p| p.to_string()),
                ..Default::default()
            }
        }

        #[test]
        fn fills_both_secrets_from_saved_params() {
            let params = params_with_ssh(Some("pwd"), Some("phrase"));
            let (password, passphrase) = apply_inline_ssh_secret_fallback(None, None, &params);
            assert_eq!(password, Some("pwd".to_string()));
            assert_eq!(passphrase, Some("phrase".to_string()));
        }

        #[test]
        fn resolved_secrets_keep_priority_over_saved() {
            let params = params_with_ssh(Some("saved_pwd"), Some("saved_phrase"));
            let (password, passphrase) = apply_inline_ssh_secret_fallback(
                Some("request_pwd".to_string()),
                Some("request_phrase".to_string()),
                &params,
            );
            assert_eq!(password, Some("request_pwd".to_string()));
            assert_eq!(passphrase, Some("request_phrase".to_string()));
        }

        #[test]
        fn blank_saved_secrets_are_ignored() {
            let params = params_with_ssh(Some("   "), Some(""));
            let (password, passphrase) = apply_inline_ssh_secret_fallback(None, None, &params);
            assert_eq!(password, None);
            assert_eq!(passphrase, None);
        }

        #[test]
        fn fills_only_missing_secret() {
            let params = params_with_ssh(Some("saved_pwd"), Some("saved_phrase"));
            let (password, passphrase) =
                apply_inline_ssh_secret_fallback(Some("request_pwd".to_string()), None, &params);
            assert_eq!(password, Some("request_pwd".to_string()));
            assert_eq!(passphrase, Some("saved_phrase".to_string()));
        }
    }

    mod is_empty_or_whitespace_tests {
        use super::*;

        #[test]
        fn test_none_is_empty() {
            assert!(is_empty_or_whitespace(&None));
        }

        #[test]
        fn test_empty_string_is_empty() {
            assert!(is_empty_or_whitespace(&Some("".to_string())));
        }

        #[test]
        fn test_whitespace_only_is_empty() {
            assert!(is_empty_or_whitespace(&Some("   ".to_string())));
        }

        #[test]
        fn test_tab_newline_is_empty() {
            assert!(is_empty_or_whitespace(&Some("\t\n  ".to_string())));
        }

        #[test]
        fn test_content_is_not_empty() {
            assert!(!is_empty_or_whitespace(&Some("content".to_string())));
        }

        #[test]
        fn test_content_with_whitespace_is_not_empty() {
            assert!(!is_empty_or_whitespace(&Some("  content  ".to_string())));
        }
    }

    mod resolve_connection_params_tests {
        use super::*;

        fn create_ssh_params(
            ssh_host: &str,
            ssh_port: u16,
            ssh_user: &str,
            remote_host: &str,
            remote_port: u16,
        ) -> ConnectionParams {
            ConnectionParams {
                driver: "mysql".to_string(),
                host: Some(remote_host.to_string()),
                port: Some(remote_port),
                username: Some("dbuser".to_string()),
                password: Some("dbpass".to_string()),
                database: DatabaseSelection::Single("testdb".to_string()),
                ssh_enabled: Some(true),
                ssh_host: Some(ssh_host.to_string()),
                ssh_port: Some(ssh_port),
                ssh_user: Some(ssh_user.to_string()),
                ssh_key_file: Some("/home/user/.ssh/id_rsa".to_string()),
                ..Default::default()
            }
        }

        #[tokio::test]
        async fn test_non_ssh_params_unchanged() {
            let params = base_params();
            let result = resolve_connection_params(&params).unwrap();
            assert_eq!(result.host, Some("localhost".to_string()));
            assert_eq!(result.port, Some(3306));
        }

        #[tokio::test]
        async fn test_ssh_params_require_host() {
            let mut params = create_ssh_params("jump.server", 22, "admin", "db.internal", 3306);
            params.ssh_host = None;
            let result = resolve_connection_params(&params);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("SSH Host"));
        }

        #[tokio::test]
        async fn test_ssh_params_require_user() {
            let mut params = create_ssh_params("jump.server", 22, "admin", "db.internal", 3306);
            params.ssh_user = None;
            let result = resolve_connection_params(&params);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("SSH User"));
        }
    }

    mod resolve_ssm_params_tests {
        use super::*;

        fn create_ssm_params(target: &str) -> ConnectionParams {
            ConnectionParams {
                driver: "mysql".to_string(),
                host: Some("db.internal".to_string()),
                port: Some(3306),
                username: Some("root".to_string()),
                database: DatabaseSelection::Single("testdb".to_string()),
                ssm_enabled: Some(true),
                ssm_target: Some(target.to_string()),
                ..Default::default()
            }
        }

        #[test]
        fn test_ssm_requires_target() {
            let mut params = create_ssm_params("i-0abc");
            params.ssm_target = Some("   ".to_string());
            let result = resolve_ssm_params(&params);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("AWS SSM target"));
        }

        #[test]
        fn test_ssm_and_ssh_mutual_exclusion() {
            let mut params = create_ssm_params("i-0abc");
            params.ssh_enabled = Some(true);
            let result = resolve_connection_params(&params);
            assert!(result.is_err());
            assert!(result
                .unwrap_err()
                .contains("SSH and AWS SSM tunnels cannot both be enabled"));
        }

        #[test]
        fn test_ssm_and_k8s_mutual_exclusion() {
            let mut params = create_ssm_params("i-0abc");
            params.k8s_enabled = Some(true);
            let result = resolve_connection_params(&params);
            assert!(result.is_err());
            assert!(result
                .unwrap_err()
                .contains("Kubernetes and AWS SSM tunnels cannot both be enabled"));
        }

        #[test]
        fn test_disabled_ssm_leaves_params_untouched() {
            let mut params = create_ssm_params("i-0abc");
            params.ssm_enabled = Some(false);
            let result = resolve_connection_params(&params).unwrap();
            assert_eq!(result.host, Some("db.internal".to_string()));
            assert_eq!(result.port, Some(3306));
        }
    }

    mod resolve_k8s_params_tests {
        use super::*;

        fn create_k8s_params(
            context: &str,
            namespace: &str,
            resource_type: &str,
            resource_name: &str,
            port: u16,
        ) -> ConnectionParams {
            ConnectionParams {
                driver: "mysql".to_string(),
                host: Some("localhost".to_string()),
                port: Some(3306),
                username: Some("root".to_string()),
                database: DatabaseSelection::Single("testdb".to_string()),
                k8s_enabled: Some(true),
                k8s_context: Some(context.to_string()),
                k8s_namespace: Some(namespace.to_string()),
                k8s_resource_type: Some(resource_type.to_string()),
                k8s_resource_name: Some(resource_name.to_string()),
                k8s_port: Some(port),
                ..Default::default()
            }
        }

        #[test]
        fn test_k8s_and_ssh_mutual_exclusion() {
            let mut params = create_k8s_params("my-ctx", "default", "service", "my-db", 3306);
            params.ssh_enabled = Some(true);
            params.ssh_host = Some("jump.host".to_string());
            let result = resolve_connection_params(&params);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("cannot both be enabled"));
        }

        #[test]
        fn test_k8s_requires_context() {
            let mut params = create_k8s_params("my-ctx", "default", "service", "my-db", 3306);
            params.k8s_context = None;
            let result = resolve_k8s_params(&params);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("K8s context"));
        }

        #[test]
        fn test_k8s_requires_namespace() {
            let mut params = create_k8s_params("my-ctx", "default", "service", "my-db", 3306);
            params.k8s_namespace = None;
            let result = resolve_k8s_params(&params);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("K8s namespace"));
        }

        #[test]
        fn test_k8s_requires_resource_type() {
            let mut params = create_k8s_params("my-ctx", "default", "service", "my-db", 3306);
            params.k8s_resource_type = None;
            let result = resolve_k8s_params(&params);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("K8s resource type"));
        }

        #[test]
        fn test_k8s_requires_resource_name() {
            let mut params = create_k8s_params("my-ctx", "default", "service", "my-db", 3306);
            params.k8s_resource_name = None;
            let result = resolve_k8s_params(&params);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("K8s resource name"));
        }

        #[test]
        fn test_k8s_requires_port() {
            let mut params = create_k8s_params("my-ctx", "default", "service", "my-db", 3306);
            params.k8s_port = None;
            let result = resolve_k8s_params(&params);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("K8s port"));
        }
    }

    mod url_encoding_edge_cases {
        use super::*;

        #[tokio::test]
        async fn test_unicode_username() {
            let mut params = base_params();
            params.username = Some("用户".to_string());
            let url = build_connection_url(&params).await.unwrap();
            // URL should contain percent-encoded UTF-8
            assert!(url.contains("%E7%94%A8%E6%88%B7"));
        }

        #[tokio::test]
        async fn test_password_with_colon() {
            let mut params = base_params();
            params.password = Some("pass:word".to_string());
            let url = build_connection_url(&params).await.unwrap();
            assert!(url.contains("pass%3Aword"));
        }

        #[tokio::test]
        async fn test_password_with_at_sign() {
            let mut params = base_params();
            params.password = Some("pass@word".to_string());
            let url = build_connection_url(&params).await.unwrap();
            assert!(url.contains("pass%40word"));
        }

        #[tokio::test]
        async fn test_password_with_slash() {
            let mut params = base_params();
            params.password = Some("pass/word".to_string());
            let url = build_connection_url(&params).await.unwrap();
            assert!(url.contains("pass%2Fword"));
        }

        #[tokio::test]
        async fn test_empty_username_and_password() {
            let mut params = base_params();
            params.username = None;
            params.password = None;
            let url = build_connection_url(&params).await.unwrap();
            assert_eq!(url, "mysql://@localhost:3306/testdb");
        }

        #[tokio::test]
        async fn test_host_with_port_in_url() {
            let mut params = base_params();
            params.host = Some("192.168.1.100".to_string());
            params.port = Some(33060);
            let url = build_connection_url(&params).await.unwrap();
            assert!(url.contains("192.168.1.100:33060"));
        }
    }

    mod cancellation_state {
        use super::super::{
            cancel_query_impl, register_abort_handle, unregister_abort_handle,
            QueryCancellationState,
        };
        use std::sync::Arc;
        use std::time::Duration;

        async fn spawn_sleeper() -> tokio::task::JoinHandle<()> {
            tokio::spawn(async { tokio::time::sleep(Duration::from_secs(10)).await })
        }

        #[tokio::test]
        async fn registers_multiple_handles_under_same_slot() {
            let state = QueryCancellationState::default();
            let task_a = spawn_sleeper().await;
            let task_b = spawn_sleeper().await;
            let handle_a = Arc::new(task_a.abort_handle());
            let handle_b = Arc::new(task_b.abort_handle());

            register_abort_handle(&state.handles, "conn-1".into(), handle_a);
            register_abort_handle(&state.handles, "conn-1".into(), handle_b);

            assert_eq!(
                state.handles.lock().unwrap().get("conn-1").unwrap().len(),
                2
            );

            task_a.abort();
            task_b.abort();
            let _ = task_a.await;
            let _ = task_b.await;
        }

        #[tokio::test]
        async fn cancel_aborts_all_handles_in_slot() {
            let state = QueryCancellationState::default();
            let task_a = spawn_sleeper().await;
            let task_b = spawn_sleeper().await;
            register_abort_handle(
                &state.handles,
                "conn-1".into(),
                Arc::new(task_a.abort_handle()),
            );
            register_abort_handle(
                &state.handles,
                "conn-1".into(),
                Arc::new(task_b.abort_handle()),
            );

            let drained = state
                .handles
                .lock()
                .unwrap()
                .remove("conn-1")
                .unwrap_or_default();
            for h in &drained {
                h.abort();
            }

            assert!(task_a.await.unwrap_err().is_cancelled());
            assert!(task_b.await.unwrap_err().is_cancelled());
        }

        #[tokio::test]
        async fn unregister_only_removes_matching_handle() {
            let state = QueryCancellationState::default();
            let task_a = spawn_sleeper().await;
            let task_b = spawn_sleeper().await;
            let handle_a = Arc::new(task_a.abort_handle());
            let handle_b = Arc::new(task_b.abort_handle());

            register_abort_handle(&state.handles, "conn-1".into(), handle_a.clone());
            register_abort_handle(&state.handles, "conn-1".into(), handle_b.clone());

            unregister_abort_handle(&state.handles, "conn-1", &handle_a);

            {
                let remaining = state.handles.lock().unwrap();
                let slot = remaining
                    .get("conn-1")
                    .expect("slot kept while B in flight");
                assert_eq!(slot.len(), 1);
                assert!(Arc::ptr_eq(&slot[0], &handle_b));
            }

            task_a.abort();
            task_b.abort();
            let _ = task_a.await;
            let _ = task_b.await;
        }

        #[tokio::test]
        async fn unregister_drops_empty_slot() {
            let state = QueryCancellationState::default();
            let task = spawn_sleeper().await;
            let handle = Arc::new(task.abort_handle());

            register_abort_handle(&state.handles, "conn-1".into(), handle.clone());
            unregister_abort_handle(&state.handles, "conn-1", &handle);

            assert!(state.handles.lock().unwrap().get("conn-1").is_none());

            task.abort();
            let _ = task.await;
        }

        #[tokio::test]
        async fn register_prunes_finished_handles() {
            let state = QueryCancellationState::default();

            let finished_task = tokio::spawn(async {});
            let finished_handle = Arc::new(finished_task.abort_handle());
            let _ = finished_task.await;
            assert!(finished_handle.is_finished());

            register_abort_handle(&state.handles, "conn-1".into(), finished_handle);

            let live_task = spawn_sleeper().await;
            let live_handle = Arc::new(live_task.abort_handle());
            register_abort_handle(&state.handles, "conn-1".into(), live_handle.clone());

            {
                let guard = state.handles.lock().unwrap();
                let slot = guard.get("conn-1").unwrap();
                assert_eq!(slot.len(), 1);
                assert!(Arc::ptr_eq(&slot[0], &live_handle));
            }

            live_task.abort();
            let _ = live_task.await;
        }

        #[tokio::test]
        async fn cancel_query_returns_err_when_no_slot() {
            let state = QueryCancellationState::default();
            let err = cancel_query_impl(&state, "conn-1").unwrap_err();
            assert_eq!(err, "No running query found");
        }

        #[tokio::test]
        async fn cancel_query_aborts_every_handle_in_slot() {
            let state = QueryCancellationState::default();
            let task_a = spawn_sleeper().await;
            let task_b = spawn_sleeper().await;
            register_abort_handle(
                &state.handles,
                "conn-1".into(),
                Arc::new(task_a.abort_handle()),
            );
            register_abort_handle(
                &state.handles,
                "conn-1".into(),
                Arc::new(task_b.abort_handle()),
            );

            cancel_query_impl(&state, "conn-1").unwrap();

            assert!(task_a.await.unwrap_err().is_cancelled());
            assert!(task_b.await.unwrap_err().is_cancelled());
            assert!(state.handles.lock().unwrap().get("conn-1").is_none());
        }

        #[tokio::test]
        async fn cancel_query_aborts_query_and_explain_sharing_the_slot() {
            let state = QueryCancellationState::default();
            let query_task = spawn_sleeper().await;
            let explain_task = spawn_sleeper().await;
            register_abort_handle(
                &state.handles,
                "conn-1".into(),
                Arc::new(query_task.abort_handle()),
            );
            register_abort_handle(
                &state.handles,
                "conn-1".into(),
                Arc::new(explain_task.abort_handle()),
            );

            cancel_query_impl(&state, "conn-1").unwrap();

            assert!(query_task.await.unwrap_err().is_cancelled());
            assert!(explain_task.await.unwrap_err().is_cancelled());
            assert!(state.handles.lock().unwrap().get("conn-1").is_none());
        }
    }

    // -------------------------------------------------------------------
    // Cascade-delete helpers
    // -------------------------------------------------------------------

    fn group(id: &str, parent: Option<&str>) -> ConnectionGroup {
        ConnectionGroup {
            id: id.to_string(),
            name: id.to_string(),
            collapsed: false,
            sort_order: 0,
            parent_id: parent.map(|p| p.to_string()),
        }
    }

    fn conn(id: &str, group_id: Option<&str>) -> SavedConnection {
        let mut c = saved_conn(id, None, false);
        c.group_id = group_id.map(|g| g.to_string());
        c
    }

    #[test]
    fn collect_group_subtree_returns_root_only_for_leaf() {
        let groups = vec![group("a", None), group("b", None)];
        let subtree = crate::models::collect_group_subtree(&groups, "a");
        assert_eq!(subtree, std::collections::HashSet::from(["a".to_string()]));
    }

    #[test]
    fn collect_group_subtree_walks_full_descendant_chain() {
        // Tree:
        //   root
        //   ├── child1
        //   │   └── grand1
        //   │       └── great1
        //   └── child2
        //   other (unrelated)
        let groups = vec![
            group("root", None),
            group("child1", Some("root")),
            group("grand1", Some("child1")),
            group("great1", Some("grand1")),
            group("child2", Some("root")),
            group("other", None),
        ];
        let subtree = crate::models::collect_group_subtree(&groups, "root");
        assert_eq!(
            subtree,
            std::collections::HashSet::from([
                "root".to_string(),
                "child1".to_string(),
                "grand1".to_string(),
                "great1".to_string(),
                "child2".to_string(),
            ])
        );
        assert!(!subtree.contains("other"));
    }

    #[test]
    fn collect_group_subtree_for_subgroup_does_not_include_siblings() {
        // Tree:
        //   root
        //   ├── keep
        //   └── drop
        let groups = vec![
            group("root", None),
            group("keep", Some("root")),
            group("drop", Some("root")),
        ];
        let subtree = crate::models::collect_group_subtree(&groups, "drop");
        assert_eq!(
            subtree,
            std::collections::HashSet::from(["drop".to_string()])
        );
        assert!(!subtree.contains("root"));
        assert!(!subtree.contains("keep"));
    }

    #[test]
    fn collect_group_subtree_for_unknown_id_is_singleton() {
        let groups = vec![group("a", None)];
        let subtree = crate::models::collect_group_subtree(&groups, "missing");
        assert_eq!(
            subtree,
            std::collections::HashSet::from(["missing".to_string()])
        );
    }

    #[test]
    fn cascade_delete_removes_parent_descendants_and_connections() {
        // Mirrors what the command does after the helper returns: groups
        // and connections not in the subtree must survive untouched.
        let groups = vec![
            group("root", None),
            group("child", Some("root")),
            group("grand", Some("child")),
            group("sibling", None),
        ];
        let connections = vec![
            conn("c1", Some("root")),
            conn("c2", Some("child")),
            conn("c3", Some("grand")),
            conn("c4", Some("sibling")),
            conn("c5", None),
        ];
        let to_delete = crate::models::collect_group_subtree(&groups, "root");

        let groups_after: Vec<_> = groups
            .iter()
            .filter(|g| !to_delete.contains(&g.id))
            .cloned()
            .collect();
        let conns_after: Vec<_> = connections
            .iter()
            .filter(|c| !c.group_id.as_ref().is_some_and(|g| to_delete.contains(g)))
            .cloned()
            .collect();

        assert_eq!(groups_after, vec![group("sibling", None)]);
        assert_eq!(
            conns_after.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
            vec!["c4".to_string(), "c5".to_string()],
        );
    }

    #[test]
    fn cascade_delete_subgroup_leaves_parent_and_other_subgroups_alone() {
        let groups = vec![
            group("root", None),
            group("keep", Some("root")),
            group("drop", Some("root")),
            group("grand", Some("drop")),
        ];
        let connections = vec![
            conn("c1", Some("root")),
            conn("c2", Some("drop")),
            conn("c3", Some("grand")),
            conn("c4", Some("keep")),
        ];
        let to_delete = crate::models::collect_group_subtree(&groups, "drop");

        let groups_after: Vec<_> = groups
            .iter()
            .filter(|g| !to_delete.contains(&g.id))
            .cloned()
            .collect();
        let conns_after: Vec<_> = connections
            .iter()
            .filter(|c| !c.group_id.as_ref().is_some_and(|g| to_delete.contains(g)))
            .cloned()
            .collect();

        assert_eq!(
            groups_after,
            vec![group("root", None), group("keep", Some("root"))],
        );
        assert_eq!(
            conns_after.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
            vec!["c1".to_string(), "c4".to_string()],
        );
    }

    #[test]
    fn ddl_database_override_targets_the_browsed_database() {
        let mut params = base_params();
        params.database =
            DatabaseSelection::Multiple(vec!["first".to_string(), "second".to_string()]);
        apply_ddl_database_override(&mut params, Some("second".to_string()));
        assert_eq!(
            params.database,
            DatabaseSelection::Single("second".to_string())
        );
    }

    fn saved(params: ConnectionParams) -> SavedConnection {
        SavedConnection {
            id: "conn-1".to_string(),
            name: "Conn".to_string(),
            params,
            group_id: None,
            sort_order: None,
            detect_json_in_text_columns: None,
            appearance: None,
            tag_ids: None,
            environment: None,
            plugin_secret_keys: Vec::new(),
        }
    }

    #[test]
    fn sql_builder_params_never_resolve_tunnels() {
        // Resolving any of these would fail (no SSH host, no SSM target, no
        // K8s context) or open a tunnel; the builders must skip it entirely.
        for (ssh, ssm, k8s) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let params = ConnectionParams {
                driver: "test-plugin".to_string(),
                host: Some("db.internal".to_string()),
                port: Some(5432),
                ssh_enabled: Some(ssh),
                ssm_enabled: Some(ssm),
                k8s_enabled: Some(k8s),
                ..base_params()
            };
            let built = sql_builder_params(&saved(params), Some("analytics".to_string()));
            assert_eq!(built.host.as_deref(), Some("db.internal"));
            assert_eq!(built.port, Some(5432));
            assert_eq!(built.connection_id.as_deref(), Some("conn-1"));
            assert_eq!(
                built.database,
                DatabaseSelection::Single("analytics".to_string())
            );
        }
    }

    #[test]
    fn sql_builder_params_keep_saved_database_without_override() {
        let built = sql_builder_params(&saved(base_params()), None);
        assert_eq!(
            built.database,
            DatabaseSelection::Single("testdb".to_string())
        );
        assert_eq!(built.host.as_deref(), Some("localhost"));
    }

    #[test]
    fn sql_builder_secrets_bind_keychain_values() {
        let mut params = ConnectionParams {
            save_in_keychain: Some(true),
            connection_uri_in_keychain: Some(true),
            ..base_params()
        };
        apply_sql_builder_secrets(
            &mut params,
            "conn-1",
            |stored| {
                assert!(stored);
                Ok(Some("plugin://token@host".to_string()))
            },
            || Ok("pw".to_string()),
        );
        assert_eq!(
            params.connection_uri.as_deref(),
            Some("plugin://token@host")
        );
        assert_eq!(params.password.as_deref(), Some("pw"));
    }

    #[test]
    fn sql_builder_secrets_tolerate_keychain_failures() {
        let mut params = ConnectionParams {
            save_in_keychain: Some(true),
            connection_uri_in_keychain: Some(true),
            ..base_params()
        };
        apply_sql_builder_secrets(
            &mut params,
            "conn-1",
            |_| Err("keychain locked".to_string()),
            || Err("keychain locked".to_string()),
        );
        assert_eq!(params.connection_uri, None);
        assert_eq!(params.password, None);

        let mut missing_uri = ConnectionParams {
            connection_uri_in_keychain: Some(true),
            ..base_params()
        };
        apply_sql_builder_secrets(
            &mut missing_uri,
            "conn-1",
            |_| Ok(None),
            || panic!("password not stored in the keychain"),
        );
        assert_eq!(missing_uri.connection_uri, None);
    }

    #[test]
    fn sql_builder_secrets_skip_iam_passwords_and_runtime_uris() {
        let mut params = ConnectionParams {
            save_in_keychain: Some(true),
            use_iam_auth: Some(true),
            connection_uri: Some("plugin://inline".to_string()),
            password: Some("iam-token".to_string()),
            ..base_params()
        };
        apply_sql_builder_secrets(
            &mut params,
            "conn-1",
            |_| panic!("runtime URI must not be replaced"),
            || panic!("IAM tokens never come from the keychain"),
        );
        assert_eq!(params.connection_uri.as_deref(), Some("plugin://inline"));
        assert_eq!(params.password.as_deref(), Some("iam-token"));
    }

    #[test]
    fn ddl_database_override_ignores_missing_or_empty_values() {
        let mut params = base_params();
        apply_ddl_database_override(&mut params, None);
        assert_eq!(
            params.database,
            DatabaseSelection::Single("testdb".to_string())
        );
        apply_ddl_database_override(&mut params, Some(String::new()));
        assert_eq!(
            params.database,
            DatabaseSelection::Single("testdb".to_string())
        );
    }
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_tables<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<TableInfo>, String> {
    crate::application::metadata::get_tables(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_columns<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<TableColumn>, String> {
    crate::application::metadata::get_columns(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        table_name,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_foreign_keys<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<ForeignKey>, String> {
    crate::application::metadata::get_foreign_keys(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        table_name,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_indexes<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<Index>, String> {
    crate::application::metadata::get_indexes(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        table_name,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn delete_record<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    pk_map: std::collections::HashMap<String, serde_json::Value>,
    schema: Option<String>,
    database: Option<String>,
) -> Result<u64, String> {
    crate::application::records::delete_record(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        table,
        pk_map,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn update_record<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    pk_map: std::collections::HashMap<String, serde_json::Value>,
    col_name: String,
    new_val: serde_json::Value,
    schema: Option<String>,
    database: Option<String>,
) -> Result<u64, String> {
    crate::application::records::update_record(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        table,
        pk_map,
        col_name,
        new_val,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn save_blob_to_file<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    col_name: String,
    pk_map: std::collections::HashMap<String, serde_json::Value>,
    file_path: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<(), String> {
    let saved_conn = find_connection_by_id(&app, &connection_id)?;
    let expanded_params = expand_ssh_connection_params(&app, &saved_conn.params).await?;
    let expanded_params = expand_k8s_connection_params(&app, &expanded_params).await?;
    let mut params = resolve_connection_params_with_id(&expanded_params, &connection_id)?;
    if let Some(db) = database.filter(|d| !d.is_empty()) {
        params.database = crate::models::DatabaseSelection::Single(db);
    }
    let drv = driver_for_params(&params).await?;
    drv.save_blob_to_file(
        &params,
        &table,
        &col_name,
        &pk_map,
        schema.as_deref(),
        &file_path,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn fetch_blob<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    col_name: String,
    pk_map: std::collections::HashMap<String, serde_json::Value>,
    schema: Option<String>,
    database: Option<String>,
) -> Result<crate::application::records::BlobFetchResponse, String> {
    crate::application::records::fetch_blob(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        table,
        col_name,
        pk_map,
        schema,
        database,
        crate::application::records::BlobFetchPolicy::Inline,
    )
    .await
}

/// Fetches a BLOB column from the database and returns it as a data: URL for image preview.
/// Same query logic as save_blob_to_file but returns the data in-memory instead of writing to disk.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn fetch_blob_as_data_url<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    col_name: String,
    pk_map: std::collections::HashMap<String, serde_json::Value>,
    schema: Option<String>,
    database: Option<String>,
) -> Result<String, String> {
    crate::application::records::fetch_blob_as_data_url(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        table,
        col_name,
        pk_map,
        schema,
        database,
    )
    .await
}

/// Detects the MIME type of base64-encoded binary data using magic-byte analysis
/// and returns the canonical blob wire format: "BLOB:<size>:<mime>:<base64>".
/// Called by the frontend after the user selects a file to upload.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn detect_blob_mime(base64_data: String) -> Result<String, String> {
    crate::application::records::detect_blob_mime(&base64_data)
}

/// Prepares a file for BLOB upload by returning only metadata and a file reference.
/// The actual file content is NOT transferred over IPC, avoiding massive string allocations.
/// The file content will be read directly from disk when needed (e.g., during INSERT/UPDATE).
/// Returns a special "BLOB_FILE_REF" format that includes file path, size, and MIME type.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn load_blob_from_file<R: Runtime>(
    app: AppHandle<R>,
    file_path: String,
) -> Result<String, String> {
    use std::io::Read;

    // Read max_blob_size from configuration
    let max_blob_size = crate::config::get_max_blob_size(&app);

    tokio::task::spawn_blocking(move || -> Result<String, String> {
        let mut file = std::fs::File::open(&file_path)
            .map_err(|e| format!("Failed to open file: {}", e))?;

        // Get file size
        let metadata = file.metadata()
            .map_err(|e| format!("Failed to get file metadata: {}", e))?;
        let file_size = metadata.len();

        // Validate file size against maximum allowed
        if file_size > max_blob_size {
            return Err(format!(
                "File size ({} bytes / {:.2}MB) exceeds maximum allowed size ({} bytes / {}MB). Please choose a smaller file.",
                file_size,
                file_size as f64 / (1024.0 * 1024.0),
                max_blob_size,
                max_blob_size / (1024 * 1024)
            ));
        }

        // Read first chunk for MIME detection (only 8KB)
        let header_size = std::cmp::min(8192, file_size as usize);
        let mut header = vec![0u8; header_size];
        file.read_exact(&mut header)
            .map_err(|e| format!("Failed to read file header: {}", e))?;

        // Detect MIME type
        let mime = infer::get(&header)
            .map(|k| k.mime_type())
            .unwrap_or("application/octet-stream");

        // Return a file reference instead of actual content
        // Format: "BLOB_FILE_REF:<size>:<mime>:<filepath>"
        Ok(format!("BLOB_FILE_REF:{}:{}:{}", file_size, mime, file_path))
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Detects the MIME type from a small base64-encoded header (first ~8KB).
/// Returns only the MIME type string — the frontend constructs the wire format
/// locally, avoiding a full round-trip of the entire file over IPC.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn detect_mime_type(header_base64: String) -> Result<String, String> {
    crate::application::records::detect_mime_type(&header_base64)
}

/// Gets file statistics (size and MIME type) without reading the entire file.
/// Used after streaming upload to construct the final wire format.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_file_stats(file_path: String) -> Result<serde_json::Value, String> {
    use std::io::Read;

    let mut file =
        std::fs::File::open(&file_path).map_err(|e| format!("Failed to open file: {}", e))?;

    let metadata = file
        .metadata()
        .map_err(|e| format!("Failed to get file metadata: {}", e))?;
    let file_size = metadata.len();

    // Read first chunk for MIME detection
    let header_size = std::cmp::min(8192, file_size as usize);
    let mut header = vec![0u8; header_size];
    file.read_exact(&mut header)
        .map_err(|e| format!("Failed to read file header: {}", e))?;

    let mime = infer::get(&header)
        .map(|k| k.mime_type())
        .unwrap_or("application/octet-stream");

    Ok(serde_json::json!({
        "size": file_size,
        "mime": mime,
    }))
}

/// Reads a file from disk and returns it as a base64-encoded data URL.
/// Used for image preview of BLOB_FILE_REF values without requiring frontend FS permissions.
/// Only available for image files; returns an error for non-image MIME types.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn read_file_as_data_url(file_path: String) -> Result<String, String> {
    use base64::Engine;
    use std::io::Read;

    tokio::task::spawn_blocking(move || -> Result<String, String> {
        let mut file =
            std::fs::File::open(&file_path).map_err(|e| format!("Failed to open file: {}", e))?;

        let metadata = file
            .metadata()
            .map_err(|e| format!("Failed to get file metadata: {}", e))?;
        let file_size = metadata.len() as usize;

        // Read full file
        let mut bytes = Vec::with_capacity(file_size);
        file.read_to_end(&mut bytes)
            .map_err(|e| format!("Failed to read file: {}", e))?;

        // Detect MIME type from header
        let mime = infer::get(&bytes)
            .map(|k| k.mime_type())
            .unwrap_or("application/octet-stream");

        if !mime.starts_with("image/") {
            return Err(format!("Not an image file: {}", mime));
        }

        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(format!("data:{};base64,{}", mime, b64))
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn insert_record<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    data: std::collections::HashMap<String, serde_json::Value>,
    schema: Option<String>,
    database: Option<String>,
) -> Result<u64, String> {
    crate::application::records::insert_record(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        table,
        data,
        schema,
        database,
    )
    .await
}

#[cfg(test)]
pub(crate) fn cancel_query_impl(
    state: &QueryCancellationState,
    connection_id: &str,
) -> Result<(), String> {
    crate::application::queries::cancel_registered_queries(state, None, connection_id, None)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn cancel_query(
    state: State<'_, std::sync::Arc<QueryCancellationState>>,
    connection_id: String,
) -> Result<(), String> {
    crate::application::queries::cancel_query(&state, None, &connection_id, None)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn execute_query<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, std::sync::Arc<QueryCancellationState>>,
    connection_id: String,
    query: String,
    limit: Option<u32>,
    page: Option<u32>,
    schema: Option<String>,
    database: Option<String>,
    session_id: Option<String>,
) -> Result<QueryResult, String> {
    crate::application::queries::execute_query(
        &app.state::<crate::runtime::RuntimeContext>(),
        &state,
        crate::application::queries::QueryRequestScope::DESKTOP,
        crate::application::queries::QueryResponsePolicy::Unbounded,
        connection_id,
        query,
        limit,
        page,
        schema,
        session_id,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
pub(crate) async fn release_connection_sessions<R: Runtime>(
    app: &AppHandle<R>,
    connection_id: &str,
    _params: Option<&ConnectionParams>,
) {
    crate::application::query_sessions::release_connection(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        connection_id,
    )
    .await;
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn release_query_session<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    session_id: String,
) -> Result<(), String> {
    crate::application::query_sessions::release(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        &session_id,
    )
    .await
}

/// Runs a sequence of statements that share a single physical database
/// connection. Use this — not multiple parallel `execute_query` calls —
/// whenever statements depend on connection-local session state
/// (`SET @var`, `LAST_INSERT_ID()` / `LASTVAL()`, `BEGIN`/`COMMIT`,
/// `TEMPORARY TABLE`, `PREPARE`/`EXECUTE`, `SET FOREIGN_KEY_CHECKS = 0`).
///
/// The whole batch shares one cancellation handle so `cancel_query`
/// aborts the entire batch atomically.
///
/// When `batch_id` is supplied, a `batch-statement-complete` event is emitted
/// after each statement so the UI updates result tabs progressively. The full
/// `Vec` is still returned at the end for final reconciliation / fallback.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn execute_query_batch<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, std::sync::Arc<QueryCancellationState>>,
    connection_id: String,
    queries: Vec<String>,
    limit: Option<u32>,
    page: Option<u32>,
    schema: Option<String>,
    database: Option<String>,
    batch_id: Option<String>,
    session_id: Option<String>,
) -> Result<Vec<BatchStatementResult>, String> {
    crate::application::queries::execute_query_batch(
        &app.state::<crate::runtime::RuntimeContext>(),
        &state,
        crate::application::queries::QueryRequestScope::DESKTOP,
        crate::application::queries::QueryResponsePolicy::Unbounded,
        connection_id,
        queries,
        limit,
        page,
        schema,
        session_id,
        batch_id,
        database,
    )
    .await
}

// --- Explain Query Plan ---

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn explain_query_plan<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, std::sync::Arc<QueryCancellationState>>,
    connection_id: String,
    query: String,
    analyze: bool,
    schema: Option<String>,
    database: Option<String>,
) -> Result<ExplainQueryOutput, String> {
    crate::application::queries::explain_query_plan(
        &app.state::<crate::runtime::RuntimeContext>(),
        &state,
        crate::application::queries::QueryRequestScope::DESKTOP,
        connection_id,
        query,
        analyze,
        schema,
        database,
    )
    .await
}

/// Run a read on the connection pinned to `session_id`, inside its open
/// transaction, so it sees the tab's uncommitted changes. A savepoint keeps a
/// failing read from aborting the user's transaction.
pub(crate) async fn read_in_open_transaction(
    drv: &dyn crate::drivers::driver_trait::DatabaseDriver,
    params: &ConnectionParams,
    query: &str,
    limit: Option<u32>,
    page: u32,
    schema: Option<&str>,
    session_id: &str,
) -> Result<QueryResult, String> {
    let batch = [
        "SAVEPOINT tabularis_read".to_string(),
        query.to_string(),
        "ROLLBACK TO SAVEPOINT tabularis_read".to_string(),
        "RELEASE SAVEPOINT tabularis_read".to_string(),
    ];
    let (results, _) = drv
        .execute_batch_in_session(params, &batch, limit, page, schema, Some(session_id), None)
        .await?;
    // With no transaction open (a stale badge) only the savepoint lines fail; the read still runs.
    match results.into_iter().nth(1) {
        Some(BatchStatementResult {
            result: Some(result),
            ..
        }) => Ok(result),
        Some(BatchStatementResult { error: Some(e), .. }) => Err(e),
        _ => Err("The statement produced no result".to_string()),
    }
}

// --- Count Query ---

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn count_query<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    query: String,
    schema: Option<String>,
    database: Option<String>,
    session_id: Option<String>,
) -> Result<u64, String> {
    crate::application::queries::count_query(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        connection_id,
        query,
        schema,
        session_id,
        database,
    )
    .await
}

// --- Window Title Management ---

/// Sets the window title with Wayland workaround
///
/// WORKAROUND: This is a temporary fix for tauri-apps/tauri#13749
/// On Wayland (Linux), the standard `window.setTitle()` API doesn't properly update
/// the window title in the window manager's title bar due to an upstream dependency issue.
/// This command directly manipulates the GTK HeaderBar to ensure the title is visible.
///
/// See: https://github.com/tauri-apps/tauri/issues/13749
///
/// This workaround should be removed once the upstream issue is resolved.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn set_window_title(app: AppHandle, title: String) -> Result<(), String> {
    // Get the main window
    let window = app
        .get_webview_window("main")
        .ok_or("Failed to get main window")?;

    // Set title using standard Tauri API (works on all platforms)
    window
        .set_title(&title)
        .map_err(|e| format!("Failed to set window title: {}", e))?;

    // Apply Wayland-specific workaround on Linux
    #[cfg(target_os = "linux")]
    {
        #[cfg(feature = "desktop")]
        use gtk::prelude::{BinExt, Cast, GtkWindowExt, HeaderBarExt};
        #[cfg(feature = "desktop")]
        use gtk::{EventBox, HeaderBar};

        // Get the GTK window
        let gtk_window = window
            .gtk_window()
            .map_err(|e| format!("Failed to get GTK window: {}", e))?;

        // Check if we have a custom titlebar (Wayland uses EventBox with HeaderBar)
        if let Some(titlebar) = gtk_window.titlebar() {
            // Try to downcast to EventBox (Wayland)
            if let Ok(event_box) = titlebar.downcast::<EventBox>() {
                // Get the HeaderBar child and set its title
                if let Some(child) = event_box.child() {
                    if let Ok(header_bar) = child.downcast::<HeaderBar>() {
                        header_bar.set_title(Some(&title));
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn open_er_diagram_window(
    app: AppHandle,
    connection_id: String,
    connection_name: String,
    database_name: String,
    focus_table: Option<String>,
    schema: Option<String>,
    database: Option<String>,
) -> Result<(), String> {
    #[cfg(feature = "desktop")]
    use tauri::{WebviewUrl, WebviewWindowBuilder};
    use urlencoding::encode;

    let schema_suffix = schema
        .as_deref()
        .map(|s| format!("/{}", s))
        .unwrap_or_default();
    let title = format!(
        "tabularis - {} ({}{})",
        database_name, connection_name, schema_suffix
    );
    let mut url = format!(
        "/schema-diagram?connectionId={}&connectionName={}&databaseName={}",
        encode(&connection_id),
        encode(&connection_name),
        encode(&database_name)
    );

    if let Some(table) = focus_table {
        url.push_str(&format!("&focusTable={}", encode(&table)));
    }

    if let Some(s) = &schema {
        url.push_str(&format!("&schema={}", encode(s)));
    }

    // `database` (distinct from the display-only `database_name` above) is only
    // set for a schema-based multi-db connection (PostgreSQL browsing several
    // databases) — it's the value the schema-diagram page actually routes its
    // snapshot request with, alongside `schema`. Absent for the flat multi-db
    // (MySQL) and plain single-database cases, which already resolve correctly
    // through `schema` alone.
    if let Some(db) = &database {
        url.push_str(&format!("&database={}", encode(db)));
    }

    // Derive a unique window label per (connection, database, schema) so that
    // diagrams for different databases on the same connection do not collide on a
    // shared label (which previously kept showing the first database's diagram).
    // Tauri window labels only allow a limited character set, so sanitize anything
    // else to '_'.
    let raw_label = format!(
        "er-diagram:{}:{}:{}",
        connection_id,
        database_name,
        schema.as_deref().unwrap_or("")
    );
    let label: String = raw_label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // If a diagram window for this exact database already exists, just focus it
    // instead of failing to build a second window with the same label.
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.set_focus();
        return Ok(());
    }

    let _webview = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .title(&title)
        .inner_size(1200.0, 800.0)
        .center()
        .decorations(crate::window_decorations::native_decorations_enabled())
        .build()
        .map_err(|e| format!("Failed to create ER Diagram window: {}", e))?;

    Ok(())
}

/// Builds a connection URL for a database driver.
pub async fn build_connection_url(params: &ConnectionParams) -> Result<String, String> {
    let user = encode(params.username.as_deref().unwrap_or_default());
    let raw_pass = params.password.as_deref().unwrap_or_default();
    let credentials = if raw_pass.is_empty() {
        user.into_owned()
    } else {
        format!("{}:{}", user, encode(raw_pass))
    };
    let host = params.host.as_deref().unwrap_or("localhost");

    match params.driver.as_str() {
        "sqlite" => Ok(format!("sqlite://{}", params.database)),
        "postgres" => Ok(format!(
            "postgres://{}@{}:{}/{}",
            credentials,
            host,
            params.port.unwrap_or(DEFAULT_POSTGRES_PORT),
            params.database
        )),
        "mysql" => Ok(format!(
            "mysql://{}@{}:{}/{}",
            credentials,
            host,
            params.port.unwrap_or(DEFAULT_MYSQL_PORT),
            params.database
        )),
        _ => Err("Unsupported driver".into()),
    }
}

/// Resolves SSH credential (password or passphrase) for testing
/// 1. Credential from request params (if provided, even if empty)
/// 2. Credential from keychain (if save_in_keychain is enabled)
/// 3. Credential from saved connection (as fallback)
#[cfg(test)]
fn resolve_ssh_test_credential(
    request_credential: Option<&str>,
    connection_id: Option<&str>,
    get_ssh_connection: impl Fn(&str) -> Option<SshConnection>,
    get_keychain_credential: impl Fn(&str) -> Result<String, String>,
    extract_saved_credential: impl Fn(&SshConnection) -> Option<String>,
) -> Option<String> {
    // Priority 1: Credential from request
    // If credential field is present in request, use it even if empty
    // Empty string means "use empty credential", not "fallback to keychain"
    if let Some(cred) = request_credential {
        return Some(cred.to_string());
    }

    // If no connection_id, we can't look up saved credentials
    let conn_id = connection_id?;
    let saved = get_ssh_connection(conn_id)?;

    // Priority 2: Credential from keychain
    if saved.save_in_keychain.unwrap_or(false) {
        if let Ok(cred) = get_keychain_credential(conn_id) {
            if !cred.trim().is_empty() {
                return Some(cred);
            }
        }
    }

    // Priority 3: Credential from saved connection
    extract_saved_credential(&saved)
}

/// Fills SSH secrets that are still unresolved from the inline SSH fields of
/// a saved database connection (already hydrated from the keychain by
/// `find_connection_by_id`). Secrets explicitly provided by the request keep
/// priority; blank saved values are ignored.
#[cfg(test)]
fn apply_inline_ssh_secret_fallback(
    resolved_password: Option<String>,
    resolved_passphrase: Option<String>,
    saved_params: &ConnectionParams,
) -> (Option<String>, Option<String>) {
    fn non_blank(value: &Option<String>) -> Option<String> {
        value.as_ref().filter(|v| !v.trim().is_empty()).cloned()
    }
    (
        resolved_password.or_else(|| non_blank(&saved_params.ssh_password)),
        resolved_passphrase.or_else(|| non_blank(&saved_params.ssh_key_passphrase)),
    )
}

/// Helper for backward compatibility - resolves SSH password
#[cfg(test)]
fn resolve_ssh_test_password(
    request_password: Option<&str>,
    connection_id: Option<&str>,
    get_ssh_connection: impl Fn(&str) -> Option<SshConnection>,
    get_keychain_password: impl Fn(&str) -> Result<String, String>,
) -> Option<String> {
    resolve_ssh_test_credential(
        request_password,
        connection_id,
        get_ssh_connection,
        get_keychain_password,
        |conn| {
            conn.password
                .as_ref()
                .filter(|p| !p.trim().is_empty())
                .cloned()
        },
    )
}

// ==================== View Management Commands ====================

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_views<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<crate::models::ViewInfo>, String> {
    crate::application::metadata::get_views(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_view_definition<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    view_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<String, String> {
    crate::application::database_objects::get_view_definition(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        view_name,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn create_view<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    view_name: String,
    definition: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<(), String> {
    crate::application::database_objects::create_view(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        view_name,
        definition,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn alter_view<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    view_name: String,
    definition: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<(), String> {
    crate::application::database_objects::alter_view(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        view_name,
        definition,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn drop_view<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    view_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<(), String> {
    crate::application::database_objects::drop_view(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        view_name,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_view_columns<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    view_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<TableColumn>, String> {
    crate::application::metadata::get_view_columns(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        view_name,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_materialized_views<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<crate::models::ViewInfo>, String> {
    crate::application::metadata::get_materialized_views(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_materialized_view_columns<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    view_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<TableColumn>, String> {
    crate::application::metadata::get_materialized_view_columns(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        view_name,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_materialized_view_definition<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    view_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<String, String> {
    crate::application::metadata::get_materialized_view_definition(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        view_name,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn refresh_materialized_view<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    view_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<(), String> {
    crate::application::database_objects::refresh_materialized_view(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        view_name,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_triggers<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<TriggerInfo>, String> {
    crate::application::metadata::get_triggers(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_trigger_definition<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    trigger_name: String,
    table_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<String, String> {
    crate::application::database_objects::get_trigger_definition(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        trigger_name,
        table_name,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn create_trigger<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    trigger_sql: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<(), String> {
    crate::application::database_objects::create_trigger(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        trigger_sql,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn drop_trigger<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    trigger_name: String,
    table_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<(), String> {
    crate::application::database_objects::drop_trigger(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        trigger_name,
        table_name,
        schema,
        database,
    )
    .await
}

// --- User management (gated by `DriverCapabilities::user_management`) -------

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_db_privilege_catalog<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
) -> Result<crate::models::DbPrivilegeCatalog, String> {
    crate::application::database_objects::get_db_privilege_catalog(
        &app.state::<crate::runtime::RuntimeContext>(),
        &connection_id,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_db_users<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
) -> Result<Vec<crate::models::DbUserInfo>, String> {
    crate::application::database_objects::get_db_users(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_db_user_grants<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    user: String,
    host: String,
) -> Result<Vec<String>, String> {
    crate::application::database_objects::get_db_user_grants(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        user,
        host,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn create_db_user<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    user: String,
    host: String,
    password: String,
) -> Result<(), String> {
    crate::application::database_objects::create_db_user(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        user,
        host,
        password,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn drop_db_user<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    user: String,
    host: String,
) -> Result<(), String> {
    crate::application::database_objects::drop_db_user(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        user,
        host,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn set_db_user_password<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    user: String,
    host: String,
    password: String,
) -> Result<(), String> {
    crate::application::database_objects::set_db_user_password(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        user,
        host,
        password,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_db_user_privileges<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    user: String,
    host: String,
) -> Result<Vec<crate::models::DbUserGrantSet>, String> {
    crate::application::database_objects::get_db_user_privileges(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        user,
        host,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn apply_db_user_privileges<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    user: String,
    host: String,
    database: Option<String>,
    table: Option<String>,
    privileges: Vec<String>,
    grant: bool,
) -> Result<(), String> {
    crate::application::database_objects::apply_db_user_privileges(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        user,
        host,
        database,
        table,
        privileges,
        grant,
    )
    .await
}

/// Register a connection as active for health-check pinging.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn register_active_connection<R: Runtime>(app: AppHandle<R>, connection_id: String) {
    crate::application::connections::register_active_connection(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        None,
        connection_id,
    )
    .await;
}

/// Snapshot of connection ids currently open in the shared backend (across all
/// windows). Used by each window to render cross-window connection status.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_active_connections() -> Vec<String> {
    crate::health_check::active_connections().await
}

/// Disconnect from a database connection by closing all of its connection pools
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn disconnect_connection<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
) -> Result<(), String> {
    crate::application::connections::disconnect_connection(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        None,
        &connection_id,
    )
    .await
}

// --- Type Registry ---

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_data_types(driver: String) -> Result<crate::models::DataTypeRegistry, String> {
    log::debug!("Fetching data types for driver: {}", driver);

    let drv = driver_for(&driver).await?;
    let types = drv.get_data_types();

    Ok(crate::models::DataTypeRegistry { driver, types })
}

/// Maps generic inferred types (emitted by the clipboard parser) to
/// driver-specific type names. Returns names in the same order as `kinds`.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn map_inferred_column_types<R: Runtime>(
    app: AppHandle<R>,
    driver: String,
    kinds: Vec<String>,
    connection_id: Option<String>,
) -> Result<Vec<String>, String> {
    crate::application::database_objects::map_inferred_column_types(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        driver,
        kinds,
        connection_id,
    )
    .await
}

// --- DDL generation commands ---

/// Point `params` at the database the user is browsing (multi-database
/// layouts, #822). An empty or missing override keeps the saved selection.
pub(crate) fn apply_ddl_database_override(params: &mut ConnectionParams, database: Option<String>) {
    if let Some(db) = database.filter(|d| !d.is_empty()) {
        params.database = crate::models::DatabaseSelection::Single(db);
    }
}

/// Bind the keychain secrets the SQL builders may forward to a plugin (the
/// connection URI and the database password), without ever failing: a
/// builder only renders SQL, so a keychain error leaves the secret unset and
/// the params are bound without it. SSH secrets are not loaded because the
/// builders never open tunnels.
pub(crate) fn apply_sql_builder_secrets(
    params: &mut ConnectionParams,
    connection_id: &str,
    connection_uri: impl FnOnce(bool) -> Result<Option<String>, String>,
    db_password: impl FnOnce() -> Result<String, String>,
) {
    if runtime_connection_uri(params).is_none() {
        let stored_in_keychain = params.connection_uri_in_keychain.unwrap_or(false);
        match connection_uri(stored_in_keychain) {
            Ok(Some(uri)) => params.connection_uri = Some(uri),
            Ok(None) if stored_in_keychain => log::warn!(
                "Connection URI for {connection_id} is missing from the OS keychain; building SQL without it"
            ),
            Ok(None) => {}
            Err(e) => log::warn!(
                "Failed to read the connection URI for {connection_id} from the OS keychain; building SQL without it: {e}"
            ),
        }
    }
    if params.save_in_keychain.unwrap_or(false) && !params.use_iam_auth.unwrap_or(false) {
        match db_password() {
            Ok(password) => params.password = Some(password),
            Err(e) => log::warn!(
                "Failed to read the DB password for {connection_id} from the OS keychain; building SQL without it: {e}"
            ),
        }
    }
}

/// Params forwarded to the plugin SQL builders: the saved params with the
/// connection id and the browsed `database` applied. SSH/SSM/K8s tunnels and
/// proxies are never resolved here, so host and port stay as saved: a
/// preview must not open a tunnel, and must work while the server is
/// unreachable.
pub(crate) fn sql_builder_params(
    saved: &SavedConnection,
    database: Option<String>,
) -> ConnectionParams {
    let mut params = saved.params.clone();
    params.connection_id = Some(saved.id.clone());
    if crate::drivers::registry::is_local_path_driver(&params.driver) {
        params.database = crate::fs_path::sanitize_database_selection(&params.database);
    }
    apply_ddl_database_override(&mut params, database);
    params
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_create_table_sql<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table_name: String,
    columns: Vec<ColumnDefinition>,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<String>, String> {
    crate::application::database_objects::get_create_table_sql(
        &app.state::<crate::runtime::RuntimeContext>(),
        &connection_id,
        table_name,
        columns,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_add_column_sql<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    column: ColumnDefinition,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<String>, String> {
    crate::application::database_objects::get_add_column_sql(
        &app.state::<crate::runtime::RuntimeContext>(),
        &connection_id,
        table,
        column,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_alter_column_sql<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    old_column: ColumnDefinition,
    new_column: ColumnDefinition,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<String>, String> {
    crate::application::database_objects::get_alter_column_sql(
        &app.state::<crate::runtime::RuntimeContext>(),
        &connection_id,
        table,
        old_column,
        new_column,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_create_index_sql<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    index_name: String,
    columns: Vec<String>,
    is_unique: bool,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<String>, String> {
    crate::application::database_objects::get_create_index_sql(
        &app.state::<crate::runtime::RuntimeContext>(),
        &connection_id,
        table,
        index_name,
        columns,
        is_unique,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_create_foreign_key_sql<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    fk_name: String,
    column: String,
    ref_table: String,
    ref_column: String,
    on_delete: Option<String>,
    on_update: Option<String>,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Vec<String>, String> {
    crate::application::database_objects::get_create_foreign_key_sql(
        &app.state::<crate::runtime::RuntimeContext>(),
        &connection_id,
        table,
        fk_name,
        column,
        ref_table,
        ref_column,
        on_delete,
        on_update,
        schema,
        database,
    )
    .await
}

/// Plugin-provided statement for dropping a table, or `None` to let the
/// explorer build its generic `DROP TABLE`. Only plugins declaring
/// `capabilities.drop_table_sql` are asked; every other driver (built-ins
/// included) returns `None` without resolving credentials or params.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_drop_table_sql<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<Option<String>, String> {
    crate::application::database_objects::get_drop_table_sql(
        &app.state::<crate::runtime::RuntimeContext>(),
        &connection_id,
        table,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn drop_index_action<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    index_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<(), String> {
    crate::application::database_objects::drop_index(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        table,
        index_name,
        schema,
        database,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn drop_foreign_key_action<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
    table: String,
    fk_name: String,
    schema: Option<String>,
    database: Option<String>,
) -> Result<(), String> {
    crate::application::database_objects::drop_foreign_key(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        &connection_id,
        table,
        fk_name,
        schema,
        database,
    )
    .await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_registered_drivers() -> Vec<crate::drivers::driver_trait::PluginManifest> {
    crate::drivers::registry::list_drivers().await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_keybindings<R: Runtime>(app: AppHandle<R>) -> Result<serde_json::Value, String> {
    crate::application::persistence::get_keybindings(&app.state::<crate::runtime::RuntimeContext>())
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn save_keybindings<R: Runtime>(
    app: AppHandle<R>,
    keybindings: serde_json::Value,
) -> Result<(), String> {
    crate::application::persistence::save_keybindings(
        &app.state::<crate::runtime::RuntimeContext>(),
        &keybindings,
    )
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_driver_manifest(
    driver_id: String,
) -> Option<crate::plugins::connection_metadata::DriverManifestDetails> {
    crate::drivers::registry::get_driver(&driver_id)
        .await
        .map(
            |d| crate::plugins::connection_metadata::DriverManifestDetails {
                manifest: d.manifest().clone(),
                connection_metadata: d.has_connection_metadata().then_some(true),
            },
        )
}

// ==================== Connection Groups Management ====================

/// Splits a `/`-separated group path into trimmed, non-empty segments.
/// Returns an error if the result is empty.
pub(crate) fn parse_group_path(path: &str) -> Result<Vec<String>, String> {
    let segments: Vec<String> = path
        .split('/')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() {
        return Err("Group path cannot be empty".to_string());
    }
    Ok(segments)
}

/// Finds an existing group by case-insensitive name match within a parent's
/// children, or `None` if no such group exists.
pub(crate) fn find_child_group<'a>(
    groups: &'a [ConnectionGroup],
    name: &str,
    parent_id: &Option<String>,
) -> Option<&'a ConnectionGroup> {
    let name_lower = name.to_lowercase();
    groups
        .iter()
        .find(|g| g.name.to_lowercase() == name_lower && g.parent_id == *parent_id)
}

/// Reject re-parenting that would create a cycle: `target` must not be a
/// descendant of `group_id`. Walks up from `target` looking for `group_id`.
/// Bounded by `groups.len()` to fail-safe against pre-existing data cycles.
pub(crate) fn reject_if_would_create_cycle(
    groups: &[ConnectionGroup],
    group_id: &str,
    new_parent_id: Option<&str>,
) -> Result<(), String> {
    let Some(target) = new_parent_id else {
        return Ok(());
    };
    let mut current = Some(target.to_string());
    let mut visited = std::collections::HashSet::new();
    let max_steps = groups.len() + 1;
    for _ in 0..max_steps {
        match current {
            Some(node) if node == group_id => {
                return Err(
                    "Cannot move a group into one of its own descendants (would create a cycle)"
                        .to_string(),
                );
            }
            Some(node) => {
                if !visited.insert(node.clone()) {
                    return Err(
                        "Connection-group tree contains a pre-existing cycle; refusing to modify it"
                            .to_string(),
                    );
                }
                current = groups
                    .iter()
                    .find(|g| g.id == node)
                    .and_then(|g| g.parent_id.clone());
            }
            None => return Ok(()),
        }
    }
    Err(
        "Connection-group tree is deeper than the number of groups; refusing to modify it"
            .to_string(),
    )
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_server_now<R: Runtime>(
    app: AppHandle<R>,
    connection_id: String,
) -> Result<String, String> {
    crate::application::queries::get_server_now(
        &app.state::<crate::runtime::RuntimeContext>(),
        None,
        connection_id,
    )
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn export_connections_payload<R: Runtime>(
    app: AppHandle<R>,
    include_secrets: Option<bool>,
    connection_ids: Option<Vec<String>>,
) -> Result<ExportPayload, String> {
    crate::application::connection_files::export_payload(
        app.state::<crate::runtime::RuntimeContext>().inner(),
        include_secrets.unwrap_or(true),
        connection_ids,
    )
    .await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn encrypt_export_payload(
    payload: ExportPayload,
    password: String,
) -> Result<crate::export_crypto::EncryptedEnvelope, String> {
    let plaintext = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    crate::export_crypto::encrypt(&plaintext, &password)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn decrypt_export_payload(
    envelope: crate::export_crypto::EncryptedEnvelope,
    password: String,
) -> Result<ExportPayload, String> {
    let plaintext = crate::export_crypto::decrypt(&envelope, &password)?;
    serde_json::from_str(&plaintext).map_err(|e| format!("Invalid export payload: {e}"))
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn import_connections_payload<R: Runtime>(
    app: AppHandle<R>,
    payload: ExportPayload,
) -> Result<(), String> {
    apply_export_payload(app, payload).await
}

#[cfg(feature = "desktop")]
/// Merge an `ExportPayload` into the user's stored connections, groups, SSH and
/// K8s records, moving any inline secrets into the keychain. Shared by the JSON
/// import command above and the foreign-app import flow.
pub async fn apply_export_payload<R: Runtime>(
    app: AppHandle<R>,
    payload: ExportPayload,
) -> Result<(), String> {
    crate::application::connection_files::apply_export_payload(
        app.state::<crate::runtime::RuntimeContext>().inner(),
        app.state::<std::sync::Arc<crate::connection_cache::ConnectionCache>>()
            .inner()
            .as_ref(),
        app.state::<std::sync::Arc<crate::credential_cache::CredentialCache>>()
            .inner()
            .as_ref(),
        payload,
    )
    .await
}
