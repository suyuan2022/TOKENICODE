import { beforeEach, describe, expect, it, vi } from 'vitest';

const WECHAT_REMOTE_SESSION_ID = 'wechat_remote_session';

const mockInvoke = vi.hoisted(() => vi.fn());
const mockReplaceSessionId = vi.hoisted(() => vi.fn());

vi.mock('@tauri-apps/api/core', () => ({
  invoke: mockInvoke,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
  emit: vi.fn(),
}));

vi.mock('../groupStore', () => ({
  useGroupStore: {
    getState: () => ({
      replaceSessionId: mockReplaceSessionId,
    }),
  },
}));

function createStorage() {
  let store: Record<string, string> = {};
  return {
    getItem: (key: string) => store[key] ?? null,
    setItem: (key: string, value: string) => {
      store[key] = value;
    },
    removeItem: (key: string) => {
      delete store[key];
    },
    clear: () => {
      store = {};
    },
  };
}

const storage = createStorage();

Object.defineProperty(globalThis, 'localStorage', { value: storage });
Object.defineProperty(globalThis, 'sessionStorage', { value: storage });

function diskSession(id: string, preview = 'normal') {
  return {
    id,
    path: `/sessions/${id}.jsonl`,
    project: '/project',
    projectDir: '-project',
    modifiedAt: 100,
    preview,
    cliResumeId: id,
  };
}

