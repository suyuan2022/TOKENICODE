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
import {
  WECHAT_CONNECTED_EVENT,
  WECHAT_REMOTE_SESSION_ID,
  WECHAT_REMOTE_SESSION_TITLE,
} from '../lib/wechat-session';
import { useSettingsStore, mapSessionModeToPermissionMode } from '../stores/settingsStore';
import { useProviderStore } from '../stores/providerStore';
import { envFingerprint, resolveModelForProvider, spawnConfigHash } from '../lib/api-provider';
import { spawnSession } from '../lib/sessionLifecycle';

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

function forwardClaudeStreamToGlobalHandler(message: any) {
  const handler = (window as any).__claudeStreamHandler;
  if (handler) {
    const queue: any[] | undefined = (window as any).__claudeStreamQueue;
    if (queue?.length) {
      const pending = queue.splice(0);
      for (const queued of pending) handler(queued);
    }
    handler(message);
    return;
  }

  if (!(window as any).__claudeStreamQueue) {
    (window as any).__claudeStreamQueue = [];
  }
  (window as any).__claudeStreamQueue.push(message);
}

type WechatRemoteSpawnSession = typeof spawnSession;

export interface EnsureWechatRemoteSessionDeps {
  getWorkingDirectory: () => string | undefined;
  getExistingStdinId: () => string | undefined;
  getSettings: () => ReturnType<typeof useSettingsStore.getState>;
  getProviderId: () => string;
  getCliResumeId: () => string | null | undefined;
  ensureTab: (tabId: string) => void;
  setSessionMeta: (tabId: string, meta: Partial<SessionMeta>) => void;
  touchWechatRemoteSession: (preview: string, modifiedAt: number) => void;
  spawn: WechatRemoteSpawnSession;
  startPolling: (stdinId: string) => Promise<void>;
  makeStdinId: () => string;
  onStream: (message: any) => void;
  onStderr: (line: string, stdinId: string) => void;
  now: () => number;
}

