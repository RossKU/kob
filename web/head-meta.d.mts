export const TESTNET_TITLE_PREFIX: string;
export function parseBuildNetwork(raw: string | undefined): 'mainnet' | 'testnet-10';
export function parsePublicOrigin(raw: string | undefined): string | null;
export function applyHead(html: string, opts: { network?: string; publicUrl?: string }): string;
