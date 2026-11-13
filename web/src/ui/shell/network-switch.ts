// The header's network switch (pure): whether this page offers one, and what choosing a network saves. The network is fixed at load, so a choice is
// saved like the Settings screen saves it (`settingsPatch(..., ['network'])`) and the page reloads (NetworkSwitch.tsx).
//
// The servers: config.ts `resolveConfig` gives each network the deployment's servers of THAT network (`networks`, or the top-level ones on
// `deploymentNetwork`) and none otherwise, so a switch never leaves the first network's servers in use. The choice is stored (`kob.settings`, per origin),
// so it beats the host's default network (`hostNetworks`) on this browser until Settings removes it. A server this BROWSER saved (Settings) cannot be told to belong to either network, and it would
// stay in force on the other one, so the switch refuses until it is removed in Settings: `blocked`.
import { readSettings, type AppConfig, type NetworkName } from '../../config';
import { formFromConfig, settingsPatch, storedServerFields, validateSettings } from '../settings/settings-model';

export const SWITCH_NETWORKS: readonly NetworkName[] = ['mainnet', 'testnet-10'];

/** The server settings a stored setting can hold (`storedServerFields` also lists the network, which a switch replaces itself). */
const STORED_SERVER_KEYS = ['indexerUrl', 'nodeUrl', 'registryUrl', 'extraIndexerUrls'] as const;

export type NetworkSwitchState =
  /** the deployment pins its network (`lockNetwork`): no switch */
  | { kind: 'pinned' }
  /** this browser saved servers (`fields`): they would stay in force on the other network; they are removed in Settings */
  | { kind: 'blocked'; fields: string[] }
  | { kind: 'open' };

/** The servers `stored` (the parsed `kob.settings`) holds. */
export const storedServers = (stored: unknown): string[] => storedServerFields(stored).filter((f) => (STORED_SERVER_KEYS as readonly string[]).includes(f));

export function networkSwitchState(config: Pick<AppConfig, 'lockNetwork'>, stored: unknown = readSettings()): NetworkSwitchState {
  if (config.lockNetwork) return { kind: 'pinned' };
  const fields = storedServers(stored);
  return fields.length ? { kind: 'blocked', fields } : { kind: 'open' };
}

/**
 * What choosing `target` hands to `saveSettings`: the network alone (the other fields of the form are not saved). Choosing the host's own default (`hostNetwork`) CLEARS the
 * stored network instead (`null`): the browser then follows the host again, so a later change of `hostNetworks` reaches it.
 */
export function networkSwitchPatch(config: AppConfig, target: NetworkName): Record<string, unknown> {
  if (target === config.hostNetwork) return { network: null };
  return settingsPatch(validateSettings({ ...formFromConfig(config), network: target }).clean, ['network']);
}

/** The other network of the two. */
export const otherNetwork = (network: string): NetworkName => (network === 'mainnet' ? 'testnet-10' : 'mainnet');
