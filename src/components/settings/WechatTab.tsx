import { useCallback, useEffect, useRef, useState } from 'react';
import {
  bridge,
  onWechatStatus,
  type WechatAccountInfo,
  type WechatStatus,
} from '../../lib/tauri-bridge';
import { useT } from '../../lib/i18n';
import { resolveWechatQrPollResult, type WechatPhase } from './wechatLoginState';

export function WechatTab() {
  const t = useT();
  const [phase, setPhase] = useState<WechatPhase>('loading');
  const [account, setAccount] = useState<WechatAccountInfo | null>(null);
  const [qrcodeId, setQrcodeId] = useState('');
  const [qrcodeImage, setQrcodeImage] = useState('');
  const [message, setMessage] = useState('');
  const pollingRef = useRef(false);

  const applyStatus = useCallback((status: WechatStatus) => {
    setAccount(status.account);
    setPhase(status.connected ? 'connected' : 'idle');
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
    try {
      const qr = await bridge.wechatStartQrLogin();
      setQrcodeId(qr.qrcodeId);
      setQrcodeImage(qr.qrcodeImage);
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
      const result = await bridge.wechatPollQrLogin(qrcodeId);
      const next = resolveWechatQrPollResult(result);
      setAccount(next.account);
      setMessage(next.message);
      if (next.clearQr) {
        setQrcodeId('');
        setQrcodeImage('');
      }
      setPhase(next.phase);
    } catch (err) {
      setMessage(String(err));
      setPhase('error');
    } finally {
      pollingRef.current = false;
    }
  }, [qrcodeId]);

  useEffect(() => {
    if (!qrcodeId || (phase !== 'waiting' && phase !== 'scanned')) return;
    const timer = window.setInterval(() => {
      pollLogin();
    }, 2500);
    pollLogin();
    return () => window.clearInterval(timer);
  }, [phase, pollLogin, qrcodeId]);

  const disconnect = useCallback(async () => {
    setMessage('');
    try {
      await bridge.wechatDisconnect();
      setAccount(null);
      setQrcodeId('');
      setQrcodeImage('');
      setPhase('idle');
    } catch (err) {
      setMessage(String(err));
      setPhase('error');
    }
  }, []);

  const busy = phase === 'loading' || phase === 'requesting';
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
          <div className="mt-4 flex items-center gap-4">
            <img
              src={qrcodeImage}
              alt={t('wechat.qrAlt')}
              className="w-44 h-44 rounded-lg border border-border-subtle bg-white p-2"
            />
            <div className="min-w-0">
              <div className="text-[13px] font-medium text-text-primary">
                {phase === 'scanned' ? t('wechat.scanned') : t('wechat.waiting')}
              </div>
              <div className="mt-1 text-xs text-text-tertiary leading-relaxed">
                {message || t('wechat.waitingDetail')}
              </div>
              <button
                onClick={startLogin}
                className="mt-3 px-2.5 py-1 text-xs font-medium rounded-md border border-border-subtle
                  text-text-muted hover:bg-bg-tertiary hover:text-text-primary transition-smooth"
              >
                {t('wechat.refreshQr')}
              </button>
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
    </div>
  );
}
