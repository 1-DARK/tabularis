# @tabularis/plugin-api

Public API surface for [Tabularis](https://github.com/TabularisDB/tabularis) plugin UI extensions.

Plugin bundles import hooks, the `defineSlot` helper, and shared types from this package. The Tabularis host injects the actual runtime implementation — the published package ships as thin stubs, so your bundle stays small and the host remains the single source of truth.

## Install

```bash
npm install --save-dev @tabularis/plugin-api
# or: pnpm add -D @tabularis/plugin-api
```

Treat `@tabularis/plugin-api` and `react` as Vite externals when building your IIFE bundle — the host provides both at runtime.

## Quick start — a typed slot component

```tsx
import { defineSlot, usePluginConnection } from "@tabularis/plugin-api";

const MySlot = defineSlot(
  "row-editor-sidebar.field.after",
  ({ context }) => {
    const { driver } = usePluginConnection();
    // context.columnName is `string` (not `string | undefined`) because the
    // chosen slot always provides it.
    return <div>{driver} · {context.columnName}</div>;
  },
);

export default MySlot.component;
```

## Hooks

| Hook | Returns | Purpose |
|------|---------|---------|
| `usePluginQuery()` | `{ executeQuery, loading, error }` | Execute read-only SQL on the active connection; writes and unrecognized statements are rejected |
| `usePluginConnection()` | `{ connectionId, driver, schema }` | Active connection metadata |
| `usePluginToast()` | `{ showInfo, showError, showWarning }` | System notifications |
| `usePluginModal()` | `{ openModal, closeModal }` | Modal with custom React content rendered inside the plugin iframe |
| `usePluginSetting(pluginId)` | `{ getSetting, setSetting, setSettings }` | Read/write the plugin's own settings |
| `usePluginTheme()` | `{ themeId, themeName, isDark, colors }` | Current theme tokens |
| `usePluginTranslation(pluginId)` | `t(key)` | Plugin-scoped i18next translator |
| `openUrl(url)` | `Promise<void>` | Open a URL in the system browser |

Plugin UI runs in a sandboxed iframe and has no direct Tauri `invoke` access.
The host exposes only the operations represented by these APIs: read-only
queries on the active connection, plugin-owned settings, notifications,
translations, theme and slot context, and opening HTTP(S) URLs. Modal content
stays inside the plugin iframe. Setting writes are asynchronous; await
`setSetting` or `setSettings` when subsequent work depends on persistence.

All hooks must run inside a React component loaded by Tabularis. Calling them outside the host throws a clear error instead of failing silently.

## Compatibility

The package exports `API_VERSION` and `MIN_HOST_VERSION`. Plugin authors can opt in to a fail-fast compatibility check at component entry:

```ts
import { assertHostCompat } from "@tabularis/plugin-api";
assertHostCompat(); // throws if the running Tabularis is older than MIN_HOST_VERSION
```

| Package version | Minimum Tabularis API | Notes |
|-----------------|-----------------------|-------|
| `0.2.0`         | `0.2.0` | Sandboxed UI API with explicit RPC capabilities |

## Slot reference

See the full slot list and context contracts in [`plugins/PLUGIN_GUIDE.md` § 3b](https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md) and the typed `SlotContextMap` in `src/slots.ts`.

## License

Apache-2.0, same as Tabularis.
