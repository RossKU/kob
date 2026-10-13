import { useWallet } from '../../app/context';
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { visibilityCheckSupported } from '../../signer/seen';
import { Badge, Button, CopyText } from '../kit';

/** The address in the chip: without the network prefix (the header names the network), the head and tail of the body; the full one is in its title. */
export const chipAddress = (address: string): string => {
  const body = address.slice(address.indexOf(':') + 1);
  return body.length <= 14 ? body : `${body.slice(0, 6)}…${body.slice(-6)}`;
};

const KASWARE_URL = 'https://chromewebstore.google.com/detail/kasware-wallet/hklhheigdmpoolooomdihmhlpjjdbklf';
const KASPIRE_URL = 'https://github.com/KaspaHUB21/Kaspire-Kaspa-Wallet';

/**
 * Wallet area of the header. Not connected: one connect button per DETECTED wallet (extensions inject late, the list updates by itself),
 * or an install hint while none is found. Connected: one compact chip with the wallet's name (wide screens), its address (shortened,
 * copyable), the wallet's network ONLY when it differs from the app's (the header's network badge already names the app's), and a disconnect button
 * (an icon on a phone).
 */
export function WalletButton() {
  const { config } = useServices();
  const w = useWallet();

  if (w.info && w.adapter) {
    return (
      <div class="wallet-chip" data-testid="wallet-connected" data-network={w.info.network} title={t('shell.wallet.chip', { wallet: w.info.label, network: w.info.network })}>
        <span class="small muted wallet-name">{w.info.label}</span>
        <span class="small">
          <CopyText value={w.info.address} text={chipAddress(w.info.address)} data-testid="wallet-address" label={t('shell.wallet.address')} />
        </span>
        {w.networkMismatch ? (
          <Badge tone="bad" data-testid="wallet-network" title={t('shell.wallet.networkOk', { network: w.info.network })}>
            {w.info.network}
          </Badge>
        ) : null}
        <Button small variant="ghost" class="wallet-disconnect" data-testid="wallet-disconnect" onClick={w.disconnect} aria-label={t('shell.wallet.disconnect')} title={t('shell.wallet.disconnect')}>
          <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M15 4h4v16h-4M10 8l-4 4 4 4M6 12h10" />
          </svg>
          <span class="wallet-disconnect-text">{t('shell.wallet.disconnect')}</span>
        </Button>
      </div>
    );
  }

  // the private key is typed into the sandboxed signer frame (SignerFrame.tsx), never into this page
  const privateKey = (
    <Button
      small
      variant="ghost"
      data-testid="wallet-connect-local-key"
      disabled={w.connecting || !visibilityCheckSupported()}
      onClick={w.openPrivateKey}
      title={visibilityCheckSupported() ? t('signer.recommend') : t('signer.unsupported')}
    >
      {t('signer.option')}
    </Button>
  );

  if (w.detected.length === 0) {
    return (
      <div class="row">
        <div class="small muted" data-testid="wallet-install-hint" title={t('shell.wallet.none')}>
          <strong>{t('shell.wallet.noneTitle')}</strong>{' '}
          <a href={KASWARE_URL} target="_blank" rel="noopener noreferrer">{t('shell.wallet.installKasware')}</a>
          {' / '}
          <a href={KASPIRE_URL} target="_blank" rel="noopener noreferrer">{t('shell.wallet.installKaspire')}</a>
        </div>
        {privateKey}
      </div>
    );
  }

  return (
    <div class="row" data-testid="wallet-connect-list" data-network={config.network}>
      {w.detected.map((a) => (
        <Button key={a.id} small variant="primary" data-testid={`wallet-connect-${a.id}`} loading={w.connecting} onClick={() => void w.connect(a.id)}>
          {w.connecting ? t('shell.wallet.connecting') : t('shell.wallet.connect', { wallet: a.label })}
        </Button>
      ))}
      {privateKey}
    </div>
  );
}
