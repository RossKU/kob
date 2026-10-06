// Settings form <-> stored settings (pure). The stored layer is validated again by config.ts (`saveSettings`), so this module only decides what
// the form shows, which fields are invalid, what changed, and whether a change points the app at other servers.
import { parseNetwork, validateRegistryUrl, validateUrl, type AppConfig, type NetworkName } from '../../config';

export interface SettingsForm {
  network: NetworkName;
  indexerUrl: string;
  nodeUrl: string;
  registryUrl: string;
  kastle: boolean;
  priorityFee: boolean;
  /** `fees.dynamic`: pay the node's current fee estimate per action instead of the relay minimum */
  dynamicFee: boolean;
}

export type SettingsField = 'network' | 'indexerUrl' | 'nodeUrl' | 'registryUrl';
export type SettingsErrorCode = 'network' | 'url' | 'registry';

export const formFromConfig = (c: AppConfig): SettingsForm => ({
  network: c.network,
  indexerUrl: c.indexerUrl,
  nodeUrl: c.nodeUrl,
  registryUrl: c.registryUrl,
  kastle: c.features.kastle,
  priorityFee: c.features.priorityFee,
  dynamicFee: c.fees.dynamic,
});

export interface SettingsValidation {
  ok: boolean;
  errors: Partial<Record<SettingsField, SettingsErrorCode>>;
  /** normalised values (trimmed URLs without a trailing slash); only meaningful when `ok` */
  clean: SettingsForm;
}

/** URL fields accept ws / wss / http / https (a node) or http / https (indexer, registry); '' clears the indexer and the node (SDK resolver). */
export function validateSettings(f: SettingsForm): SettingsValidation {
  const errors: SettingsValidation['errors'] = {};
  const network = parseNetwork(f.network);
  if (!network) errors.network = 'network';
  const indexer = validateUrl(f.indexerUrl, ['http:', 'https:']);
  if (indexer === null) errors.indexerUrl = 'url';
  const node = validateUrl(f.nodeUrl);
  if (node === null) errors.nodeUrl = 'url';
  const registry = validateRegistryUrl(f.registryUrl);
  if (registry === null) errors.registryUrl = 'registry';
  return {
    ok: Object.keys(errors).length === 0,
    errors,
    clean: { network: network ?? f.network, indexerUrl: indexer ?? f.indexerUrl, nodeUrl: node ?? f.nodeUrl, registryUrl: registry ?? f.registryUrl, kastle: f.kastle, priorityFee: f.priorityFee, dynamicFee: f.dynamicFee },
  };
}

/** Fields whose value differs from the running configuration (compared after normalisation). */
export function changedFields(current: AppConfig, form: SettingsForm): (SettingsField | 'kastle' | 'priorityFee' | 'dynamicFee')[] {
  const v = validateSettings(form).clean;
  const out: (SettingsField | 'kastle' | 'priorityFee' | 'dynamicFee')[] = [];
  if (v.network !== current.network) out.push('network');
  if (v.indexerUrl !== current.indexerUrl) out.push('indexerUrl');
  if (v.nodeUrl !== current.nodeUrl) out.push('nodeUrl');
  if (v.registryUrl !== current.registryUrl) out.push('registryUrl');
  if (v.kastle !== current.features.kastle) out.push('kastle');
  if (v.priorityFee !== current.features.priorityFee) out.push('priorityFee');
  if (v.dynamicFee !== current.fees.dynamic) out.push('dynamicFee');
  return out;
}

/** A change of any server the app talks to (needs the "wrong data" acknowledgement before saving). */
export const changesServers = (fields: readonly string[]): boolean => fields.some((f) => f === 'indexerUrl' || f === 'nodeUrl' || f === 'registryUrl');

/** The object handed to `saveSettings` (network + URLs + Kastle, priority-fee and dynamic-fee flags; `features.test` can never be stored). */
export function settingsPatch(clean: SettingsForm): Record<string, unknown> {
  return { network: clean.network, indexerUrl: clean.indexerUrl, nodeUrl: clean.nodeUrl, registryUrl: clean.registryUrl, features: { kastle: clean.kastle, priorityFee: clean.priorityFee }, fees: { dynamic: clean.dynamicFee } };
}

// ------------------------------------------------------------------------------------------------ local data

/** Storage with enumerable keys (the browser's localStorage). */
export interface EnumerableStorage {
  readonly length: number;
  key(index: number): string | null;
  removeItem(key: string): void;
}

/** Every key this app owns (`kob.` prefix: settings, placement records, tracked tokens). */
export function kobStorageKeys(storage: Pick<EnumerableStorage, 'length' | 'key'>): string[] {
  const keys: string[] = [];
  for (let i = 0; i < storage.length; i++) {
    const k = storage.key(i);
    if (k !== null && k.startsWith('kob.')) keys.push(k);
  }
  return keys;
}

/** Removes every app key; returns them. Other sites' data (there is none on this origin) and unrelated keys are left alone. */
export function clearKobStorage(storage: EnumerableStorage): string[] {
  const keys = kobStorageKeys(storage);
  for (const k of keys) storage.removeItem(k);
  return keys;
}

/** What a key holds, for the "what will be deleted" list. */
export function describeStorageKey(key: string): 'settings' | 'records' | 'tokens' | 'other' {
  if (key === 'kob.settings') return 'settings';
  if (key.startsWith('kob.records.')) return 'records';
  if (key.startsWith('kob.tokens.')) return 'tokens';
  return 'other';
}
