import { describe, it, expect } from 'vitest';
import { partitionWorkspaceSessions } from '../groupSelectors';
import type { SessionGroup } from '../groupStore';
import type { SessionListItem } from '../../lib/tauri-bridge';

function sess(id: string, modifiedAt = 0): SessionListItem {
  return {
    id,
    path: '',
    project: '~/ws',
    projectDir: '',
    modifiedAt,
    preview: '',
    cliResumeId: null,
  };
}

function grp(id: string, sessionIds: string[], pinnedInGroup: string[] = []): SessionGroup {
  return { id, label: id, workspace: '~/ws', sessionIds, pinnedInGroup };
}

describe('partitionWorkspaceSessions', () => {
  it('places sessions into their groups in ledger order', () => {
    const sessions = [sess('a'), sess('b'), sess('c')];
    const groups = [grp('g1', ['b', 'a'])];
    const result = partitionWorkspaceSessions(sessions, groups);
    expect(result.groups[0].sessions.map((s) => s.id)).toEqual(['b', 'a']);
  });

  it('puts in-group pinned sessions first within the group', () => {
    const sessions = [sess('a'), sess('b'), sess('c')];
    const groups = [grp('g1', ['a', 'b', 'c'], ['c'])];
    const result = partitionWorkspaceSessions(sessions, groups);
    expect(result.groups[0].sessions.map((s) => s.id)).toEqual(['c', 'a', 'b']);
  });

  it('lists ungrouped sessions sorted by recency (newest first)', () => {
    const sessions = [sess('old', 100), sess('new', 300), sess('mid', 200)];
    const result = partitionWorkspaceSessions(sessions, []);
    expect(result.ungrouped.map((s) => s.id)).toEqual(['new', 'mid', 'old']);
  });

  it('excludes grouped sessions from the ungrouped bucket', () => {
    const sessions = [sess('a'), sess('b'), sess('c')];
    const groups = [grp('g1', ['a'])];
    const result = partitionWorkspaceSessions(sessions, groups);
    expect(result.groups[0].sessions.map((s) => s.id)).toEqual(['a']);
    expect(result.ungrouped.map((s) => s.id).sort()).toEqual(['b', 'c']);
  });

  it('skips ledger ids whose session no longer exists', () => {
    const sessions = [sess('a')];
    const groups = [grp('g1', ['a', 'ghost'])];
    const result = partitionWorkspaceSessions(sessions, groups);
    expect(result.groups[0].sessions.map((s) => s.id)).toEqual(['a']);
  });
});
