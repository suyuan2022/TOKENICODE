import { bridge } from '../lib/tauri-bridge';
import { useGroupStore, type SessionGroup } from './groupStore';

let saveSubscribed = false;

/**
 * Load groups from disk into the store, then keep the disk copy in sync on every
 * change. Mirrors the pinned/archived persistence pattern
 * (bridge → Tauri command → ~/.tokenicode/groups.json). Call once on app startup.
 */
export async function initGroupPersistence(): Promise<void> {
  const data = await bridge.loadSessionGroups();
  if (Array.isArray(data) && data.length > 0) {
    useGroupStore.setState({ groups: data as SessionGroup[] });
  }

  // Subscribe exactly once: every later store mutation writes back to disk.
  // Registered after the initial hydrate so loading doesn't trigger a save.
  if (!saveSubscribed) {
    saveSubscribed = true;
    useGroupStore.subscribe((state) => {
      bridge.saveSessionGroups(state.groups);
    });
  }
}
