import { describe, expect, it } from 'vitest';

import {
  applyRemoteClearDesktopConversation,
  applyRemoteDesktopStop,
  applyRemoteDesktopUserMessage,
  ensureWechatRemoteSession,
  resolveRemoteDesktopSessionId,
  type RemoteSessionBridge,
  syncRemotePollingRoute,
} from '../useRemoteSession';
import type { ChatMessage } from '../../stores/chatStore';
import { WECHAT_REMOTE_SESSION_ID } from '../../lib/wechat-session';

describe('resolveRemoteDesktopSessionId', () => {
  it('uses only the fixed WeChat session stdin route', () => {
    expect(resolveRemoteDesktopSessionId('stdin-wechat')).toBe('stdin-wechat');
    expect(resolveRemoteDesktopSessionId(undefined)).toBeNull();
  });
});

describe('syncRemotePollingRoute', () => {
  it('starts polling with the active desktop stdin route', async () => {
    const calls: string[] = [];
    await syncRemotePollingRoute('stdin-1', fakeBridge(calls));

    expect(calls).toEqual(['start:stdin-1']);
  });

  it('keeps the last desktop route when the fixed window route is temporarily unavailable', async () => {
    const calls: string[] = [];
    await syncRemotePollingRoute(null, fakeBridge(calls));

    expect(calls).toEqual(['start:']);
  });
});

describe('ensureWechatRemoteSession', () => {
  it('reuses an existing fixed WeChat stdin route', async () => {
    const spawnCalls: unknown[] = [];

    const stdinId = await ensureWechatRemoteSession({
      ...fakeBootstrapDeps(spawnCalls),
      getExistingStdinId: () => 'stdin-existing',
      getExistingCwd: () => '/repo',
    });

    expect(stdinId).toBe('stdin-existing');
    expect(spawnCalls).toEqual([]);
  });

  it('pre-warms the fixed WeChat session and publishes the polling route', async () => {
    const spawnCalls: any[] = [];
    const pollingRoutes: string[] = [];
    const ensuredTabs: string[] = [];
    const metas: Array<{ tabId: string; meta: Record<string, unknown> }> = [];
    const touches: Array<{ preview: string; modifiedAt: number }> = [];

    const stdinId = await ensureWechatRemoteSession({
      ...fakeBootstrapDeps(spawnCalls),
      ensureTab: (tabId) => ensuredTabs.push(tabId),
      setSessionMeta: (tabId, meta) => metas.push({ tabId, meta }),
      touchWechatRemoteSession: (preview, modifiedAt) => touches.push({ preview, modifiedAt }),
      startPolling: async (route) => {
        pollingRoutes.push(route);
      },
      now: () => 1234,
    });

    expect(stdinId).toBe('stdin-wechat-auto');
    expect(ensuredTabs).toEqual([WECHAT_REMOTE_SESSION_ID]);
    expect(spawnCalls).toHaveLength(1);
    expect(spawnCalls[0]).toMatchObject({
      tabId: WECHAT_REMOTE_SESSION_ID,
      stdinId: 'stdin-wechat-auto',
      cwdSnapshot: '/repo',
      sessionModeSnapshot: 'bypass',
      sessionParams: {
        prompt: '',
        cwd: '/repo',
        session_id: 'stdin-wechat-auto',
        permission_mode: 'bypassPermissions',
      },
      setRunning: false,
    });
    expect(pollingRoutes).toEqual(['stdin-wechat-auto']);
    expect(touches).toEqual([{ preview: '微信接入', modifiedAt: 1234 }]);
    expect(metas.some((entry) => entry.meta.stdinReady === false)).toBe(true);
  });

  it('does not resume an old CLI session when pre-warming a bound workspace', async () => {
    const spawnCalls: any[] = [];

    await ensureWechatRemoteSession({
      ...fakeBootstrapDeps(spawnCalls),
      getCliResumeId: () => 'old-wechat-cli-session',
    });

    expect(spawnCalls[0].sessionParams.resume_session_id).toBeUndefined();
  });

  it('restarts the fixed WeChat process when the bound workspace changes', async () => {
    const spawnCalls: any[] = [];
    const stopped: string[] = [];

    const stdinId = await ensureWechatRemoteSession({
      ...fakeBootstrapDeps(spawnCalls),
      getWorkingDirectory: () => '/new-repo',
      getExistingStdinId: () => 'stdin-old',
      getExistingCwd: () => '/old-repo',
      stopExistingSession: async (oldStdinId) => {
        stopped.push(oldStdinId);
      },
    });

    expect(stopped).toEqual(['stdin-old']);
    expect(stdinId).toBe('stdin-wechat-auto');
    expect(spawnCalls[0]).toMatchObject({
      tabId: WECHAT_REMOTE_SESSION_ID,
      cwdSnapshot: '/new-repo',
      sessionParams: {
        cwd: '/new-repo',
      },
    });
  });
});

