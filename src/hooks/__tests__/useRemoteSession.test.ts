import { describe, expect, it } from 'vitest';

import {
  applyRemoteClearDesktopConversation,
  applyRemoteDesktopUserMessage,
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

    const handled = applyRemoteClearDesktopConversation(
      { desktopSessionId: 'stdin-wechat' },
      {
        getTabForStdin: (stdinId) =>
          stdinId === 'stdin-wechat' ? WECHAT_REMOTE_SESSION_ID : undefined,
        clearMessages: (tabId) => cleared.push(tabId),
        touchWechatRemoteSession: () => {},
      },
    );

    expect(handled).toBe(true);
    expect(cleared).toEqual([WECHAT_REMOTE_SESSION_ID]);
  });

  it('ignores clear events for unknown stdin routes', () => {
    const cleared: string[] = [];

    const handled = applyRemoteClearDesktopConversation(
      { desktopSessionId: 'missing-stdin' },
      {
        getTabForStdin: () => undefined,
        clearMessages: (tabId) => cleared.push(tabId),
        touchWechatRemoteSession: () => {},
      },
    );

    expect(handled).toBe(false);
    expect(cleared).toEqual([]);
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
