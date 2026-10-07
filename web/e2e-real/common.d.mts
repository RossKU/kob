// Type declarations of e2e-real/common.mjs (plain node ESM). Playwright types are left loose on purpose.
export const chromium: any;
export const E2E_REAL_DIR: string;
export const WEB_ROOT: string;
export const REPO_ROOT: string;
export const EXT_ROOT: string;
export const PROFILE_ROOT: string;
export const OUT_DIR: string;
export const ENV_PATH: string;
export const WALLET_GATE_VENDOR_CANDIDATES: string[];
export const sleep: (ms: number) => Promise<void>;
export const collapse: (s: string) => string;
export const log: (...a: unknown[]) => void;

export type RealWallet = 'kasware' | 'kaspire' | 'kastle';
export const EXTENSIONS: Record<RealWallet, { label: string; vendorDir: string; version: string; id?: string; crxUrl?: string; crxSha256?: string; zipUrl?: string; zipSha256?: string }>;
export function crxToZip(buf: Uint8Array): Buffer;
export function unpackZip(buf: Uint8Array, dir: string): string[];
export function unpackCrx(buf: Uint8Array, dir: string): string[];
export function extensionVersion(dir: string): string;
export function ensureExtension(
  wallet: RealWallet,
  opts?: { download?: boolean; force?: boolean; extRoot?: string; vendors?: string[] },
): Promise<{ dir: string; version: string; source: 'cache' | 'vendor' | 'download'; sha256?: string }>;

export function parseEnv(text: string): Record<string, string>;
export function loadEnv(path?: string): Record<string, string>;
export function upsertEnv(pairs: Record<string, string>, path?: string): void;
export function kaspaNode(): any;
export function randomMnemonic(): string;
export function ensureMnemonic(key: string, path?: string): string;
export function kaswareAddress(phrase: string, network?: string): string;

export function launchWalletBrowser(o: { extensionDir: string; profileName: string; headless?: boolean; extraArgs?: string[]; viewport?: { width: number; height: number }; fresh?: boolean }): Promise<any>;
export function extensionId(ctx: any, timeoutMs?: number): Promise<string>;
export function shot(page: any, dir: string, name: string): Promise<string>;
export function saveResults(name: string, obj: unknown): string;

export const POPUP_MATCHERS: Record<RealWallet, (url: string) => boolean>;
export function isWalletPopup(wallet: RealWallet, page: { isClosed(): boolean; url(): string }): boolean;
export function findPopup(ctx: any, wallet: RealWallet, o?: { handled?: WeakSet<object>; timeoutMs?: number; pollMs?: number }): Promise<any | null>;
export interface PopupWatch {
  seen: { index: number; url: string; result?: unknown; error?: string }[];
  stop(): Promise<PopupWatch['seen']>;
}
export function approvePopups(ctx: any, handler: (page: any, info: { wallet: RealWallet; index: number }) => Promise<unknown> | unknown, o: { wallet: RealWallet; pollMs?: number }): PopupWatch;
export function withPopupApproval<T>(ctx: any, wallet: RealWallet, start: () => Promise<T>, handler: (page: any, info: { wallet: RealWallet; index: number }) => Promise<unknown> | unknown, opts?: { pollMs?: number }): Promise<{ result: T; popups: PopupWatch['seen'] }>;
export function kaswareApprover(o?: { approve?: boolean; screenshotDir?: string }): (pop: any) => Promise<any>;
export function kaspireApprover(o?: { approve?: boolean; password?: string; screenshotDir?: string }): (pop: any) => Promise<any>;
export function kastleApprover(o?: { approve?: boolean; password?: string; screenshotDir?: string; rawDetails?: boolean }): (pop: any) => Promise<any>;

export function onboardKasware(ctx: any, words: string[], o?: { password?: string }): Promise<any>;
export function switchKaswareToTn10(page: any): Promise<string>;
export function onboardKaspire(ctx: any, extId: string, mnemonic: string, o: { password: string }): Promise<string>;
export function onboardKastle(ctx: any, extId: string, o: { mnemonic: string; password: string }): Promise<boolean>;