describe('applyRemoteDesktopUserMessage', () => {
  it('appends a WeChat user message to the tab that owns the stdin route', () => {
    const messages: ChatMessage[] = [];
    const statuses: string[] = [];
    const activities: string[] = [];
    const metas: Array<Record<string, unknown>> = [];

    const handled = applyRemoteDesktopUserMessage(
      {
        desktopSessionId: 'stdin-1',
        content: 'hello from WeChat',
        attachments: [
          {
            name: 'photo.jpg',
            path: '/tmp/photo.jpg',
            isImage: true,
          },
        ],
      },
      {
        getTabForStdin: (stdinId) => (stdinId === 'stdin-1' ? 'tab-1' : undefined),
        addMessage: (_tabId, message) => messages.push(message),
        setSessionStatus: (_tabId, status) => statuses.push(status),
        setActivityStatus: (_tabId, status) => activities.push(status.phase),
        setSessionMeta: (_tabId, meta) => metas.push(meta),
        touchWechatRemoteSession: () => {},
        newMessageId: () => 'remote-msg-1',
        now: () => 123,
      },
    );

    expect(handled).toBe(true);
    expect(messages).toEqual([
      {
        id: 'remote-msg-1',
        role: 'user',
        type: 'text',
        content: 'hello from WeChat',
        timestamp: 123,
        attachments: [
          {
            name: 'photo.jpg',
            path: '/tmp/photo.jpg',
            isImage: true,
          },
        ],
      },
    ]);
    expect(statuses).toEqual(['running']);
    expect(activities).toEqual(['thinking']);
    expect(metas).toEqual([
      {
        turnStartTime: 123,
        lastProgressAt: 123,
        apiRetry: undefined,
        inputTokens: 0,
        outputTokens: 0,
      },
    ]);
  });

  it('ignores remote user messages for unknown stdin routes', () => {
    const messages: ChatMessage[] = [];

    const handled = applyRemoteDesktopUserMessage(
      {
        desktopSessionId: 'missing-stdin',
        content: 'hello',
      },
      {
        getTabForStdin: () => undefined,
        addMessage: (_tabId, message) => messages.push(message),
        setSessionStatus: () => {},
        setActivityStatus: () => {},
        setSessionMeta: () => {},
        touchWechatRemoteSession: () => {},
        newMessageId: () => 'remote-msg-1',
        now: () => 123,
      },
    );

    expect(handled).toBe(false);
    expect(messages).toEqual([]);
  });

  it('refreshes the fixed WeChat session preview when the message lands there', () => {
    const touches: Array<{ preview: string; modifiedAt: number }> = [];

    const handled = applyRemoteDesktopUserMessage(
      {
        desktopSessionId: 'stdin-wechat',
        content: '微信发来的图片',
      },
      {
        getTabForStdin: (stdinId) =>
          stdinId === 'stdin-wechat' ? WECHAT_REMOTE_SESSION_ID : undefined,
        addMessage: () => {},
        setSessionStatus: () => {},
        setActivityStatus: () => {},
        setSessionMeta: () => {},
        touchWechatRemoteSession: (preview, modifiedAt) => touches.push({ preview, modifiedAt }),
        newMessageId: () => 'remote-msg-1',
        now: () => 456,
      },
    );

    expect(handled).toBe(true);
    expect(touches).toEqual([{ preview: '微信发来的图片', modifiedAt: 456 }]);
  });
});

describe('applyRemoteClearDesktopConversation', () => {
  it('clears the tab that owns the WeChat stdin route without dropping the route', () => {
    const cleared: string[] = [];
    const resumeCleared: string[] = [];

    const handled = applyRemoteClearDesktopConversation(
      { desktopSessionId: 'stdin-wechat' },
      {
        getTabForStdin: (stdinId) =>
          stdinId === 'stdin-wechat' ? WECHAT_REMOTE_SESSION_ID : undefined,
        ensureTab: () => {},
        clearMessages: (tabId) => cleared.push(tabId),
        clearCliResumeId: (tabId) => resumeCleared.push(tabId),
        clearSessionIdentity: () => {},
        touchWechatRemoteSession: () => {},
      },
    );

    expect(handled).toBe(true);
    expect(cleared).toEqual([WECHAT_REMOTE_SESSION_ID]);
    expect(resumeCleared).toEqual([WECHAT_REMOTE_SESSION_ID]);
  });

  it('falls back to the fixed WeChat tab when clearing a stale stdin route', () => {
    const cleared: string[] = [];
    const resumeCleared: string[] = [];

    const handled = applyRemoteClearDesktopConversation(
      { desktopSessionId: 'missing-stdin' },
      {
        getTabForStdin: () => undefined,
        ensureTab: () => {},
        clearMessages: (tabId) => cleared.push(tabId),
        clearCliResumeId: (tabId) => resumeCleared.push(tabId),
        clearSessionIdentity: () => {},
        touchWechatRemoteSession: () => {},
      },
    );

    expect(handled).toBe(true);
    expect(cleared).toEqual([WECHAT_REMOTE_SESSION_ID]);
    expect(resumeCleared).toEqual([WECHAT_REMOTE_SESSION_ID]);
  });
});

