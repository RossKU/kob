export function encodeAddress(prefix: string, version: number, payload: Uint8Array): string;
export function decodeAddress(address: string): { prefix: string; version: number; payload: Uint8Array };
export const p2pkSpk: (pubkeyHex: string) => string;
export const p2shSpk: (hashHex: string) => string;
export function spkOfAddress(address: string): string;
export function addressOfSpk(prefix: string, spk: string): string | null;
export const addressPrefix: (network: string) => string;
