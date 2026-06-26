import { describe, expect, it } from 'vitest';
import { resolveWechatQrPollResult, resolveWechatStatusPhase } from '../wechatLoginState';

describe('wechat QR login state', () => {
  it('clears the stale QR image when the iLink QR expires', () => {
    expect(resolveWechatQrPollResult({
      status: 'expired',
      connected: false,
      message: 'QR code expired',
    })).toEqual({
      phase: 'error',
      account: null,
      message: 'QR code expired',
      clearQr: true,
      redirectBaseUrl: null,
    });
  });

  it('keeps redirect scan states in the waiting flow', () => {
    expect(resolveWechatQrPollResult({
      status: 'scaned_but_redirect',
      connected: false,
      message: 'redirecting',
      redirectBaseUrl: 'https://hk.weixin.qq.com',
    })).toEqual({
      phase: 'scanned',
      account: null,
      message: 'redirecting',
      clearQr: false,
      redirectBaseUrl: 'https://hk.weixin.qq.com',
    });
  });

  it('does not let a late disconnected status hide an active QR login', () => {
    expect(resolveWechatStatusPhase(false, 'requesting')).toBe('requesting');
    expect(resolveWechatStatusPhase(false, 'waiting')).toBe('waiting');
    expect(resolveWechatStatusPhase(false, 'scanned')).toBe('scanned');
    expect(resolveWechatStatusPhase(false, 'connected')).toBe('idle');
    expect(resolveWechatStatusPhase(true, 'waiting')).toBe('connected');
  });
});
