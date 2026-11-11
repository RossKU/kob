// Block-explorer links for transactions: the base URL follows the configured network (`config.explorerUrl`, else kaspa.stream's TN10 / mainnet site).
import { explorerBase, explorerTxUrl, type AppConfig } from '../config';
import { useServices } from './context';

/** `(txid) => explorer URL | null` for the configured network; a link opens in a new tab (`rel="noopener noreferrer"`). */
export function useTxUrl(): (txid: string | null | undefined) => string | null {
  const { config } = useServices();
  return (txid) => explorerTxUrl(config, txid);
}

/** The explorer page of a wallet address (`kaspa:` / `kaspatest:` plus the bech32 body), or null for anything else. The explorer is the network's, as for transactions. */
export function explorerAddressUrl(c: Pick<AppConfig, 'network' | 'explorerUrl'>, address: string | null | undefined): string | null {
  if (typeof address !== 'string' || !/^kaspa(test|dev|sim)?:[a-z0-9]{10,100}$/.test(address)) return null;
  return `${explorerBase(c)}/addresses/${address}`;
}

/** `(address) => explorer URL | null` for the configured network. */
export function useAddressUrl(): (address: string | null | undefined) => string | null {
  const { config } = useServices();
  return (address) => explorerAddressUrl(config, address);
}
