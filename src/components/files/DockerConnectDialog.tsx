import { useCallback, useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { bridge } from '../../lib/tauri-bridge';
import type { ContainerSummary, MountEntry } from '../../lib/tauri-bridge';
import { useSettingsStore } from '../../stores/settingsStore';
import { useT } from '../../lib/i18n';

interface DockerConnectDialogProps {
  open: boolean;
  onClose: () => void;
}

type Step = 'containers' | 'mounts';

/**
 * Two-step Docker container connect flow:
 *  1. Pick a running container (docker ps).
 *  2. Pick one of the container's bind-mount destinations as the project root,
 *     connect + preflight, then persist via setWorkingProject.
 * The selected mount is used as the project root; sub-directory selection is
 * left to the existing FileExplorer tree (v1 simplification).
 */
export function DockerConnectDialog({ open, onClose }: DockerConnectDialogProps) {
  const t = useT();
  const setWorkingProject = useSettingsStore((s) => s.setWorkingProject);

  const [step, setStep] = useState<Step>('containers');
  const [containers, setContainers] = useState<ContainerSummary[]>([]);
  const [mounts, setMounts] = useState<MountEntry[]>([]);
  const [selected, setSelected] = useState<ContainerSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [connecting, setConnecting] = useState(false);
  /** i18n key of the current error, or null. */
  const [errorKey, setErrorKey] = useState<string | null>(null);
  /** Raw Rust error string, shown alongside the generic `docker.error` fallback. */
  const [rawError, setRawError] = useState<string | null>(null);
  /** Whether the error offers a one-click `docker start`. */
  const [canStart, setCanStart] = useState(false);

  /** Map a raw Rust error string to an i18n key. */
  const mapError = useCallback((err: unknown): string => {
    const msg = String((err as { message?: string })?.message ?? err ?? '');
    setRawError(null);
    if (msg.includes('docker not available')) return 'docker.notInstalled';
    if (msg.startsWith('CONTAINER_NOT_RUNNING')) return 'docker.notRunning';
    if (msg.startsWith('PATH_NOT_MOUNTED') || msg.startsWith('HOST_PATH_MISSING'))
      return 'docker.notMounted';
    if (msg.startsWith('CLAUDE_NOT_FOUND_IN_CONTAINER')) return 'docker.claudeMissing';
    // Unknown error: generic fallback plus the raw string for context, rather
    // than misleadingly claiming Docker is not installed.
    setRawError(msg || null);
    return 'docker.error';
  }, []);

  const loadContainers = useCallback(async () => {
    setLoading(true);
    setErrorKey(null);
    setCanStart(false);
    try {
      const list = await bridge.listDockerContainers();
      setContainers(list.filter((c) => c.state === 'running'));
    } catch (err) {
      setErrorKey(mapError(err));
      setContainers([]);
    } finally {
      setLoading(false);
    }
  }, [mapError]);

  useEffect(() => {
    if (!open) return;
    setStep('containers');
    setSelected(null);
    setMounts([]);
    setErrorKey(null);
    setCanStart(false);
    loadContainers();
  }, [open, loadContainers]);

  const openMounts = useCallback(async (container: ContainerSummary) => {
    setSelected(container);
    setStep('mounts');
    setLoading(true);
    setErrorKey(null);
    setCanStart(false);
    try {
      setMounts(await bridge.listContainerMounts(container.name));
    } catch (err) {
      setErrorKey(mapError(err));
      setMounts([]);
    } finally {
      setLoading(false);
    }
  }, [mapError]);

  const handleStart = useCallback(async () => {
    if (!selected) return;
    setConnecting(true);
    setErrorKey(null);
    setCanStart(false);
    try {
      await bridge.startContainer(selected.name);
      await openMounts(selected);
    } catch (err) {
      setErrorKey(mapError(err));
    } finally {
      setConnecting(false);
    }
  }, [selected, openMounts, mapError]);

  const handleConnect = useCallback(async (dest: string) => {
    if (!selected) return;
    setConnecting(true);
    setErrorKey(null);
    setCanStart(false);
    try {
      await bridge.connectDockerProject(selected.name, dest);
      await bridge.dockerPreflight(selected.name);
      setWorkingProject(dest, {
        kind: 'docker',
        container: selected.name,
        containerCwd: dest,
      });
      onClose();
    } catch (err) {
      const key = mapError(err);
      setErrorKey(key);
      setCanStart(key === 'docker.notRunning');
    } finally {
      setConnecting(false);
    }
  }, [selected, setWorkingProject, onClose, mapError]);

  if (!open) return null;

  return createPortal(
    <div
      className="fixed inset-0 z-[10000] flex items-center justify-center bg-black/40"
      onClick={onClose}
    >
      <div
        className="bg-bg-card border border-border-subtle rounded-xl p-5
          shadow-lg max-w-md w-full mx-4 animate-fade-in"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm font-semibold text-text-primary">
            {step === 'mounts' && selected
              ? `🐳 ${selected.name}`
              : t('docker.title')}
          </h3>
          {step === 'mounts' ? (
            <button
              onClick={() => { setStep('containers'); setErrorKey(null); setCanStart(false); }}
              className="text-xs text-text-muted hover:text-text-primary transition-smooth cursor-pointer"
            >
              {t('docker.back')}
            </button>
          ) : (
            <button
              onClick={loadContainers}
              className="text-xs text-text-muted hover:text-text-primary transition-smooth cursor-pointer"
            >
              {t('docker.refresh')}
            </button>
          )}
        </div>

        {step === 'mounts' && (
          <p className="text-xs text-text-muted mb-3">{t('docker.pickMount')}</p>
        )}

        {/* Body */}
        <div className="max-h-72 overflow-y-auto flex flex-col gap-1.5">
          {loading || connecting ? (
            <p className="text-xs text-text-tertiary py-4 text-center">
              {connecting ? t('docker.connecting') : '…'}
            </p>
          ) : step === 'containers' ? (
            containers.length === 0 ? (
              !errorKey && (
                <p className="text-xs text-text-tertiary py-4 text-center">
                  {t('docker.noContainers')}
                </p>
              )
            ) : (
              containers.map((c) => (
                <button
                  key={c.id}
                  onClick={() => openMounts(c)}
                  className="flex flex-col items-start gap-0.5 px-3 py-2 rounded-lg
                    border border-border-subtle text-left
                    hover:border-accent hover:bg-accent/5 transition-smooth cursor-pointer"
                >
                  <span className="text-sm text-text-primary">🐳 {c.name}</span>
                  <span className="text-[10px] text-text-tertiary truncate max-w-full">{c.image}</span>
                </button>
              ))
            )
          ) : (
            mounts.length === 0 ? (
              !errorKey && (
                <p className="text-xs text-text-tertiary py-4 text-center">
                  {t('docker.notMounted')}
                </p>
              )
            ) : (
              mounts.map((m) => (
                <button
                  key={m.destination}
                  onClick={() => handleConnect(m.destination)}
                  className="flex flex-col items-start gap-0.5 px-3 py-2 rounded-lg
                    border border-border-subtle text-left
                    hover:border-accent hover:bg-accent/5 transition-smooth cursor-pointer"
                >
                  <span className="text-sm text-text-primary font-mono">{m.destination}</span>
                  <span className="text-[10px] text-text-tertiary truncate max-w-full">{m.source}</span>
                </button>
              ))
            )
          )}
        </div>

        {/* Error */}
        {errorKey && (
          <div className="mt-3 flex items-center justify-between gap-2">
            <p className="text-xs text-error">
              {t(errorKey)}
              {errorKey === 'docker.error' && rawError ? `: ${rawError}` : ''}
            </p>
            {canStart && selected && (
              <button
                onClick={handleStart}
                className="px-3 py-1.5 text-xs rounded-lg bg-accent/10 text-accent
                  hover:bg-accent/20 transition-smooth cursor-pointer flex-shrink-0"
              >
                {t('docker.startContainer')}
              </button>
            )}
          </div>
        )}
      </div>
    </div>,
    document.body,
  );
}