describe('sessionStore WeChat remote session', () => {
  beforeEach(() => {
    vi.resetModules();
    storage.clear();
    mockInvoke.mockReset();
    mockReplaceSessionId.mockReset();
  });

  it('keeps the fixed WeChat session id when a real CLI id is reported', async () => {
    const { useSessionStore } = await import('../sessionStore');
    const store = useSessionStore.getState();

    store.ensureWechatRemoteSession('/project');
    store.setSelectedSession(WECHAT_REMOTE_SESSION_ID);
    store.registerStdinTab('stdin-wechat', WECHAT_REMOTE_SESSION_ID);
    store.setSessionRunning(WECHAT_REMOTE_SESSION_ID, true);

    store.promoteDraft(WECHAT_REMOTE_SESSION_ID, 'real-cli-session');

    const state = useSessionStore.getState();
    const fixed = state.sessions.find((session) => session.id === WECHAT_REMOTE_SESSION_ID);
    expect(fixed?.cliResumeId).toBe('real-cli-session');
    expect(state.sessions.some((session) => session.id === 'real-cli-session')).toBe(false);
    expect(state.selectedSessionId).toBe(WECHAT_REMOTE_SESSION_ID);
    expect(state.stdinToTab).toEqual({ 'stdin-wechat': WECHAT_REMOTE_SESSION_ID });
    expect(state.runningSessions.has(WECHAT_REMOTE_SESSION_ID)).toBe(true);
    expect(state.runningSessions.has('real-cli-session')).toBe(false);
    expect(sessionStorage.getItem('tokenicode_last_session')).toBe(WECHAT_REMOTE_SESSION_ID);
    expect(mockReplaceSessionId).not.toHaveBeenCalled();
  });

  it('hides the real CLI backing session for the fixed WeChat window', async () => {
    mockInvoke.mockImplementation((command: string) => {
      if (command === 'list_sessions') {
        return Promise.resolve([
          diskSession('real-wechat-session', '微信接入初始化，请只回复：已准备好。'),
          diskSession('normal-session', 'normal task'),
        ]);
      }
      return Promise.resolve(undefined);
    });

    const { useSessionStore } = await import('../sessionStore');
    const store = useSessionStore.getState();

    store.ensureWechatRemoteSession('/project');
    store.setCliResumeId(WECHAT_REMOTE_SESSION_ID, 'real-wechat-session');
    store.setSelectedSession('real-wechat-session');

    await store.fetchSessions();

    const state = useSessionStore.getState();
    expect(state.sessions.map((session) => session.id)).toEqual([
      WECHAT_REMOTE_SESSION_ID,
      'normal-session',
    ]);
    expect(
      state.sessions.find((session) => session.id === WECHAT_REMOTE_SESSION_ID)?.cliResumeId,
    ).toBe('real-wechat-session');
    expect(
      state.sessions.find((session) => session.id === WECHAT_REMOTE_SESSION_ID)?.path,
    ).toBe('/sessions/real-wechat-session.jsonl');
    expect(state.selectedSessionId).toBe(WECHAT_REMOTE_SESSION_ID);
    expect(sessionStorage.getItem('tokenicode_last_session')).toBe(WECHAT_REMOTE_SESSION_ID);
  });

  it('restores the fixed WeChat row from the saved backing CLI session after app restart', async () => {
    localStorage.setItem('tokenicode_wechat_remote_cli_resume_id', 'real-wechat-session');
    mockInvoke.mockImplementation((command: string) => {
      if (command === 'list_sessions') {
        return Promise.resolve([
          diskSession('real-wechat-session', '微信接入初始化，请只回复：已准备好。'),
          diskSession('normal-session', 'normal task'),
        ]);
      }
      return Promise.resolve(undefined);
    });

    const { useSessionStore } = await import('../sessionStore');
    const store = useSessionStore.getState();

    await store.fetchSessions();

    const fixed = useSessionStore.getState().sessions.find(
      (session) => session.id === WECHAT_REMOTE_SESSION_ID,
    );
    expect(fixed).toMatchObject({
      id: WECHAT_REMOTE_SESSION_ID,
      path: '/sessions/real-wechat-session.jsonl',
      project: '/project',
      cliResumeId: 'real-wechat-session',
    });
    expect(useSessionStore.getState().sessions.map((session) => session.id)).toEqual([
      WECHAT_REMOTE_SESSION_ID,
      'normal-session',
    ]);
  });

  it('clears the fixed WeChat backing path when the remote context is cleared', async () => {
    const { useSessionStore } = await import('../sessionStore');
    const store = useSessionStore.getState();

    store.ensureWechatRemoteSession('/project');
    store.setCliResumeId(WECHAT_REMOTE_SESSION_ID, 'real-wechat-session');
    await store.fetchSessions();
    useSessionStore.setState((state) => ({
      sessions: state.sessions.map((session) =>
        session.id === WECHAT_REMOTE_SESSION_ID
          ? { ...session, path: '/sessions/real-wechat-session.jsonl' }
          : session,
      ),
    }));

    store.setCliResumeId(WECHAT_REMOTE_SESSION_ID, null);

    const fixed = useSessionStore.getState().sessions.find(
      (session) => session.id === WECHAT_REMOTE_SESSION_ID,
    );
    expect(fixed?.cliResumeId).toBeNull();
    expect(fixed?.path).toBe('');
    expect(localStorage.getItem('tokenicode_wechat_remote_cli_resume_id')).toBeNull();
  });

  it('hides legacy WeChat initialization sessions that were created before backing ids were tracked', async () => {
    mockInvoke.mockImplementation((command: string) => {
      if (command === 'list_sessions') {
        return Promise.resolve([
          diskSession('legacy-wechat-session', '微信接入初始化，请只回复：已准备好。'),
          diskSession('normal-session', 'normal task'),
        ]);
      }
      return Promise.resolve(undefined);
    });

    const { useSessionStore } = await import('../sessionStore');
    const store = useSessionStore.getState();

    store.ensureWechatRemoteSession('/project');
    await store.fetchSessions();

    expect(useSessionStore.getState().sessions.map((session) => session.id)).toEqual([
      WECHAT_REMOTE_SESSION_ID,
      'normal-session',
    ]);
    expect(localStorage.getItem('tokenicode_wechat_remote_hidden_session_ids')).toContain(
      'legacy-wechat-session',
    );
  });

  it('moves the fixed WeChat session to a newly bound workspace', async () => {
    const { useSessionStore } = await import('../sessionStore');
    const store = useSessionStore.getState();

    store.ensureWechatRemoteSession('/old-project');
    store.ensureWechatRemoteSession('/new-project');

    const state = useSessionStore.getState();
    const fixedSessions = state.sessions.filter((session) => session.id === WECHAT_REMOTE_SESSION_ID);
    expect(fixedSessions).toHaveLength(1);
    expect(fixedSessions[0]).toMatchObject({
      project: '/new-project',
      projectDir: '-new-project',
    });
  });
});
