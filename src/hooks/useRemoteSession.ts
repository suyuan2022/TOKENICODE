import { useEffect, useRef } from 'react';
import { bridge } from '../lib/tauri-bridge';
import { useChatStore } from '../stores/chatStore';
import { useSessionStore } from '../stores/sessionStore';

export type RemoteSessionBridge = Pick<
  typeof bridge,
  'wechatSetDesktopSession' | 'wechatStartPolling' | 'wechatStopPolling'
>;

export function resolveRemoteDesktopSessionId(
  selectedSessionId: string | null,
  stdinId: string | undefined,
): string | null {
  return selectedSessionId && stdinId ? stdinId : null;
}

export async function syncRemotePollingRoute(
  route: string | null,
  remoteBridge: RemoteSessionBridge = bridge,
) {
  if (!route) {
    await remoteBridge.wechatStopPolling();
    await remoteBridge.wechatSetDesktopSession(null);
    return;
  }

  await remoteBridge.wechatStartPolling(route);
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

    syncRemotePollingRoute(route).catch((err) => {
      console.warn('[WeChat] failed to sync remote polling route', err);
    });

    return () => {
      if (route) {
        bridge.wechatStopPolling().catch((err) => {
          console.warn('[WeChat] failed to stop polling', err);
        });
      }
    };
  }, [route]);
}
