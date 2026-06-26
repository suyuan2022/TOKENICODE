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
