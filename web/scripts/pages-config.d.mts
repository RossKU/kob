export const NETWORKS: string[];
export const MAX_EXTRA_INDEXERS: number;
export interface PagesConfig { network: string; indexerUrl: string; extraIndexerUrls: string[]; nodeUrl: string; explorerUrl?: string; tokenNamesUrl?: string }
/** True for a host name that is not reachable from the public internet (loopback, private, link-local, unspecified). */
export function isPrivateHost(hostname: string): boolean;
/** A public URL of one of `schemes`, trailing slashes removed; or an error message. */
export function publicUrl(name: string, value: unknown, schemes: string[]): { url?: string; error?: string };
/** The deployment's config.json from the KOB_PAGES_* variables; `config` is null when anything is wrong. */
export function pagesConfig(env: Record<string, string | undefined>): { config: PagesConfig | null; errors: string[]; notes: string[] };
/** The shipped registry's programs for `network`, with no token entries. */
export function programRegistry(registryText: string, network: string): string;
/** Writes config.json (and the testnet-10 program list) into `dist`. */
export function writeDeployment(dist: string, config: PagesConfig, registryPath?: string): { path: string; sha256: string }[];
