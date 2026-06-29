import type { SessionListItem } from './tauri-bridge';

export const WECHAT_REMOTE_SESSION_ID = 'wechat_remote_session';
export const WECHAT_REMOTE_SESSION_TITLE = '微信接入';
export const WECHAT_CONNECTED_EVENT = 'tokenicode:wechat-connected';

export function isWechatRemoteSessionId(id: string | null | undefined): boolean {
  return id === WECHAT_REMOTE_SESSION_ID;
}

export function createWechatRemoteSession(
  projectPath: string,
  modifiedAt: number = Date.now(),
): SessionListItem {
  return {
    id: WECHAT_REMOTE_SESSION_ID,
    path: '',
    project: projectPath,
    projectDir: projectPath.replace(/\//g, '-'),
    modifiedAt,
    preview: WECHAT_REMOTE_SESSION_TITLE,
    cliResumeId: null,
  };
}

export function resolveWechatRemoteWorkspace(
  boundWorkspacePath: string | null | undefined,
  activeWorkspacePath: string | null | undefined,
  fallbackWorkspacePath: string | null | undefined = '',
): string {
  return boundWorkspacePath?.trim()
    || activeWorkspacePath?.trim()
    || fallbackWorkspacePath?.trim()
    || '';
}

export function upsertWechatRemoteSession(
  sessions: SessionListItem[],
  projectPath: string,
  modifiedAt: number = Date.now(),
): SessionListItem[] {
  const existing = sessions.find((session) => isWechatRemoteSessionId(session.id));
  const remote = existing
    ? {
      ...existing,
      project: projectPath || existing.project,
      projectDir: (projectPath || existing.projectDir).replace(/\//g, '-'),
      preview: WECHAT_REMOTE_SESSION_TITLE,
      modifiedAt: Math.max(existing.modifiedAt || 0, modifiedAt),
    }
    : createWechatRemoteSession(projectPath, modifiedAt);

  return [
    remote,
    ...sessions.filter((session) => !isWechatRemoteSessionId(session.id)),
  ];
}

// --- CLI resume ID + hidden session persistence (localStorage) ---

const WECHAT_REMOTE_CLI_RESUME_KEY = 'tokenicode_wechat_remote_cli_resume_id';
const WECHAT_REMOTE_HIDDEN_SESSIONS_KEY = 'tokenicode_wechat_remote_hidden_session_ids';

export function loadWechatRemoteCliResumeId(): string | null {
  return localStorage.getItem(WECHAT_REMOTE_CLI_RESUME_KEY);
}

export function loadWechatRemoteHiddenSessionIds(): Set<string> {
  try {
    const raw = JSON.parse(localStorage.getItem(WECHAT_REMOTE_HIDDEN_SESSIONS_KEY) || '[]');
    return new Set(Array.isArray(raw) ? raw.filter((value: unknown) => typeof value === 'string') : []);
  } catch {
    return new Set();
  }
}

export function rememberWechatRemoteCliSessionId(id: string | null | undefined) {
  if (!id || id.startsWith('desk_')) return;
  const hidden = loadWechatRemoteHiddenSessionIds();
  hidden.add(id);
  localStorage.setItem(WECHAT_REMOTE_HIDDEN_SESSIONS_KEY, JSON.stringify([...hidden]));
}

export function saveWechatRemoteCliResumeId(id: string | null) {
  if (id) {
    localStorage.setItem(WECHAT_REMOTE_CLI_RESUME_KEY, id);
    rememberWechatRemoteCliSessionId(id);
  } else {
    localStorage.removeItem(WECHAT_REMOTE_CLI_RESUME_KEY);
  }
}

export function getWechatRemoteCliResumeId(sessions: SessionListItem[]): string | null {
  return sessions.find((session) => isWechatRemoteSessionId(session.id))?.cliResumeId
    ?? loadWechatRemoteCliResumeId();
}

export function restoreWechatRemoteCliResumeId(
  sessions: SessionListItem[],
  cliResumeId: string | null,
): SessionListItem[] {
  if (!cliResumeId) return sessions;
  return sessions.map((session) =>
    isWechatRemoteSessionId(session.id) && !session.cliResumeId
      ? { ...session, cliResumeId }
      : session,
  );
}

export function materializeWechatRemoteSession(
  sessions: SessionListItem[],
  backingSession: SessionListItem | undefined,
  cliResumeId: string | null,
): SessionListItem[] {
  if (!backingSession || !cliResumeId) {
    return restoreWechatRemoteCliResumeId(sessions, cliResumeId);
  }

  const sessionsWithRemote = sessions.some((session) => isWechatRemoteSessionId(session.id))
    ? sessions
    : upsertWechatRemoteSession(sessions, backingSession.project, backingSession.modifiedAt);

  return sessionsWithRemote.map((session) => {
    if (!isWechatRemoteSessionId(session.id)) return session;
    return {
      ...session,
      path: backingSession.path,
      project: session.project || backingSession.project,
      projectDir: session.projectDir || backingSession.projectDir,
      modifiedAt: Math.max(session.modifiedAt || 0, backingSession.modifiedAt || 0),
      cliResumeId,
    };
  });
}

export function isLegacyWechatRemoteBootstrapSession(session: SessionListItem): boolean {
  const preview = session.preview.trim();
  return preview.startsWith('微信接入初始化，请只回复')
    || preview.startsWith('微信接入专用会话初始化');
}
