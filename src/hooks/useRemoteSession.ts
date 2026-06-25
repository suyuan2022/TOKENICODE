import { useEffect, useRef } from 'react';
import { bridge } from '../lib/tauri-bridge';
import { useChatStore } from '../stores/chatStore';
import { useSessionStore } from '../stores/sessionStore';

export function resolveRemoteDesktopSessionId(
  selectedSessionId: string | null,
  stdinId: string | undefined,
): string | null {
  return selectedSessionId && stdinId ? stdinId : null;
}

export function useRemoteSession() {
  const selectedSessionId = useSessionStore((state) => state.selectedSessionId);
  const activeStdinId = useChatStore((state) =>
    selectedSessionId ? state.tabs.get(selectedSessionId)?.sessionMeta.stdinId : undefined,
  );
  const route = resolveRemoteDesktopSessionId(selectedSessionId, activeStdinId);
  const publishedRouteRef = useRef<string | null | undefined>(undefined);

  useEffect(() => {
    if (publishedRouteRef.current === route) return;
    publishedRouteRef.current = route;

    bridge.wechatSetDesktopSession(route).catch((err) => {
      console.warn('[WeChat] failed to sync desktop session route', err);
    });
  }, [route]);
}
