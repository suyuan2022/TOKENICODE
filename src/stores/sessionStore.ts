import { create } from 'zustand';
import { bridge, SessionListItem, ContentSearchResult } from '../lib/tauri-bridge';
import { useGroupStore } from './groupStore';
import {
  WECHAT_REMOTE_SESSION_ID,
  isWechatRemoteSessionId,
  upsertWechatRemoteSession,
} from '../lib/wechat-session';

// --- Orphan drain callback ---
// useStreamProcessor exports drainOrphanBuffer(), but sessionStore can't import
// it directly (circular dependency). Instead, useStreamProcessor registers its
// drain function at module init via setOrphanDrainCallback(). registerStdinTab
// then calls it so text that arrived before the mapping existed is flushed.
let _orphanDrainCallback: ((stdinId: string, tabId: string) => void) | null = null;
export function setOrphanDrainCallback(cb: (stdinId: string, tabId: string) => void) {
  _orphanDrainCallback = cb;
}

// Persist custom session names in localStorage as fast cache,
// and sync to disk via Tauri backend for durability.
const CUSTOM_PREVIEWS_KEY = 'tokenicode_custom_previews';
const LAST_SESSION_KEY = 'tokenicode_last_session';
const STDIN_TO_TAB_KEY = 'tokenicode_stdinToTab';
const WECHAT_REMOTE_CLI_RESUME_KEY = 'tokenicode_wechat_remote_cli_resume_id';
const WECHAT_REMOTE_HIDDEN_SESSIONS_KEY = 'tokenicode_wechat_remote_hidden_session_ids';

function loadCustomPreviewsSync(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(CUSTOM_PREVIEWS_KEY) || '{}');
  } catch {
    return {};
  }
}

function saveCustomPreviewsLocal(map: Record<string, string>) {
  localStorage.setItem(CUSTOM_PREVIEWS_KEY, JSON.stringify(map));
}

/** Persist the last active session ID so app restart can auto-restore */
function saveLastSessionId(id: string | null) {
  if (id && !id.startsWith('draft_')) {
    sessionStorage.setItem(LAST_SESSION_KEY, id);
  }
}

function loadLastSessionId(): string | null {
  return sessionStorage.getItem(LAST_SESSION_KEY);
}

/** Persist stdinToTab across page refreshes using sessionStorage.
 *  sessionStorage survives same-window refreshes but clears on app restart,
 *  which is exactly the right scope for process-lifetime mappings. */
function loadStdinToTabSync(): Record<string, string> {
  try {
    return JSON.parse(sessionStorage.getItem(STDIN_TO_TAB_KEY) || '{}');
  } catch {
    return {};
  }
}

function saveStdinToTab(map: Record<string, string>) {
  sessionStorage.setItem(STDIN_TO_TAB_KEY, JSON.stringify(map));
}

function loadWechatRemoteCliResumeId(): string | null {
  return localStorage.getItem(WECHAT_REMOTE_CLI_RESUME_KEY);
}

function loadWechatRemoteHiddenSessionIds(): Set<string> {
  try {
    const raw = JSON.parse(localStorage.getItem(WECHAT_REMOTE_HIDDEN_SESSIONS_KEY) || '[]');
    return new Set(Array.isArray(raw) ? raw.filter((value) => typeof value === 'string') : []);
  } catch {
    return new Set();
  }
}

function rememberWechatRemoteCliSessionId(id: string | null | undefined) {
  if (!id || id.startsWith('desk_')) return;
  const hidden = loadWechatRemoteHiddenSessionIds();
  hidden.add(id);
  localStorage.setItem(WECHAT_REMOTE_HIDDEN_SESSIONS_KEY, JSON.stringify([...hidden]));
}

function saveWechatRemoteCliResumeId(id: string | null) {
  if (id) {
    localStorage.setItem(WECHAT_REMOTE_CLI_RESUME_KEY, id);
    rememberWechatRemoteCliSessionId(id);
  } else {
    localStorage.removeItem(WECHAT_REMOTE_CLI_RESUME_KEY);
  }
}

function getWechatRemoteCliResumeId(sessions: SessionListItem[]): string | null {
  return sessions.find((session) => isWechatRemoteSessionId(session.id))?.cliResumeId
    ?? loadWechatRemoteCliResumeId();
}

