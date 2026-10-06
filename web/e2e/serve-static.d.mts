export interface StaticMount { prefix: string; dir: string }
export const DEFAULT_MOUNTS: StaticMount[];
export function resolveStatic(urlPath: string, mounts?: StaticMount[]): string | null;
export function startStaticServer(opts?: { port?: number; host?: string; mounts?: StaticMount[] }): Promise<{ url: string; port: number; close(): Promise<void> }>;