export async function ensureWechatRemoteSession(
  deps: EnsureWechatRemoteSessionDeps = {
    getWorkingDirectory: () => useSettingsStore.getState().workingDirectory,
    getExistingStdinId: () =>
      useChatStore.getState().tabs.get(WECHAT_REMOTE_SESSION_ID)?.sessionMeta.stdinId,
    getSettings: () => useSettingsStore.getState(),
    getProviderId: () => useProviderStore.getState().activeProviderId || '',
    getCliResumeId: () =>
      useSessionStore.getState().sessions.find((session) => session.id === WECHAT_REMOTE_SESSION_ID)
        ?.cliResumeId,
    ensureTab: (tabId) => useChatStore.getState().ensureTab(tabId),
    setSessionMeta: (tabId, meta) => useChatStore.getState().setSessionMeta(tabId, meta),
    touchWechatRemoteSession: (preview, modifiedAt) =>
      useSessionStore.getState().touchWechatRemoteSession(preview, modifiedAt),
    spawn: spawnSession,
    startPolling: (stdinId) => bridge.wechatStartPolling(stdinId),
    makeStdinId: () => `desk_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
    onStream: forwardClaudeStreamToGlobalHandler,
    onStderr: (line) => console.warn('[WeChat] remote session stderr:', line),
    now: Date.now,
  },
): Promise<string | null> {
  const existingStdinId = deps.getExistingStdinId();
  if (existingStdinId) return existingStdinId;

  const cwd = deps.getWorkingDirectory()?.trim();
  if (!cwd) return null;

  const settings = deps.getSettings();
  const providerId = deps.getProviderId();
  const model = resolveModelForProvider(settings.selectedModel);
  const permissionMode = mapSessionModeToPermissionMode(settings.sessionMode);
  const stdinId = deps.makeStdinId();
  const preEnvFingerprint = envFingerprint();
  const preSpawnConfigHash = spawnConfigHash();

  deps.ensureTab(WECHAT_REMOTE_SESSION_ID);
  deps.setSessionMeta(WECHAT_REMOTE_SESSION_ID, {
    stdinReady: false,
    pendingReadyMessage: undefined,
  });

  const spawnResult = await deps.spawn({
    tabId: WECHAT_REMOTE_SESSION_ID,
    stdinId,
    cwdSnapshot: cwd,
    configSnapshot: {
      model,
      providerId,
      thinkingLevel: settings.thinkingLevel,
      permissionMode,
    },
    sessionModeSnapshot: settings.sessionMode,
    sessionParams: {
      prompt: '',
      cwd,
      model,
      session_id: stdinId,
      resume_session_id: deps.getCliResumeId() || undefined,
      thinking_level: settings.thinkingLevel,
      session_mode: (settings.sessionMode === 'ask' || settings.sessionMode === 'plan')
        ? settings.sessionMode
        : undefined,
      provider_id: providerId || undefined,
      permission_mode: permissionMode,
    },
    onStream: deps.onStream,
    onStderr: (line: string) => deps.onStderr(line, stdinId),
    setRunning: false,
  });

  deps.setSessionMeta(WECHAT_REMOTE_SESSION_ID, {
    sessionId: spawnResult.sessionInfo.cli_session_id ?? undefined,
    envFingerprint: preEnvFingerprint,
    spawnedModel: model,
    stdinReady: false,
    pendingReadyMessage: undefined,
    spawnConfigHash: preSpawnConfigHash,
  });
  deps.touchWechatRemoteSession(WECHAT_REMOTE_SESSION_TITLE, deps.now());
  await deps.startPolling(spawnResult.sessionInfo.stdin_id);

  return spawnResult.sessionInfo.stdin_id;
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
  ensureTab: (tabId: string) => void;
  clearMessages: (tabId: string) => void;
  clearCliResumeId: (tabId: string) => void;
  clearSessionIdentity: (tabId: string) => void;
  touchWechatRemoteSession: (preview: string, modifiedAt: number) => void;
}

export function applyRemoteClearDesktopConversation(
  message: WechatDesktopClearConversationEvent,
  deps: RemoteClearDesktopConversationDeps = {
    getTabForStdin: (stdinId) => useSessionStore.getState().getTabForStdin(stdinId),
    ensureTab: (tabId) => useChatStore.getState().ensureTab(tabId),
    clearMessages: (tabId) => useChatStore.getState().clearMessages(tabId),
    clearCliResumeId: (tabId) => useSessionStore.getState().setCliResumeId(tabId, null),
    clearSessionIdentity: (tabId) => useChatStore.getState().setSessionMeta(tabId, {
      sessionId: undefined,
      stdinReady: false,
      pendingReadyMessage: undefined,
      turnAcceptedForResume: undefined,
      interruptedAssistantText: undefined,
    }),
    touchWechatRemoteSession: (preview, modifiedAt) =>
      useSessionStore.getState().touchWechatRemoteSession(preview, modifiedAt),
  },
): boolean {
  const tabId = deps.getTabForStdin(message.desktopSessionId) ?? WECHAT_REMOTE_SESSION_ID;

  deps.ensureTab(tabId);
  deps.clearMessages(tabId);
  deps.clearCliResumeId(tabId);
  deps.clearSessionIdentity(tabId);
  if (tabId === WECHAT_REMOTE_SESSION_ID) {
    deps.touchWechatRemoteSession('微信接入', Date.now());
  }
  return true;
}

export function useRemoteSession() {
  const activeStdinId = useChatStore((state) =>
    state.tabs.get(WECHAT_REMOTE_SESSION_ID)?.sessionMeta.stdinId,
  );
  const workingDirectory = useSettingsStore((state) => state.workingDirectory);
  const route = resolveRemoteDesktopSessionId(activeStdinId);
  const publishedRouteRef = useRef<string | null | undefined>(undefined);
  const bootstrapRef = useRef<Promise<string | null> | null>(null);

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
    if (activeStdinId || !workingDirectory) return;
    let disposed = false;

    const ensureIfConnected = () => {
      if (bootstrapRef.current) return;
      bridge.wechatGetStatus()
        .then((status) => {
          if (disposed || !status.connected) return;
          if (useChatStore.getState().tabs.get(WECHAT_REMOTE_SESSION_ID)?.sessionMeta.stdinId) {
            return;
          }
          bootstrapRef.current = ensureWechatRemoteSession()
            .catch((err) => {
              console.warn('[WeChat] failed to prepare fixed remote session', err);
              return null;
            })
            .finally(() => {
              bootstrapRef.current = null;
            });
        })
        .catch((err) => {
          console.warn('[WeChat] failed to check connected status before remote bootstrap', err);
        });
    };

    ensureIfConnected();
    window.addEventListener(WECHAT_CONNECTED_EVENT, ensureIfConnected);
    return () => {
      disposed = true;
      window.removeEventListener(WECHAT_CONNECTED_EVENT, ensureIfConnected);
    };
  }, [activeStdinId, workingDirectory]);

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
