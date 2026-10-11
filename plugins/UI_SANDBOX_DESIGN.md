# UI plugin sandbox design

## Boundary

Each UI extension module runs in a sandboxed `tabularis-plugin` iframe with
scripts enabled, same-origin access disabled, and a guest CSP that blocks
network connections. The host and guest communicate over a transferred
`MessagePort`. Host requests are checked against an explicit method allow-list;
there is no generic Tauri command or `invoke` method in the guest API.

## Exposed capabilities

- Slot context, active connection metadata, theme data, and the plugin's own
  settings and translations.
- Read-only SQL on the currently active connection. The host rejects writes
  and statements the SQL classifier cannot identify as read-only.
- Writes to the calling plugin's own JSON settings, capped at 64 KiB per value.
- Native info, warning, and error dialogs; opening HTTP(S) URLs without URL
  credentials.
- Plugin-authored React modals rendered inside that plugin's iframe.

The frame receives no connection credentials, filesystem interface, or generic
backend command proxy. Database results returned by the read-only query API are
available to the plugin that requested them.

## Plugin migration

Plugins continue to import hooks from `@tabularis/plugin-api`; rebuild against
API `0.2.0` and require host API `0.2.0`. `setSetting` and `setSettings` now
return promises, so await them when subsequent work depends on persistence.
Modals remain React-based, but render within the plugin iframe rather than the
host React tree. Translation lookup supports dotted keys and `{{name}}`
interpolation; plugins that rely on advanced i18next features need to provide
their own formatting or simplify those entries. `openUrl` accepts only HTTP(S)
URLs and rejects embedded credentials. `usePluginQuery` is read-only.

Plugins must not import Tauri APIs or use `window.__TAURI__` directly. The host
injects `__TABULARIS_API__` only in the guest frame; `__TABULARIS_EXPLAIN__` is
also available there for EXPLAIN parser bundles.
