import { describe, it, expect, beforeEach } from 'vitest';
import { useSettingsStore } from '../settingsStore';

describe('workspace backend', () => {
  beforeEach(() => {
    useSettingsStore.setState({ workingDirectory: '', workingBackend: { kind: 'local' } });
  });

  it('defaults to local backend', () => {
    expect(useSettingsStore.getState().workingBackend).toEqual({ kind: 'local' });
  });

  it('setWorkingDirectory keeps local backend', () => {
    useSettingsStore.getState().setWorkingDirectory('/Users/me/p');
    expect(useSettingsStore.getState().workingBackend.kind).toBe('local');
  });

  it('setWorkingProject stores docker backend with container and cwd', () => {
    useSettingsStore.getState().setWorkingProject('/workspace', {
      kind: 'docker', container: 'dev-box', containerCwd: '/workspace' });
    const s = useSettingsStore.getState();
    expect(s.workingDirectory).toBe('/workspace');
    expect(s.workingBackend).toEqual({ kind: 'docker', container: 'dev-box', containerCwd: '/workspace' });
  });

  it('switching back to a local folder resets backend', () => {
    useSettingsStore.getState().setWorkingProject('/workspace', {
      kind: 'docker', container: 'dev-box', containerCwd: '/workspace' });
    useSettingsStore.getState().setWorkingDirectory('/Users/me/p');
    expect(useSettingsStore.getState().workingBackend).toEqual({ kind: 'local' });
  });
});
