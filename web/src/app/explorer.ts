// Block-explorer links for transactions: the base URL follows the configured network (`config.explorerUrl`, else kaspa.stream's TN10 / mainnet site).
import { explorerTxUrl } from '../config';
import { useServices } from './context';

/** `(txid) => explorer URL | null` for the configured network; a link opens in a new tab (`rel="noopener noreferrer"`). */
export function useTxUrl(): (txid: string | null | undefined) => string | null {
  const { config } = useServices();
  return (txid) => explorerTxUrl(config, txid);
}
