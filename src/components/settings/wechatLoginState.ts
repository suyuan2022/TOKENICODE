import type { WechatAccountInfo, WechatQrPoll } from '../../lib/tauri-bridge';

export type WechatPhase =
  | 'loading'
  | 'idle'
  | 'requesting'
  | 'waiting'
  | 'scanned'
  | 'connected'
  | 'sessionExpired'
  | 'error';

interface WechatQrPollTransition {
  phase: WechatPhase;
  account: WechatAccountInfo | null;
  message: string;
  clearQr: boolean;
}

export function resolveWechatQrPollResult(result: WechatQrPoll): WechatQrPollTransition {
  if (result.connected) {
    return {
      phase: 'connected',
      account: result.account ?? null,
      message: '',
      clearQr: true,
    };
  }

  const message = result.message ?? '';
  if (result.status === 'scaned' || result.status === 'scanned' || result.status === 'scaned_but_redirect') {
    return {
      phase: 'scanned',
      account: null,
      message,
      clearQr: false,
    };
  }

  if (result.status === 'wait' || result.status === 'need_verifycode') {
    return {
      phase: 'waiting',
      account: null,
      message,
      clearQr: false,
    };
  }

  return {
    phase: 'error',
    account: null,
    message: message || result.status,
    clearQr: true,
  };
}
