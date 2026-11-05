// kUSDT on testnet-10, a federated-bridge token (contracts/third-party/kusdt/UPSTREAM.md): its genesis group as kascov.io's per-token file and the
// Kaspa REST explorer's transaction would serve it, built from the chain bytes of contracts/third-party/kusdt/observed.json (the genesis outputs, the
// BridgeMint redeem script its first spend revealed). Output 0 is the BridgeMint (spent, revealed), output 1 the unspent TokenSeed nobody revealed (the
// check rebuilds it from the creator's key, which only the explorer's transaction names), output 2 the creator's change. The covenant id commits to all
// of it, so a check that passes on these documents passes on the chain's.
import { readFileSync } from 'node:fs';

interface Observed {
  token: { ticker: string; covenant_id: string };
  genesis: {
    txid: string;
    authorizing_outpoint: { txid: string; index: number };
    outputs: { index: number; value: number; script_public_key: string; covenant: { authorizing_input: number; covenant_id: string } | null }[];
  };
  bridge_mint: { revealed_by: { txid: string; input: number }; redeem_script: string };
}

export interface KusdtRecording {
  covenantId: string;
  ticker: string;
  genesisTxid: string;
  /** kascov.io's `<data>/testnet-10/c/<covenant id>.json` */
  kascov: Record<string, unknown>;
  /** the explorer's `/transactions/<genesis txid>` */
  explorerTx: Record<string, unknown>;
}

export function kusdtRecording(): KusdtRecording {
  const o = JSON.parse(readFileSync(new URL('../../../contracts/third-party/kusdt/observed.json', import.meta.url), 'utf8')) as Observed;
  const g = o.genesis;
  const cov = o.token.covenant_id;
  const group = g.outputs.filter((x) => x.covenant?.covenant_id === cov);
  return {
    covenantId: cov,
    ticker: o.token.ticker,
    genesisTxid: g.txid,
    kascov: {
      covenant_id: cov,
      network: 'testnet-10',
      genesis_txid: g.txid,
      kcc23: { binding: 'verified', preparation_txid: g.authorizing_outpoint.txid, output_index: g.authorizing_outpoint.index },
      utxos: group.map((x) =>
        x.index === 0
          ? { outpoint: `${g.txid}:0`, value: x.value, script_hex: x.script_public_key, live: false, revealed_hex: o.bridge_mint.redeem_script, spent_txid: o.bridge_mint.revealed_by.txid }
          : { outpoint: `${g.txid}:${x.index}`, value: x.value, script_hex: x.script_public_key, live: true },
      ),
      utxos_truncated: false,
      reveal_bodies_omitted: 0,
    },
    explorerTx: {
      transaction_id: g.txid,
      inputs: [{ transaction_id: g.txid, index: 0, previous_outpoint_hash: g.authorizing_outpoint.txid, previous_outpoint_index: String(g.authorizing_outpoint.index) }],
      outputs: g.outputs.map((x) => ({
        transaction_id: g.txid,
        index: x.index,
        amount: x.value,
        script_public_key: x.script_public_key,
        script_public_key_address: null,
        covenant_authorizing_input: x.covenant?.authorizing_input ?? null,
        covenant_id: x.covenant?.covenant_id ?? null,
      })),
    },
  };
}
