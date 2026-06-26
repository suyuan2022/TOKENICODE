import { describe, expect, it } from 'vitest';

import {
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

  it('clears the desktop route but keeps polling for WeChat commands', async () => {
    const calls: string[] = [];
    await syncRemotePollingRoute(null, fakeBridge(calls));

    expect(calls).toEqual(['set:null', 'start:']);
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
        touchWechatRemoteSession: (preview, modifiedAt) => touches.push({ preview, modifiedAt }),
        newMessageId: () => 'remote-msg-1',
        now: () => 456,
      },
    );

    expect(handled).toBe(true);
    expect(touches).toEqual([{ preview: '微信发来的图片', modifiedAt: 456 }]);
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