describe('applyRemoteDesktopStop', () => {
  it('records a neutral WeChat stop message and marks the turn as user-stopped', () => {
    const messages: ChatMessage[] = [];
    const statuses: string[] = [];
    const activities: string[] = [];
    const metas: Array<Record<string, unknown>> = [];
    const touches: Array<{ preview: string; modifiedAt: number }> = [];

    const handled = applyRemoteDesktopStop(
      {
        desktopSessionId: 'stdin-wechat',
        source: 'wechat',
      },
      {
        getTabForStdin: (stdinId) =>
          stdinId === 'stdin-wechat' ? WECHAT_REMOTE_SESSION_ID : undefined,
        addMessage: (_tabId, message) => messages.push(message),
        setSessionStatus: (_tabId, status) => statuses.push(status),
        setActivityStatus: (_tabId, status) => activities.push(status.phase),
        setSessionMeta: (_tabId, meta) => metas.push(meta),
        touchWechatRemoteSession: (preview, modifiedAt) => touches.push({ preview, modifiedAt }),
        newMessageId: () => 'remote-stop-1',
        now: () => 789,
      },
    );

    expect(handled).toBe(true);
    expect(messages).toEqual([
      {
        id: 'remote-stop-1',
        role: 'system',
        type: 'text',
        content: '已从微信停止当前任务',
        timestamp: 789,
        commandType: 'action',
      },
    ]);
    expect(statuses).toEqual(['stopping']);
    expect(activities).toEqual(['idle']);
    expect(metas).toEqual([
      {
        teardownReason: 'stop',
        apiRetry: undefined,
        lastProgressAt: 789,
      },
    ]);
    expect(touches).toEqual([{ preview: '已从微信停止当前任务', modifiedAt: 789 }]);
  });

  it('ignores stop events for unknown stdin routes', () => {
    const messages: ChatMessage[] = [];

    const handled = applyRemoteDesktopStop(
      {
        desktopSessionId: 'missing-stdin',
        source: 'wechat',
      },
      {
        getTabForStdin: () => undefined,
        addMessage: (_tabId, message) => messages.push(message),
        setSessionStatus: () => {},
        setActivityStatus: () => {},
        setSessionMeta: () => {},
        touchWechatRemoteSession: () => {},
        newMessageId: () => 'remote-stop-1',
        now: () => 789,
      },
    );

    expect(handled).toBe(false);
    expect(messages).toEqual([]);
  });
});

function fakeBridge(calls: string[]): RemoteSessionBridge {
  return {
    wechatSetDesktopSession: async (sessionId) => {
      calls.push(`set:${sessionId ?? 'null'}`);
    },
    wechatStartPolling: async (sessionId) => {
      calls.push(`start:${sessionId}`);
    },
    wechatStopPolling: async () => {
      calls.push('stop');
    },
  };
}

function fakeBootstrapDeps(spawnCalls: unknown[]) {
  return {
    getWorkingDirectory: () => '/repo',
    getExistingStdinId: () => undefined,
    getExistingCwd: () => undefined,
    getSettings: () => ({
      selectedModel: 'claude-opus-4-6',
      thinkingLevel: 'medium',
      sessionMode: 'bypass',
    }) as any,
    getProviderId: () => 'provider-1',
    getCliResumeId: () => null,
    ensureTab: () => {},
    setSessionMeta: () => {},
    touchWechatRemoteSession: () => {},
    spawn: async (params: any) => {
      spawnCalls.push(params);
      return {
        stdinId: params.stdinId,
        sessionInfo: {
          stdin_id: params.stdinId,
          cli_session_id: 'cli-session-1',
          pid: 1,
          cli_path: 'claude',
        },
        unlisten: () => {},
      };
    },
    startPolling: async () => {},
    stopExistingSession: async () => {},
    makeStdinId: () => 'stdin-wechat-auto',
    onStream: () => {},
    onStderr: () => {},
    now: () => 1,
  };
}
