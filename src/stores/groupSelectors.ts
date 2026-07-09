import type { SessionListItem } from '../lib/tauri-bridge';
import type { SessionGroup } from './groupStore';

export interface PartitionedGroup {
  group: SessionGroup;
  /** Group members, in-group-pinned first, then ledger order. Missing sessions skipped. */
  sessions: SessionListItem[];
}

export interface PartitionedWorkspace {
  groups: PartitionedGroup[];
  /** Sessions in no group, newest-first. */
  ungrouped: SessionListItem[];
}

/**
 * Split one workspace's sessions into its groups + an ungrouped bucket.
 * Pure view-layer derivation over the group ledger; renders the three-level
 * "workspace › group › session + ungrouped" tree. Does not touch global pin.
 */
export function partitionWorkspaceSessions(
  sessions: SessionListItem[],
  groups: SessionGroup[],
): PartitionedWorkspace {
  const byId = new Map(sessions.map((s) => [s.id, s]));
  const grouped = new Set<string>();

  const partitionedGroups: PartitionedGroup[] = groups.map((group) => {
    const pinnedSet = new Set(group.pinnedInGroup);
    const pinnedFirst = [
      ...group.sessionIds.filter((id) => pinnedSet.has(id)),
      ...group.sessionIds.filter((id) => !pinnedSet.has(id)),
    ];
    const groupSessions: SessionListItem[] = [];
    for (const id of pinnedFirst) {
      const s = byId.get(id);
      if (s) {
        groupSessions.push(s);
        grouped.add(id);
      }
    }
    return { group, sessions: groupSessions };
  });

  const ungrouped = sessions
    .filter((s) => !grouped.has(s.id))
    .sort((a, b) => b.modifiedAt - a.modifiedAt);

  return { groups: partitionedGroups, ungrouped };
}
