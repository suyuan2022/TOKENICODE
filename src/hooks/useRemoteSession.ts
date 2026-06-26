import { useEffect, useRef } from 'react';
import {
  bridge,
  onWechatDesktopClearConversation,
  onWechatDesktopUserMessage,
  type WechatDesktopClearConversationEvent,
  type WechatDesktopUserMessageEvent,
} from '../lib/tauri-bridge';
import {
  generateMessageId,
  useChatStore,
  type ActivityStatus,
  type ChatMessage,
  type SessionMeta,
  type SessionStatus,
} from '../stores/chatStore';
import { useSessionStore } from '../stores/sessionStore';
import { WECHAT_REMOTE_SESSION_ID } from '../lib/wechat-session';

export type RemoteSessionBridge = Pick<
  typeof bridge,
  'wechatSetDesktopSession' | 'wechatStartPolling' | 'wechatStopPolling'
>;

export function resolveRemoteDesktopSessionId(
  wechatSessionStdinId: string | undefined,
): string | null {
  return wechatSessionStdinId || null;
}

export async function syncRemotePollingRoute(
  route: string | null,
  remoteBridge: RemoteSessionBridge = bridge,
) {
  if (!route) {
    await remoteBridge.wechatStartPolling('');
    return;
  }

  await remoteBridge.wechatStartPolling(route);
}

export interface RemoteDesktopUserMessageDeps {
  getTabForStdin: (stdinId: string) => string | undefined;
  addMessage: (tabId: string, message: ChatMessage) => void;
  setSessionStatus: (tabId: string, status: SessionStatus) => void;
  setActivityStatus: (tabId: string, status: ActivityStatus) => void;
  setSessionMeta: (tabId: string, meta: Partial<SessionMeta>) => void;
  touchWechatRemoteSession: (preview: string, modifiedAt: number) => void;
  newMessageId: () => string;
  now: () => number;
}

export function applyRemoteDesktopUserMessage(
  message: WechatDesktopUserMessageEvent,
  deps: RemoteDesktopUserMessageDeps = {
    getTabForStdin: (stdinId) => useSessionStore.getState().getTabForStdin(stdinId),
    addMessage: (tabId, chatMessage) => useChatStore.getState().addMessage(tabId, chatMessage),
    setSessionStatus: (tabId, status) => useChatStore.getState().setSessionStatus(tabId, status),
    setActivityStatus: (tabId, status) => useChatStore.getState().setActivityStatus(tabId, status),
    setSessionMeta: (tabId, meta) => useChatStore.getState().setSessionMeta(tabId, meta),
    touchWechatRemoteSession: (preview, modifiedAt) =>
      useSessionStore.getState().touchWechatRemoteSession(preview, modifiedAt),
    newMessageId: generateMessageId,
    now: Date.now,
  },
): boolean {
  const tabId = deps.getTabForStdin(message.desktopSessionId);
  if (!tabId) return false;
  const timestamp = deps.now();

  deps.addMessage(tabId, {
    id: deps.newMessageId(),
    role: 'user',
    type: 'text',
    content: message.content,
    timestamp,
    attachments: message.attachments?.length ? message.attachments : undefined,
  });
  deps.setSessionStatus(tabId, 'running');
  deps.setActivityStatus(tabId, { phase: 'thinking' });
  deps.setSessionMeta(tabId, {
    turnStartTime: timestamp,
    lastProgressAt: timestamp,
    apiRetry: undefined,
    inputTokens: 0,
    outputTokens: 0,
  });
  if (tabId === WECHAT_REMOTE_SESSION_ID) {
    deps.touchWechatRemoteSession(message.content, timestamp);
  }
  return true;
}

export interface RemoteClearDesktopConversationDeps {
  getTabForStdin: (stdinId: string) => string | undefined;
  clearMessages: (tabId: string) => void;
  touchWechatRemoteSession: (preview: string, modifiedAt: number) => void;
}

export function applyRemoteClearDesktopConversation(
  message: WechatDesktopClearConversationEvent,
  deps: RemoteClearDesktopConversationDeps = {
    getTabForStdin: (stdinId) => useSessionStore.getState().getTabForStdin(stdinId),
    clearMessages: (tabId) => useChatStore.getState().clearMessages(tabId),
    touchWechatRemoteSession: (preview, modifiedAt) =>
      useSessionStore.getState().touchWechatRemoteSession(preview, modifiedAt),
  },
): boolean {
  const tabId = deps.getTabForStdin(message.desktopSessionId);
  if (!tabId) return false;

  deps.clearMessages(tabId);
  if (tabId === WECHAT_REMOTE_SESSION_ID) {
    deps.touchWechatRemoteSession('微信接入', Date.now());
  }
  return true;
}

export function useRemoteSession() {
  const activeStdinId = useChatStore((state) =>
    state.tabs.get(WECHAT_REMOTE_SESSION_ID)?.sessionMeta.stdinId,
  );
  const route = resolveRemoteDesktopSessionId(activeStdinId);
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
    let unlistenUserMessage: (() => void) | undefined;
    let unlistenClearConversation: (() => void) | undefined;

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
        unlistenUserMessage = fn;
      }
    }).catch((err) => {
      console.warn('[WeChat] failed to subscribe to remote desktop messages', err);
    });

    onWechatDesktopClearConversation((message) => {
      if (!applyRemoteClearDesktopConversation(message)) {
        console.warn('[WeChat] received clear event for unknown desktop route', {
          desktopSessionId: message.desktopSessionId,
        });
      }
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlistenClearConversation = fn;
      }
    }).catch((err) => {
      console.warn('[WeChat] failed to subscribe to remote clear events', err);
    });

    return () => {
      disposed = true;
      unlistenUserMessage?.();
      unlistenClearConversation?.();
    };
  }, []);
}
