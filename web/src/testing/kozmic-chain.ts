// Kozmic tokens on Kaspa mainnet, issued by a ZK bridge (contracts/third-party/kozmic/UPSTREAM.md): each genesis group as kascov.io's per-token file serves
// it, built from the chain bytes of contracts/third-party/kozmic/observed.json (the one genesis output, the BridgeMint redeem script its first spend revealed,
// the KCC-23 binding that names the authorising outpoint). The covenant id commits to all of it, so a check that passes on these documents passes on the
// chain's. `kzusdc` is kzUSDC cc9957fe..., `kzusdc_instance_1` the older instance 9753d77b..., `sepia` the Sepolia rehearsal Sepia.k 13af6019...: its script
// was left out of the file the fixture was read from, so it is rebuilt from kzUSDC's bytes and its bridge covenant id (the output's hash proves it), as a
// source that kept it would serve it. The documents are served under the app's network: the bytes do not depend on it.
import { readFileSync } from 'node:fs';

interface Deployment {
  key: string;
  token: { covenant_id: string; ticker: string; declared_bridge_covenant_id: string };
  genesis: { txid: string; authorizing_outpoint: { txid: string; index: number }; group: { index: number; value: number; script_public_key: string }[] };
  bridge_mint: { output: number; revealed_by: string; redeem_script: string | null };
}

export type KozmicKey = 'kzusdc' | 'kzusdc_instance_1' | 'sepia';

export interface KozmicRecording {
  covenantId: string;
  ticker: string;
  genesisTxid: string;
  /** the bridge covenant the BridgeMint's state names */
  bridgeCovenantId: string;
  /** kascov.io's `<data>/<network>/c/<covenant id>.json` */
  kascov: Record<string, unknown>;
}

export function kozmicRecording(key: KozmicKey = 'kzusdc', network = 'testnet-10'): KozmicRecording {
  const all = JSON.parse(readFileSync(new URL('../../../contracts/third-party/kozmic/observed.json', import.meta.url), 'utf8')) as { deployments: Deployment[] };
  const d = all.deployments.find((x) => x.key === key)!;
  const base = all.deployments.find((x) => x.key === 'kzusdc')!.bridge_mint.redeem_script!;
  // the bridge covenant id is the second field of the 99-byte state after the 34-byte prefix (bytes 68..100)
  const redeem = d.bridge_mint.redeem_script ?? base.slice(0, 136) + d.token.declared_bridge_covenant_id + base.slice(200);
  const g = d.genesis;
  return {
    covenantId: d.token.covenant_id,
    ticker: d.token.ticker,
    genesisTxid: g.txid,
    bridgeCovenantId: d.token.declared_bridge_covenant_id,
    kascov: {
      covenant_id: d.token.covenant_id,
      network,
      genesis_txid: g.txid,
      kcc23: { binding: 'verified', preparation_txid: g.authorizing_outpoint.txid, output_index: g.authorizing_outpoint.index },
      utxos: g.group.map((x) => ({
        outpoint: `${g.txid}:${x.index}`,
        value: x.value,
        script_hex: x.script_public_key,
        live: false,
        ...(x.index === d.bridge_mint.output ? { revealed_hex: redeem, spent_txid: d.bridge_mint.revealed_by } : {}),
      })),
      utxos_truncated: false,
      reveal_bodies_omitted: 0,
    },
  };
}
