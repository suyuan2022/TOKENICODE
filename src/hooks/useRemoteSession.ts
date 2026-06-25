import { useEffect, useRef } from 'react';
import {
  bridge,
  onWechatDesktopUserMessage,
  type WechatDesktopUserMessageEvent,
} from '../lib/tauri-bridge';
import { generateMessageId, useChatStore, type ChatMessage } from '../stores/chatStore';
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

export interface RemoteDesktopUserMessageDeps {
  getTabForStdin: (stdinId: string) => string | undefined;
  addMessage: (tabId: string, message: ChatMessage) => void;
  newMessageId: () => string;
  now: () => number;
}

export function applyRemoteDesktopUserMessage(
  message: WechatDesktopUserMessageEvent,
  deps: RemoteDesktopUserMessageDeps = {
    getTabForStdin: (stdinId) => useSessionStore.getState().getTabForStdin(stdinId),
    addMessage: (tabId, chatMessage) => useChatStore.getState().addMessage(tabId, chatMessage),
    newMessageId: generateMessageId,
    now: Date.now,
  },
): boolean {
  const tabId = deps.getTabForStdin(message.desktopSessionId);
  if (!tabId) return false;

  deps.addMessage(tabId, {
    id: deps.newMessageId(),
    role: 'user',
    type: 'text',
    content: message.content,
    timestamp: deps.now(),
    attachments: message.attachments?.length ? message.attachments : undefined,
  });
  return true;
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

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    onWechatDesktopUserMessage((message) => {
      if (!applyRemoteDesktopUserMessage(message)) {
        console.warn('[WeChat] received remote user message for unknown desktop route', {
          desktopSessionId: message.desktopSessionId,
        });
      }
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    }).catch((err) => {
      console.warn('[WeChat] failed to subscribe to remote desktop messages', err);
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
}
