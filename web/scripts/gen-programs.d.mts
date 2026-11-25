export const SOURCE_URL: URL;
export const OUTPUT_URL: URL;
/** crates/kob-protocol/data/topup_extra.json (bid-delivery-spec §9). */
export const TOPUP_EXTRA_URL: URL;
/** The reference top-up extras of bid-delivery-spec §9, used until topup_extra.json exists. */
export const TOPUP_EXTRA_REFERENCE: Readonly<Record<string, number>>;
/** The parsed topup_extra.json (program name -> sompi), or null when it does not exist. */
export function readTopupExtra(): Record<string, number | string | null> | null;
/** crates/kob-protocol/data/merge_costs.json (per program and program pair, sompi). */
export const MERGE_COSTS_URL: URL;
/** The parsed merge_costs.json. */
export function readMergeCosts(): unknown;
/**
 * Renders web/src/kob/programs.generated.ts from the parsed crates/kob-protocol/data/programs.json
 * (and the top-up extras, else the reference ones).
 */
export function render(json: unknown, topupExtra?: Record<string, number | string | null> | null, mergeCosts?: unknown): string;
