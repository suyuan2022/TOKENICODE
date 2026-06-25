import { describe, expect, it } from 'vitest';

import { resolveRemoteDesktopSessionId } from '../useRemoteSession';

describe('resolveRemoteDesktopSessionId', () => {
  it('uses the active tab stdinId and clears the route when no active process exists', () => {
    expect(resolveRemoteDesktopSessionId('tab-1', 'stdin-1')).toBe('stdin-1');
    expect(resolveRemoteDesktopSessionId('tab-1', undefined)).toBeNull();
    expect(resolveRemoteDesktopSessionId(null, 'stdin-1')).toBeNull();
  });
});
