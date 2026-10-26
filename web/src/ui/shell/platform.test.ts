import { describe, expect, it } from 'vitest';
import { isPhoneBrowser } from './platform';

describe('isPhoneBrowser', () => {
  it('knows phones and tablets', () => {
    expect(isPhoneBrowser({ userAgent: 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15' })).toBe(true);
    expect(isPhoneBrowser({ userAgent: 'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 Chrome/126 Mobile Safari/537.36' })).toBe(true);
    expect(isPhoneBrowser({ userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15', platform: 'MacIntel', maxTouchPoints: 5 })).toBe(true);
  });
  it('treats a desktop (a Mac without a touch screen too) and an unknown navigator as not a phone', () => {
    expect(isPhoneBrowser({ userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/126', platform: 'Win32', maxTouchPoints: 0 })).toBe(false);
    expect(isPhoneBrowser({ userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)', platform: 'MacIntel', maxTouchPoints: 0 })).toBe(false);
    expect(isPhoneBrowser(undefined)).toBe(false);
  });
});
