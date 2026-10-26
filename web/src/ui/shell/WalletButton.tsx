import { useWallet } from '../../app/context';
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { visibilityCheckSupported } from '../../signer/seen';
import { Badge, Button, CopyText } from '../kit';
import { isPhoneBrowser } from './platform';

/** The address in the chip as a wallet address reads: its network prefix, then the head and tail of the body ("kaspatest:qq776p…wutfav"). */
export const chipAddress = (address: string): string => {
  const colon = address.indexOf(':');
  const prefix = colon >= 0 ? address.slice(0, colon + 1) : '';
  const body = address.slice(colon + 1);
  return body.length <= 14 ? address : `${prefix}${body.slice(0, 6)}…${body.slice(-6)}`;
};

/** How the wallet is connected, for the chip's wallet icon (never a label of the address: "Private key" next to it reads as a key). */
const connectedWith = (id: string, label: string): string => t('shell.wallet.connectedVia', { wallet: id === 'local-key' ? t('shell.wallet.viaLocalKey') : label });

const KASWARE_URL = 'https://chromewebstore.google.com/detail/kasware-wallet/hklhheigdmpoolooomdihmhlpjjdbklf';
const KASPIRE_URL = 'https://github.com/KaspaHUB21/Kaspire-Kaspa-Wallet';

/**
 * Wallet area of the header. Not connected: one connect button per DETECTED wallet (extensions inject late, the list updates by itself),
 * or an install hint while none is found. Connected: one compact chip with the address (shortened, copyable) as its text, a small wallet icon
 * whose tooltip says how it is connected, the wallet's network ONLY when it differs from the app's (the header's network badge already names the
 * app's), and a disconnect button (an icon on a phone).
 */
export function WalletButton() {
  const { config } = useServices();
  const w = useWallet();

  if (w.info && w.adapter) {
    return (
      <div class="wallet-chip" data-testid="wallet-connected" data-network={w.info.network} data-wallet={w.info.id}>
        <span class="small">
          <CopyText value={w.info.address} text={chipAddress(w.info.address)} data-testid="wallet-address" label={t('shell.wallet.address')} />
        </span>
        <span class="wallet-kind muted" data-testid="wallet-kind" role="img" aria-label={connectedWith(w.info.id, w.info.label)} title={connectedWith(w.info.id, w.info.label)}>
          <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M3 7h15a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2zM3 7l12-3v3M16 13.5h.01" />
          </svg>
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

  // the private key is typed into the sandboxed signer frame (SignerFrame.tsx), never into this page. Where the browser cannot run its visibility
  // check the option is disabled and says why in text (a tooltip does not exist on a touch screen)
  const supported = visibilityCheckSupported();
  const phone = isPhoneBrowser();
  const privateKey = (
    <>
      <Button
        small
        variant="ghost"
        data-testid="wallet-connect-local-key"
        disabled={w.connecting || !supported}
        onClick={w.openPrivateKey}
        title={supported ? t('signer.recommend') : t('signer.unsupported')}
      >
        {t('signer.option')}
      </Button>
      {supported ? null : (
        <p class="small muted wallet-note" data-testid="wallet-key-unsupported">
          {t(phone ? 'signer.unsupportedWhyPhone' : 'signer.unsupportedWhy')}
        </p>
      )}
    </>
  );

  if (w.detected.length === 0) {
    return (
      <div class="row">
        {/* a phone browser has no extension to install: the install links and "in this browser" would mislead */}
        <div class="small muted" data-testid="wallet-install-hint" {...(phone ? {} : { title: t('shell.wallet.none') })}>
          <strong>{t('shell.wallet.noneTitle')}</strong>{' '}
          {phone ? (
            <span data-testid="wallet-install-phone">{t('shell.wallet.nonePhone')}</span>
          ) : (
            <>
              <a href={KASWARE_URL} target="_blank" rel="noopener noreferrer">{t('shell.wallet.installKasware')}</a>
              {' / '}
              <a href={KASPIRE_URL} target="_blank" rel="noopener noreferrer">{t('shell.wallet.installKaspire')}</a>
            </>
          )}
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
