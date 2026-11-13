// Block-explorer links. The base URL follows the configured network: `config.explorerUrl`, else
// the kaspa.stream site of TN10 or mainnet.
import { explorerBase, explorerTxUrl, type AppConfig } from '../config';
import { useServices } from './context';

/** `(txid) => explorer URL | null` for the configured network. */
export function useTxUrl(): (txid: string | null | undefined) => string | null {
  const { config } = useServices();
  return (txid) => explorerTxUrl(config, txid);
}

/**
 * The explorer page of a wallet address (a `kaspa:` or `kaspatest:` style prefix plus the bech32
 * body), or null for anything else. The explorer is the network's, as for transactions.
 */
export function explorerAddressUrl(c: Pick<AppConfig, 'network' | 'explorerUrl'>, address: string | null | undefined): string | null {
  if (typeof address !== 'string' || !/^kaspa(test|dev|sim)?:[a-z0-9]{10,100}$/.test(address)) return null;
  return `${explorerBase(c)}/addresses/${address}`;
}

/** `(address) => explorer URL | null` for the configured network. */
export function useAddressUrl(): (address: string | null | undefined) => string | null {
  const { config } = useServices();
  return (address) => explorerAddressUrl(config, address);
}
