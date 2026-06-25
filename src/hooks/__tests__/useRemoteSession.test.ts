import { describe, expect, it } from 'vitest';

import {
  resolveRemoteDesktopSessionId,
  type RemoteSessionBridge,
  syncRemotePollingRoute,
} from '../useRemoteSession';

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
