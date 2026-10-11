import { useContext, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { message } from "@tauri-apps/plugin-dialog";
import { openUrl as openExternal } from "@tauri-apps/plugin-opener";
import { useTranslation } from "react-i18next";
import { useSettings } from "../../hooks/useSettings";
import { ThemeContext } from "../../contexts/ThemeContext";
import { isReadOnlyQuery } from "../../utils/sqlAnalysis";
import type { SlotContext } from "../../types/pluginSlots";
import { connectFrame, pluginFrameUrl } from "./connectFrame";
import type { RpcPort } from "./rpc";

interface Props {
  pluginId: string;
  module: string;
  context: SlotContext;
}

const MAX_HEIGHT = 4000;

export function SandboxedPluginFrame({ pluginId, module, context }: Props) {
  const { settings, updateSetting } = useSettings();
  const { i18n } = useTranslation();
  const language = i18n.language;
  const theme = useContext(ThemeContext);
  const frameRef = useRef<HTMLIFrameElement>(null);
  const rpcRef = useRef<RpcPort | null>(null);
  const guestContext: SlotContext = useMemo(
    () => ({
      ...context,
      pluginSettings: settings.plugins?.[pluginId]?.settings ?? {},
      translations:
        i18n.getResourceBundle(language?.split("-")[0] ?? language, pluginId) ?? {},
      theme: theme
        ? {
            themeId: theme.currentTheme.id,
            themeName: theme.currentTheme.name,
            isDark: !theme.currentTheme.id.includes("-light"),
            colors: theme.currentTheme.colors,
          }
        : null,
    }),
    [context, i18n, language, pluginId, settings.plugins, theme],
  );
  const contextRef = useRef(guestContext);
  const [height, setHeight] = useState(0);

  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    const controller = new AbortController();
    let disposed = false;
    const handlers = {
      "ui.resize": (value: unknown) => {
        const requested = typeof value === "number" ? value : 0;
        setHeight(Math.min(MAX_HEIGHT, Math.max(0, requested)));
      },
      "slot.context": () => structuredClone(contextRef.current),
      "api.query": async (query: unknown) => {
        if (typeof query !== "string" || query.trim().length === 0 || query.length > 100_000) {
          throw new Error("SQL query must contain between 1 and 100,000 characters");
        }
        if (!isReadOnlyQuery(query)) {
          throw new Error("Plugin queries must be read-only");
        }
        const active = contextRef.current;
        if (typeof active.connectionId !== "string" || !active.connectionId) {
          throw new Error("No active connection");
        }
        return invoke<{ columns: string[]; rows: unknown[][] }>("execute_query", {
          connectionId: active.connectionId,
          query,
          ...(typeof active.schema === "string" && active.schema
            ? { schema: active.schema }
            : {}),
        });
      },
      "api.toast": async (kind: unknown, text: unknown) => {
        if (typeof text !== "string") throw new Error("Notification text must be a string");
        const dialogKind = kind === "error" || kind === "warning" ? kind : "info";
        await message(text, { kind: dialogKind });
      },
      "api.setting.set": async (key: unknown, value: unknown) => {
        if (typeof key !== "string" || !key) throw new Error("Setting key is required");
        const serialized = JSON.stringify(value);
        if (typeof serialized !== "string" || serialized.length > 65_536) {
          throw new Error("Plugin settings must be JSON values smaller than 64 KiB");
        }
        const safeValue = JSON.parse(serialized) as unknown;
        await updateSetting("plugins", (previous) => {
          const plugins = previous ?? {};
          const pluginConfig = plugins[pluginId] ?? {};
          return {
            ...plugins,
            [pluginId]: {
              ...pluginConfig,
              settings: { ...(pluginConfig.settings ?? {}), [key]: safeValue },
            },
          };
        });
        return null;
      },
      "api.openUrl": async (value: unknown) => {
        if (typeof value !== "string") throw new Error("URL must be a string");
        const url = new URL(value);
        if (url.protocol !== "https:" && url.protocol !== "http:") {
          throw new Error("Only HTTP and HTTPS URLs can be opened");
        }
        if (url.username || url.password) throw new Error("URLs containing credentials cannot be opened");
        await openExternal(url.toString());
        return null;
      },
      "host.loadFailed": (message: unknown) => {
        console.error(`[PluginSlot] ${pluginId}/${module}: ${String(message)}`);
      },
    };

    void connectFrame(frame, handlers, controller.signal).then(async (rpc) => {
      if (disposed) {
        rpc.close();
        return;
      }
      rpcRef.current = rpc;
      try {
        await rpc.call("slot.context", [contextRef.current]);
      } catch (error) {
        console.error(`[PluginSlot] Failed to initialize ${pluginId}/${module}`, error);
      }
    }).catch((error: unknown) => {
      if (!controller.signal.aborted) {
        console.error(`[PluginSlot] Failed to connect ${pluginId}/${module}`, error);
      }
    });

    return () => {
      disposed = true;
      controller.abort();
      rpcRef.current?.close();
      rpcRef.current = null;
    };
  }, [module, pluginId, updateSetting]);

  useEffect(() => {
    contextRef.current = guestContext;
    void rpcRef.current?.call("slot.context", [guestContext]).catch(() => undefined);
  }, [guestContext]);

  return (
    <iframe
      ref={frameRef}
      title={`Plugin ${pluginId}`}
      sandbox="allow-scripts"
      referrerPolicy="no-referrer"
      src={pluginFrameUrl(pluginId, module)}
      style={{ width: "100%", height, border: 0 }}
    />
  );
}
