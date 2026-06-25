import { describe, expect, it } from 'vitest';

import {
  applyRemoteDesktopUserMessage,
  resolveRemoteDesktopSessionId,
  type RemoteSessionBridge,
  syncRemotePollingRoute,
} from '../useRemoteSession';
import type { ChatMessage } from '../../stores/chatStore';

describe('resolveRemoteDesktopSessionId', () => {
  it('uses the active tab stdinId and clears the route when no active process exists', () => {
    expect(resolveRemoteDesktopSessionId('tab-1', 'stdin-1')).toBe('stdin-1');
    expect(resolveRemoteDesktopSessionId('tab-1', undefined)).toBeNull();
    expect(resolveRemoteDesktopSessionId(null, 'stdin-1')).toBeNull();
  });
});

describe('syncRemotePollingRoute', () => {
  it('starts polling with the active desktop stdin route', async () => {
    const calls: string[] = [];
    await syncRemotePollingRoute('stdin-1', fakeBridge(calls));

    expect(calls).toEqual(['start:stdin-1']);
  });

  it('stops polling before clearing the desktop route', async () => {
    const calls: string[] = [];
    await syncRemotePollingRoute(null, fakeBridge(calls));

    expect(calls).toEqual(['stop', 'set:null']);
  });
});

describe('applyRemoteDesktopUserMessage', () => {
  it('appends a WeChat user message to the tab that owns the stdin route', () => {
    const messages: ChatMessage[] = [];

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
        newMessageId: () => 'remote-msg-1',
        now: () => 123,
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
