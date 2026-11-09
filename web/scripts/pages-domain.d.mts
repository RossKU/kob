/** The host of a public URL; null when it is not an https origin. */
export function hostOf(publicUrl: string): string | null;
/** The zone names a host may belong to, longest first. */
export function zoneCandidates(host: string): string[];
/** Attaches `host` to the Pages `project` and creates its DNS record when the token may; `lines` is markdown for the job summary. */
export function ensureDomain(opts: { token: string; account: string; project: string; host: string; fetchFn?: typeof fetch }): Promise<{ ok: boolean; lines: string[] }>;
/** The pages.dev host from the `subdomain` the Pages API reports for a project (`kob-web-bs6.pages.dev`); null when it is not one. */
export function pagesDevHost(subdomain: unknown): string | null;
