/** The host of a public URL; null when it is not an https origin. */
export function hostOf(publicUrl: string): string | null;
/** The zone names a host may belong to, longest first. */
export function zoneCandidates(host: string): string[];
/** Attaches `host` to the Pages `project` and creates its DNS record when the token may; `lines` is markdown for the job summary. */
export function ensureDomain(opts: { token: string; account: string; project: string; host: string; fetchFn?: typeof fetch }): Promise<{ ok: boolean; lines: string[] }>;
