# UI plugin sandbox design

## Boundary

Each UI extension module runs in a sandboxed `tabularis-plugin` iframe with
scripts enabled, same-origin access disabled, and a guest CSP that blocks
direct network connections. Google OAuth token exchange is the one network
operation proxied by the host to a fixed endpoint and fixed form-field
allow-list. The host and guest communicate over a transferred
`MessagePort`. Host requests are checked against an explicit method allow-list;
there is no generic Tauri command or `invoke` method in the guest API.

## Exposed capabilities

- Slot context, active connection metadata, theme data, and the plugin's own
  settings and translations.
- SQL on the currently active connection, after a best-effort statement-type
  filter rejects recognized writes. This filter is not a security boundary:
  side-effecting `SELECT` functions and database-specific constructs may pass.
  Use read-only database credentials for an actual read-only guarantee.
- Writes to the calling plugin's own JSON settings, capped at 64 KiB per value.
- Native info, warning, and error dialogs; opening HTTP(S) URLs without URL
  credentials.
- Plugin-authored React modals rendered inside that plugin's iframe.

The frame receives no connection credentials, filesystem interface, or generic
backend command proxy. Query results are available to the plugin that requested
them. Host callbacks such as `onDatabaseChange`, `setExtraField`, and
`setCredentialFieldsHidden` are represented by plain context data and
slot-specific RPC methods; functions are never sent through `postMessage`.

RPC requests time out after 30 seconds. Internal guest resources use reserved
`__runtime__.js` and `__host.css__` paths so plugin files named `runtime.js` or
`host.css` remain addressable.

## Plugin migration

Plugins continue to import hooks from `@tabularis/plugin-api`; rebuild against
API `0.3.0` and require host API `0.3.0`. `setSetting` and `setSettings` now
return promises, so await them when subsequent work depends on persistence.
Modals remain React-based, but render within the plugin iframe rather than the
host React tree. Translation lookup supports dotted keys and `{{name}}`
interpolation; plugins that rely on advanced i18next features need to provide
their own formatting or simplify those entries. `openUrl` accepts only HTTP(S)
URLs and rejects embedded credentials. Replace direct Google token-endpoint
`fetch` calls with `exchangeGoogleOAuthToken`; other guest network requests
remain blocked. `usePluginQuery` applies only a best-effort statement filter,
not a read-only security guarantee.

## Related connection-export hardening

This branch also changes connection export: secrets are omitted by default,
and requesting them requires native confirmation. The backup flow uses the
separate internal `export_connections_payload_unconfirmed` path only after its
own explicit backup action. This is related credential-exposure hardening, but
it is separate from the iframe RPC boundary.

Plugins must not import Tauri APIs or use `window.__TAURI__` directly. The host
injects `__TABULARIS_API__` only in the guest frame; `__TABULARIS_EXPLAIN__` is
also available there for EXPLAIN parser bundles.
