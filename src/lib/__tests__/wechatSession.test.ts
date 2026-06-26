import { describe, expect, it } from 'vitest';
import {
  WECHAT_REMOTE_SESSION_ID,
  WECHAT_REMOTE_SESSION_TITLE,
  createWechatRemoteSession,
  upsertWechatRemoteSession,
} from '../wechat-session';
import type { SessionListItem } from '../tauri-bridge';

function session(id: string, modifiedAt = 1): SessionListItem {
  return {
    id,
    path: `/tmp/${id}.jsonl`,
    project: '/repo',
    projectDir: '-repo',
    modifiedAt,
    preview: id,
    cliResumeId: null,
  };
}

describe('WeChat fixed remote session', () => {
  it('creates a stable draft session for the WeChat remote window', () => {
    expect(createWechatRemoteSession('/repo', 123)).toEqual({
      id: WECHAT_REMOTE_SESSION_ID,
      path: '',
      project: '/repo',
      projectDir: '-repo',
      modifiedAt: 123,
      preview: WECHAT_REMOTE_SESSION_TITLE,
      cliResumeId: null,
    });
  });

  it('keeps the fixed WeChat session at the top without duplicating it', () => {
    const sessions = upsertWechatRemoteSession([session('normal')], '/repo', 100);
    const next = upsertWechatRemoteSession(sessions, '/repo', 200);

    expect(next.map((item) => item.id)).toEqual([WECHAT_REMOTE_SESSION_ID, 'normal']);
    expect(next.filter((item) => item.id === WECHAT_REMOTE_SESSION_ID)).toHaveLength(1);
    expect(next[0].modifiedAt).toBe(200);
  });
});
