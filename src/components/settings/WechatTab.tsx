import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import {
  bridge,
  onWechatStatus,
  type WechatAccountInfo,
  type WechatStatus,
} from '../../lib/tauri-bridge';
import { useT } from '../../lib/i18n';
import { showToast } from '../shared/Toast';
import { useSessionStore } from '../../stores/sessionStore';
import { useSettingsStore } from '../../stores/settingsStore';
import {
  WECHAT_CONNECTED_EVENT,
  resolveWechatRemoteWorkspace,
} from '../../lib/wechat-session';
import {
  resolveWechatQrPollResult,
  resolveWechatStatusPhase,
  type WechatPhase,
} from './wechatLoginState';

const QR_EXPIRES_AFTER_MS = 140_000;

function workspaceLabel(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() || path;
}

function compactWorkspacePath(path: string): string {
  return path.replace(/^\/Users\/[^/]+/, '~');
}

export function WechatTab() {
  const t = useT();
  const sessions = useSessionStore((state) => state.sessions);
  const ensureWechatRemoteSession = useSessionStore((state) => state.ensureWechatRemoteSession);
  const workingDirectory = useSettingsStore((state) => state.workingDirectory);
  const wechatWorkspacePath = useSettingsStore((state) => state.wechatWorkspacePath);
  const setWechatWorkspacePath = useSettingsStore((state) => state.setWechatWorkspacePath);
  const [phase, setPhase] = useState<WechatPhase>('loading');
  const [account, setAccount] = useState<WechatAccountInfo | null>(null);
  const [qrcodeId, setQrcodeId] = useState('');
  const [qrcodeImage, setQrcodeImage] = useState('');
  const [qrcodeUrl, setQrcodeUrl] = useState('');
  const [qrcodeCreatedAt, setQrcodeCreatedAt] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [pollBaseUrl, setPollBaseUrl] = useState('');
  const [message, setMessage] = useState('');
  const pollingRef = useRef(false);
  const workspaceOptions = useMemo(() => {
    const paths = [
      wechatWorkspacePath,
      workingDirectory,
      ...sessions.map((session) => session.project || session.projectDir),
    ]
      .map((path) => path?.trim())
      .filter((path): path is string => Boolean(path));
    return Array.from(new Set(paths));
  }, [sessions, wechatWorkspacePath, workingDirectory]);
  const effectiveWorkspacePath = resolveWechatRemoteWorkspace(
    wechatWorkspacePath,
    workingDirectory,
  );

  const bindWorkspace = useCallback((path: string) => {
    setWechatWorkspacePath(path);
    const nextWorkspace = resolveWechatRemoteWorkspace(path, workingDirectory);
    if (nextWorkspace) {
      ensureWechatRemoteSession(nextWorkspace);
      window.dispatchEvent(new Event(WECHAT_CONNECTED_EVENT));
    }
  }, [ensureWechatRemoteSession, setWechatWorkspacePath, workingDirectory]);

  const chooseWorkspace = useCallback(async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: t('wechat.workspaceSelectTitle'),
    });
    if (typeof selected === 'string') {
      bindWorkspace(selected);
    }
  }, [bindWorkspace, t]);

  const applyStatus = useCallback((status: WechatStatus) => {
    setAccount(status.account);
    setPhase((currentPhase) => resolveWechatStatusPhase(status.connected, currentPhase));
    if (status.connected) {
      window.dispatchEvent(new Event(WECHAT_CONNECTED_EVENT));
    }
  }, []);

  const refreshStatus = useCallback(async () => {
    try {
      const status = await bridge.wechatGetStatus();
      applyStatus(status);
    } catch (err) {
      setMessage(String(err));
      setPhase('error');
    }
  }, [applyStatus]);

  useEffect(() => {
    refreshStatus();
  }, [refreshStatus]);

  useEffect(() => {
    let disposed = false;
    let cleanup: (() => void) | null = null;

    onWechatStatus((status) => {
      if (status.status !== 'sessionExpired') return;
      setAccount(null);
      setQrcodeId('');
      setQrcodeImage('');
      setQrcodeUrl('');
      setQrcodeCreatedAt(null);
      setPollBaseUrl('');
      setMessage('');
      setPhase('sessionExpired');
    }).then((unlisten) => {
      if (disposed) {
        unlisten();
      } else {
        cleanup = unlisten;
      }
    }).catch(() => {});

    return () => {
      disposed = true;
      cleanup?.();
    };
  }, []);

  const startLogin = useCallback(async () => {
    setPhase('requesting');
    setMessage('');
    setQrcodeId('');
    setQrcodeImage('');
    setQrcodeUrl('');
    setQrcodeCreatedAt(null);
    setPollBaseUrl('');
    try {
      const qr = await bridge.wechatStartQrLogin();
      setQrcodeId(qr.qrcodeId);
      setQrcodeImage(qr.qrcodeImage);
      setQrcodeUrl(qr.qrcodeUrl);
      setQrcodeCreatedAt(Date.now());
      setPhase('waiting');
    } catch (err) {
      setMessage(String(err));
      setPhase('error');
    }
  }, []);

  const pollLogin = useCallback(async () => {
    if (!qrcodeId || pollingRef.current) return;
    pollingRef.current = true;
    try {
      const result = await bridge.wechatPollQrLogin(qrcodeId, undefined, pollBaseUrl || undefined);
      const next = resolveWechatQrPollResult(result);
      setAccount(next.account);
      setMessage(next.message);
      if (next.redirectBaseUrl) {
        setPollBaseUrl(next.redirectBaseUrl);
      }
      if (next.clearQr) {
        setQrcodeId('');
        setQrcodeImage('');
        setQrcodeUrl('');
        setQrcodeCreatedAt(null);
        setPollBaseUrl('');
      }
      setPhase(next.phase);
    } catch (err) {
      setMessage(String(err));
      setPhase('error');
    } finally {
      pollingRef.current = false;
    }
  }, [pollBaseUrl, qrcodeId]);

  useEffect(() => {
    if (!qrcodeId || (phase !== 'waiting' && phase !== 'scanned')) return;
    const timer = window.setInterval(() => {
      pollLogin();
    }, 2500);
    pollLogin();
    return () => window.clearInterval(timer);
  }, [phase, pollLogin, qrcodeId]);

  useEffect(() => {
    if (!qrcodeCreatedAt || phase === 'connected') return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [phase, qrcodeCreatedAt]);

  const disconnect = useCallback(async () => {
    setMessage('');
    try {
      await bridge.wechatDisconnect();
      setAccount(null);
      setQrcodeId('');
      setQrcodeImage('');
      setQrcodeUrl('');
      setQrcodeCreatedAt(null);
      setPollBaseUrl('');
      setPhase('idle');
    } catch (err) {
      setMessage(String(err));
      setPhase('error');
    }
  }, []);

  const busy = phase === 'loading' || phase === 'requesting';
  const copyQrLink = useCallback(() => {
    if (!qrcodeUrl) return;
    navigator.clipboard.writeText(qrcodeUrl)
      .then(() => showToast(t('wechat.copyQrLinkDone'), 'success'))
      .catch((err) => showToast(String(err), 'error'));
  }, [qrcodeUrl, t]);
  const secondsRemaining = qrcodeCreatedAt
    ? Math.max(0, Math.ceil((qrcodeCreatedAt + QR_EXPIRES_AFTER_MS - now) / 1000))
    : null;
  const statusTitle = phase === 'sessionExpired'
    ? t('wechat.sessionExpired')
    : phase === 'connected'
      ? t('wechat.connected')
      : t('wechat.disconnected');

  return (
    <div className="space-y-5">
      <div>
        <h3 className="text-[13px] font-medium text-text-primary mb-2">
          {t('wechat.title')}
        </h3>
        <p className="text-xs text-text-tertiary leading-relaxed">
          {t('wechat.subtitle')}
        </p>
      </div>

      <div className="rounded-lg border border-border-subtle bg-bg-secondary/40 p-4">
        <div className="flex items-center justify-between gap-4">
          <div>
            <div className="text-[13px] font-medium text-text-primary">
              {statusTitle}
            </div>
            <div className="mt-1 text-xs text-text-tertiary">
              {account ? account.userId : t('wechat.noAccount')}
            </div>
          </div>
          {phase === 'connected' ? (
            <button
              onClick={disconnect}
              className="px-3 py-1.5 text-[13px] font-medium rounded-lg border border-border-subtle
                text-text-muted hover:bg-bg-tertiary hover:text-text-primary transition-smooth"
            >
              {t('wechat.disconnect')}
            </button>
          ) : (
            <button
              onClick={startLogin}
              disabled={busy}
              className="px-3 py-1.5 text-[13px] font-medium rounded-lg bg-accent text-text-inverse
                hover:bg-accent-hover transition-smooth disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {busy ? t('wechat.connecting') : t('wechat.connect')}
            </button>
          )}
        </div>

        {qrcodeImage && phase !== 'connected' && (
          <div className="mt-4 flex items-start gap-5">
            <img
              src={qrcodeImage}
              alt={t('wechat.qrAlt')}
              className="h-80 w-80 shrink-0 rounded-lg border border-border-subtle bg-white p-3 shadow-sm [image-rendering:pixelated]"
            />
            <div className="min-w-0 pt-2">
              <div className="text-[13px] font-medium text-text-primary">
                {phase === 'scanned' ? t('wechat.scanned') : t('wechat.waiting')}
              </div>
              <div className="mt-1 text-xs text-text-tertiary leading-relaxed">
                {message || t('wechat.waitingDetail')}
              </div>
              {secondsRemaining !== null && (
                <div className="mt-1 text-xs text-text-tertiary">
                  {secondsRemaining > 0
                    ? t('wechat.qrExpiresIn').replace('{seconds}', String(secondsRemaining))
                    : t('wechat.qrExpiredHint')}
                </div>
              )}
              <button
                onClick={startLogin}
                className="mt-3 px-2.5 py-1 text-xs font-medium rounded-md border border-border-subtle
                  text-text-muted hover:bg-bg-tertiary hover:text-text-primary transition-smooth"
              >
                {t('wechat.refreshQr')}
              </button>
              {qrcodeUrl && (
                <button
                  onClick={copyQrLink}
                  className="ml-2 mt-3 px-2.5 py-1 text-xs font-medium rounded-md border border-border-subtle
                    text-text-muted hover:bg-bg-tertiary hover:text-text-primary transition-smooth"
                >
                  {t('wechat.copyQrLink')}
                </button>
              )}
            </div>
          </div>
        )}

        {(phase === 'sessionExpired' || (phase === 'error' && message)) && (
          <div className={`mt-4 rounded-lg px-3 py-2 text-[13px] ${
            phase === 'sessionExpired'
              ? 'bg-amber-500/10 text-amber-600 dark:text-amber-400'
              : 'bg-red-500/10 text-red-500'
          }`}>
            {phase === 'sessionExpired' ? t('wechat.sessionExpiredDetail') : message}
          </div>
        )}
      </div>

      <div className="rounded-lg border border-border-subtle bg-bg-secondary/40 p-4">
        <div className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <div className="text-[13px] font-medium text-text-primary">
              {t('wechat.workspaceBinding')}
            </div>
            <div className="mt-1 text-xs text-text-tertiary leading-relaxed">
              {effectiveWorkspacePath
                ? t('wechat.workspaceBindingDetail').replace(
                  '{workspace}',
                  compactWorkspacePath(effectiveWorkspacePath),
                )
                : t('wechat.workspaceBindingEmpty')}
            </div>
          </div>
          {workingDirectory && (
            <button
              onClick={() => bindWorkspace(workingDirectory)}
              className="shrink-0 px-3 py-1.5 text-[13px] font-medium rounded-lg border border-border-subtle
                text-text-muted hover:bg-bg-tertiary hover:text-text-primary transition-smooth"
            >
              {t('wechat.bindCurrentWorkspace')}
            </button>
          )}
        </div>

        <div className="mt-3 flex items-center gap-2">
          <select
            value={wechatWorkspacePath}
            onChange={(event) => bindWorkspace(event.target.value)}
            className="min-w-0 flex-1 rounded-lg border border-border-subtle bg-bg-primary px-3 py-2
              text-[13px] text-text-primary outline-none focus:border-accent"
          >
            <option value="">
              {t('wechat.followCurrentWorkspace')}
            </option>
            {workspaceOptions.map((path) => (
              <option key={path} value={path}>
                {workspaceLabel(path)} · {compactWorkspacePath(path)}
              </option>
            ))}
          </select>
          <button
            onClick={chooseWorkspace}
            className="px-3 py-2 text-[13px] font-medium rounded-lg border border-border-subtle
              text-text-muted hover:bg-bg-tertiary hover:text-text-primary transition-smooth"
          >
            {t('wechat.chooseWorkspace')}
          </button>
        </div>
      </div>
    </div>
  );
}