function restoreWechatRemoteCliResumeId(
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

function materializeWechatRemoteSession(
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

function isLegacyWechatRemoteBootstrapSession(session: SessionListItem): boolean {
  const preview = session.preview.trim();
  return preview.startsWith('微信接入初始化，请只回复')
    || preview.startsWith('微信接入专用会话初始化');
}

interface SessionState {
  sessions: SessionListItem[];
  isLoading: boolean;
  searchQuery: string;
  selectedSessionId: string | null;
  /** Previously selected session ID, for Ctrl+Tab quick switch */
  previousSessionId: string | null;
  /** Custom display names keyed by session ID, persisted to disk */
  customPreviews: Record<string, string>;
  /** Track which sessions are actively running (streaming/working) */
  runningSessions: Set<string>;
  /** Map stdinId → tabId so stream events can be routed to the correct session */
  stdinToTab: Record<string, string>;
  /** Content search results keyed by session ID */
  contentSearchResults: Map<string, ContentSearchResult>;
  isContentSearching: boolean;
  contentSearchQuery: string;

  fetchSessions: () => Promise<void>;
  setSearchQuery: (query: string) => void;
  setSelectedSession: (id: string | null) => void;
  /** Insert a temporary "draft" session at the top of the list */
  addDraftSession: (id: string, projectPath: string) => void;
  /** Ensure the fixed WeChat remote session is present in the local session list. */
  ensureWechatRemoteSession: (projectPath: string) => void;
  /** Update the fixed WeChat remote session preview/time after remote activity. */
  touchWechatRemoteSession: (preview: string, modifiedAt: number) => void;
  /** Update an existing draft session's project path (e.g. after folder selection) */
  updateDraftProject: (id: string, projectPath: string) => void;
  /** Set a custom display name for a session */
  setCustomPreview: (sessionId: string, name: string) => void;
  /** Get the display name for a session (custom > preview > fallback) */
  getDisplayName: (session: SessionListItem) => string;
  /** Mark a session as running (actively streaming/working) */
  setSessionRunning: (sessionId: string, running: boolean) => void;
  /** Check if a session is currently running */
  isSessionRunning: (sessionId: string) => boolean;
  /** Register a stdinId → tabId mapping (persisted to sessionStorage) */
  registerStdinTab: (stdinId: string, tabId: string) => void;
  /** Remove a stdinId mapping on process exit (cleans sessionStorage too) */
  unregisterStdinTab: (stdinId: string) => void;
  /** Look up which tabId owns a given stdinId */
  getTabForStdin: (stdinId: string) => string | undefined;
  /** Remove a draft session from the local list (no disk deletion needed) */
  removeDraft: (draftId: string) => void;
  /** Promote a draft session to a real session ID (when CLI returns the actual UUID).
   *  Updates session id, selectedSessionId, stdinToTab mapping, and runningSessions. */
  promoteDraft: (oldDraftId: string, newRealId: string) => void;
  /** Switch to the previously selected session (Ctrl+Tab) */
  switchToPrevious: () => void;
  /** Load custom previews from backend (called once on init) */
  loadCustomPreviewsFromDisk: () => Promise<void>;
  /** Get the last active session ID from localStorage (for app restart recovery) */
  getLastSessionId: () => string | null;
  /** Set the CLI's session UUID for --resume on a given session */
  setCliResumeId: (sessionId: string, cliResumeId: string | null) => void;
  /** Search session content via backend */
  searchSessionContent: (query: string) => Promise<void>;
  /** Clear content search results */
  clearContentSearch: () => void;
}

export const useSessionStore = create<SessionState>()((set, get) => ({
  sessions: [],
  isLoading: false,
  searchQuery: '',
  selectedSessionId: null,
  previousSessionId: null,
  customPreviews: loadCustomPreviewsSync(),
  runningSessions: new Set<string>(),
  stdinToTab: loadStdinToTabSync(),
  contentSearchResults: new Map<string, ContentSearchResult>(),
  isContentSearching: false,
  contentSearchQuery: '',

  fetchSessions: async () => {
    const isFirstLoad = get().sessions.length === 0;
    if (isFirstLoad) set({ isLoading: true });
    try {
      const diskSessions = await bridge.listSessions();
      const existing = get().sessions;
      const wechatRemoteCliResumeId = getWechatRemoteCliResumeId(existing);
      const wechatRemoteBackingSession = wechatRemoteCliResumeId
        ? diskSessions.find((session) => session.id === wechatRemoteCliResumeId)
        : undefined;
      const hiddenWechatSessionIds = loadWechatRemoteHiddenSessionIds();
      if (wechatRemoteCliResumeId) hiddenWechatSessionIds.add(wechatRemoteCliResumeId);
      const visibleDiskSessions = diskSessions.filter((session) => {
        if (isLegacyWechatRemoteBootstrapSession(session)) {
          rememberWechatRemoteCliSessionId(session.id);
          hiddenWechatSessionIds.add(session.id);
          return false;
        }
        return !hiddenWechatSessionIds.has(session.id);
      });
      // Preserve draft sessions (path === '') that haven't been written to disk yet
      const drafts = existing.filter(
        (s) => s.path === '' && !visibleDiskSessions.some((d) => d.id === s.id),
      );
      // Merge: preserve in-memory cliResumeId on disk sessions
      const merged = visibleDiskSessions.map((d) => {
        const mem = existing.find((s) => s.id === d.id);
        return mem?.cliResumeId ? { ...d, cliResumeId: mem.cliResumeId } : d;
      });
      const sessions = materializeWechatRemoteSession(
        [...drafts, ...merged],
        wechatRemoteBackingSession,
        wechatRemoteCliResumeId,
      );
      const selectedSessionId = hiddenWechatSessionIds.has(get().selectedSessionId || '')
        ? WECHAT_REMOTE_SESSION_ID
        : get().selectedSessionId;
      const previousSessionId = hiddenWechatSessionIds.has(get().previousSessionId || '')
        ? WECHAT_REMOTE_SESSION_ID
        : get().previousSessionId;
      if (selectedSessionId === WECHAT_REMOTE_SESSION_ID) {
        saveLastSessionId(WECHAT_REMOTE_SESSION_ID);
      }
      set({ sessions, selectedSessionId, previousSessionId, isLoading: false });
    } catch {
      set({ isLoading: false });
    }
  },

  setSearchQuery: (query) => set({ searchQuery: query }),

  setSelectedSession: (id) => {
    saveLastSessionId(id);
    set((state) => ({
      selectedSessionId: id,
      previousSessionId: state.selectedSessionId !== id ? state.selectedSessionId : state.previousSessionId,
    }));
  },

  addDraftSession: (id, projectPath) => set((state) => {
    const projectDir = projectPath.replace(/\//g, '-');
    const draft: SessionListItem = {
      id,
      path: '',
      project: projectPath,
      projectDir,
      modifiedAt: Date.now(),
      preview: '',
      cliResumeId: null,
    };
    return {
      sessions: [draft, ...state.sessions],
      selectedSessionId: id,
    };
  }),

  ensureWechatRemoteSession: (projectPath) => set((state) => ({
    sessions: restoreWechatRemoteCliResumeId(
      upsertWechatRemoteSession(state.sessions, projectPath),
      getWechatRemoteCliResumeId(state.sessions),
    ),
  })),

  touchWechatRemoteSession: (preview, modifiedAt) => set((state) => {
    const existing = state.sessions.find((session) => isWechatRemoteSessionId(session.id));
    const projectPath = existing?.project || state.sessions[0]?.project || '';
    const sessions = upsertWechatRemoteSession(state.sessions, projectPath, modifiedAt)
      .map((session) => (
        session.id === WECHAT_REMOTE_SESSION_ID
          ? { ...session, preview: preview || session.preview, modifiedAt }
          : session
      ));
    return { sessions };
  }),

  updateDraftProject: (id, projectPath) => set((state) => ({
    sessions: state.sessions.map((s) =>
      s.id === id
        ? { ...s, project: projectPath, projectDir: projectPath.replace(/\//g, '-'), modifiedAt: Date.now() }
        : s,
    ),
  })),

  setCustomPreview: (sessionId, name) => {
    const updated = { ...get().customPreviews, [sessionId]: name };
    // Fast local cache
    saveCustomPreviewsLocal(updated);
    set({ customPreviews: updated });
    // Persist to disk via backend (fire-and-forget)
    bridge.saveCustomPreviews(updated).catch(() => {});
  },

  getDisplayName: (session) => {
    const custom = get().customPreviews[session.id];
    return custom || session.preview || '';
  },

  setSessionRunning: (sessionId, running) => set((state) => {
    const next = new Set(state.runningSessions);
    if (running) next.add(sessionId);
    else next.delete(sessionId);
    return { runningSessions: next };
  }),

  isSessionRunning: (sessionId) => get().runningSessions.has(sessionId),

  registerStdinTab: (stdinId, tabId) => {
    const next = { ...get().stdinToTab, [stdinId]: tabId };
    saveStdinToTab(next);
    set({ stdinToTab: next });
    // Drain any orphaned stream buffer that arrived before this mapping existed
    _orphanDrainCallback?.(stdinId, tabId);
  },

  unregisterStdinTab: (stdinId) => {
    const { [stdinId]: _, ...rest } = get().stdinToTab;
    saveStdinToTab(rest);
    set({ stdinToTab: rest });
  },

  getTabForStdin: (stdinId) => get().stdinToTab[stdinId],

  setCliResumeId: (sessionId, cliResumeId) => set((state) => {
    const isWechatRemoteSession = isWechatRemoteSessionId(sessionId);
    if (isWechatRemoteSession) {
      saveWechatRemoteCliResumeId(cliResumeId);
    }
    return {
      sessions: state.sessions.map((s) =>
        s.id === sessionId
          ? { ...s, cliResumeId, ...(isWechatRemoteSession && !cliResumeId ? { path: '' } : {}) }
          : s,
      ),
    };
  }),

  removeDraft: (draftId) => set((state) => ({
    sessions: state.sessions.filter((s) => s.id !== draftId),
  })),

  promoteDraft: (oldDraftId, newRealId) => {
    if (isWechatRemoteSessionId(oldDraftId)) {
      saveLastSessionId(oldDraftId);
      saveWechatRemoteCliResumeId(newRealId);
      set((state) => ({
        sessions: state.sessions.map((session) =>
          isWechatRemoteSessionId(session.id)
            ? { ...session, cliResumeId: newRealId, modifiedAt: Date.now() }
            : session,
        ),
        selectedSessionId: state.selectedSessionId === newRealId
          ? oldDraftId
          : state.selectedSessionId,
        previousSessionId: state.previousSessionId === newRealId
          ? oldDraftId
          : state.previousSessionId,
      }));
      return;
    }

    saveLastSessionId(newRealId);
    // Keep the group ledger in sync: a draft promoted to its real CLI id must
    // stay in its task group (the ledger still referenced the old draft id).
    useGroupStore.getState().replaceSessionId(oldDraftId, newRealId);
    set((state) => {
    // 1) Rename session in the list
    const sessions = state.sessions.map((s) =>
      s.id === oldDraftId ? { ...s, id: newRealId } : s,
    );

    // 2) Update selectedSessionId if it was the draft
    const selectedSessionId = state.selectedSessionId === oldDraftId
      ? newRealId
      : state.selectedSessionId;

    // 3) Migrate runningSessions
    const runningSessions = new Set(state.runningSessions);
    if (runningSessions.has(oldDraftId)) {
      runningSessions.delete(oldDraftId);
      runningSessions.add(newRealId);
    }

    // 4) Migrate stdinToTab entries that pointed to oldDraftId
    const stdinToTab = { ...state.stdinToTab };
    for (const [k, v] of Object.entries(stdinToTab)) {
      if (v === oldDraftId) stdinToTab[k] = newRealId;
    }

    // 5) Migrate previousSessionId if it was the draft
    const previousSessionId = state.previousSessionId === oldDraftId
      ? newRealId
      : state.previousSessionId;

    // 6) Migrate customPreviews if the old draft had a custom name
    const customPreviews = { ...state.customPreviews };
    if (customPreviews[oldDraftId]) {
      customPreviews[newRealId] = customPreviews[oldDraftId];
      delete customPreviews[oldDraftId];
      saveCustomPreviewsLocal(customPreviews);
      bridge.saveCustomPreviews(customPreviews).catch(() => {});
    }

    saveStdinToTab(stdinToTab);
    return { sessions, selectedSessionId, previousSessionId, runningSessions, stdinToTab, customPreviews };
  });
  },

  switchToPrevious: () => {
    const { previousSessionId, selectedSessionId, sessions } = get();
    if (!previousSessionId || previousSessionId === selectedSessionId) return;
    // Verify the previous session still exists
    const exists = sessions.some((s) => s.id === previousSessionId);
    if (!exists) return;
    set({
      selectedSessionId: previousSessionId,
      previousSessionId: selectedSessionId,
    });
  },

  loadCustomPreviewsFromDisk: async () => {
    try {
      const diskPreviews = await bridge.loadCustomPreviews();
      // Merge: disk data takes precedence, but keep any localStorage-only entries
      const localPreviews = get().customPreviews;
      const merged = { ...localPreviews, ...diskPreviews };
      saveCustomPreviewsLocal(merged);
      set({ customPreviews: merged });
    } catch {
      // Silently fall back to localStorage data
    }
  },

  getLastSessionId: () => loadLastSessionId(),

  searchSessionContent: async (query: string) => {
    set({ isContentSearching: true, contentSearchQuery: query });
    try {
      const results = await bridge.searchSessions(query);
      // Stale check: discard if query has changed while awaiting
      if (get().contentSearchQuery !== query) return;
      const map = new Map<string, ContentSearchResult>();
      for (const r of results) {
        map.set(r.session_id, r);
      }
      set({ contentSearchResults: map, isContentSearching: false });
    } catch {
      set({ isContentSearching: false });
    }
  },

  clearContentSearch: () => {
    set({
      contentSearchResults: new Map<string, ContentSearchResult>(),
      isContentSearching: false,
      contentSearchQuery: '',
    });
  },
}));
