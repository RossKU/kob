import { useState } from 'preact/hooks';
import { useWallet } from '../../app/context';
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { visibilityCheckSupported } from '../../signer/seen';
import { Badge, Button, CopyText, Modal } from '../kit';
import { isPhoneBrowser } from './platform';
import { useNarrow } from './narrow';

/** The address in the chip as a wallet address reads: its network prefix, then the head and tail of the body ("kaspatest:qq776p…wutfav"). */
export const chipAddress = (address: string, narrow = false): string => {
  const colon = address.indexOf(':');
  const prefix = colon >= 0 ? address.slice(0, colon + 1) : '';
  const body = address.slice(colon + 1);
  // a phone: the head and the tail of the body alone ("qq77…tfav"), the network is the chip's word
  if (narrow) return body.length <= 10 ? body : `${body.slice(0, 4)}…${body.slice(-4)}`;
  return body.length <= 14 ? address : `${prefix}${body.slice(0, 6)}…${body.slice(-6)}`;
};

/** How the wallet is connected, for the chip's wallet icon (never a label of the address: "Private key" next to it reads as a key). */
const connectedWith = (id: string, label: string): string => t('shell.wallet.connectedVia', { wallet: id === 'local-key' ? t('shell.wallet.viaLocalKey') : label });

const KASWARE_URL = 'https://chromewebstore.google.com/detail/kasware-wallet/hklhheigdmpoolooomdihmhlpjjdbklf';
const KASPIRE_URL = 'https://github.com/KaspaHUB21/Kaspire-Kaspa-Wallet';

/**
 * Wallet area of the header. Not connected: two buttons, "Connect wallet" and "Private key" (ConnectBar). Connected: one compact chip with the address
 * (shortened, copyable) as its text, a small wallet icon whose tooltip says how it is connected, the wallet's network ONLY when it differs from the app's
 * (the header's network badge already names the app's), and a disconnect button (an icon on a phone).
 */
export function WalletButton() {
  const w = useWallet();
  const narrow = useNarrow();

  if (w.info && w.adapter) {
    return (
      <div class="wallet-chip" data-testid="wallet-connected" data-network={w.info.network} data-wallet={w.info.id}>
        <span class="small">
          <CopyText value={w.info.address} text={chipAddress(w.info.address, narrow)} data-testid="wallet-address" label={t('shell.wallet.address')} />
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

  return <ConnectBar />;
}

/**
 * Not connected. A desktop: two buttons of the same size on one line, "Connect wallet" and "Private key". A phone (narrow): ONE button, "Connect", that opens a
 * sheet with "Wallet" and "Private key" and the explanation of what is missing. Nothing else stays in the bar. One detected extension wallet connects at once
 * from "Connect wallet" (a phone: from the sheet); several are listed in a dialog; none says what is missing, with the install links (on a phone: extension
 * wallets do not run in phone browsers, use Private key). Where the browser cannot run the private key signer the button stays, looks off, and says why when
 * pressed. The private key is typed into the sandboxed signer frame (SignerFrame.tsx), never into this page.
 */
function ConnectBar() {
  const { config } = useServices();
  const w = useWallet();
  const narrow = useNarrow();
  const [dialog, setDialog] = useState<'wallets' | 'none' | 'key' | 'sheet' | null>(null);
  const supported = visibilityCheckSupported();
  const phone = isPhoneBrowser();
  const only = w.detected.length === 1 ? w.detected[0]! : null;

  const connectOne = (id: Parameters<typeof w.connect>[0]) => void w.connect(id).then(() => setDialog(null));
  const onConnect = () => {
    if (narrow) setDialog('sheet');
    else if (only) void w.connect(only.id);
    else setDialog(w.detected.length > 1 ? 'wallets' : 'none');
  };
  const onKey = () => {
    if (supported) {
      setDialog(null);
      w.openPrivateKey();
    } else setDialog('key');
  };
  // an extension that appears while the "none" dialog is open (they inject late) takes its place
  const shown = dialog === 'none' && w.detected.length > 0 ? 'wallets' : dialog;
  const installHint = (
    <div class="stack-sm" data-testid="wallet-install-hint">
      {/* a phone browser has no extension to install: the install links and "in this browser" would mislead */}
      {phone ? (
        <p data-testid="wallet-install-phone">{t('shell.wallet.nonePhone')}</p>
      ) : (
        <>
          <p>{t('shell.wallet.none')}</p>
          <p>
            <a href={KASWARE_URL} target="_blank" rel="noopener noreferrer">{t('shell.wallet.installKasware')}</a>
            {' / '}
            <a href={KASPIRE_URL} target="_blank" rel="noopener noreferrer">{t('shell.wallet.installKaspire')}</a>
          </p>
        </>
      )}
    </div>
  );
  const keyButton = (
    <Button
      small={!narrow}
      block={narrow}
      data-testid="wallet-connect-local-key"
      disabled={w.connecting || (narrow && !supported)}
      data-state={supported ? undefined : 'unavailable'}
      class={supported ? undefined : 'btn-off'}
      onClick={onKey}
      title={supported ? t('signer.recommend') : t('signer.unsupported')}
    >
      {t('signer.option')}
    </Button>
  );

  return (
    <div class="wallet-actions" data-testid="wallet-connect-list" data-network={config.network}>
      <Button
        small
        variant="primary"
        data-testid={!narrow && only ? `wallet-connect-${only.id}` : 'wallet-connect'}
        loading={w.connecting && !narrow}
        onClick={onConnect}
      >
        {w.connecting && !narrow ? t('shell.wallet.connecting') : t(narrow ? 'shell.wallet.connectShort' : 'shell.wallet.connectButton')}
      </Button>
      {narrow ? null : keyButton}
      {shown === 'sheet' ? (
        <Modal title={t('shell.wallet.sheetTitle')} onClose={() => setDialog(null)} data-testid="wallet-sheet">
          <div class="stack-sm">
            {w.detected.length === 0 ? (
              <Button variant="primary" block disabled data-testid="wallet-connect-none">{t('shell.wallet.optionWallet')}</Button>
            ) : (
              w.detected.map((a) => (
                <Button key={a.id} variant="primary" block data-testid={`wallet-connect-${a.id}`} loading={w.connecting} onClick={() => connectOne(a.id)}>
                  {w.connecting ? t('shell.wallet.connecting') : only ? t('shell.wallet.optionWallet') : t('shell.wallet.connect', { wallet: a.label })}
                </Button>
              ))
            )}
            {keyButton}
            {w.detected.length === 0 ? <div class="small muted">{installHint}</div> : null}
            {supported ? null : <p class="small muted" data-testid="wallet-key-unsupported">{t(phone ? 'signer.unsupportedWhyPhone' : 'signer.unsupportedWhy')}</p>}
          </div>
        </Modal>
      ) : null}
      {shown === 'wallets' ? (
        <Modal title={t('shell.wallet.chooseTitle')} onClose={() => setDialog(null)} data-testid="wallet-dialog">
          <div class="stack-sm" data-testid="wallet-choose">
            {w.detected.map((a) => (
              <Button key={a.id} variant="primary" block data-testid={`wallet-connect-${a.id}`} loading={w.connecting} onClick={() => connectOne(a.id)}>
                {w.connecting ? t('shell.wallet.connecting') : t('shell.wallet.connect', { wallet: a.label })}
              </Button>
            ))}
          </div>
        </Modal>
      ) : null}
      {shown === 'none' ? (
        <Modal title={t('shell.wallet.noneTitle')} onClose={() => setDialog(null)} data-testid="wallet-dialog">
          <div class="stack-sm">
            {installHint}
            {supported ? <p class="small muted">{t('shell.wallet.noneOrKey')}</p> : null}
          </div>
        </Modal>
      ) : null}
      {shown === 'key' ? (
        <Modal title={t('signer.unsupportedTitle')} onClose={() => setDialog(null)} data-testid="wallet-dialog">
          <p data-testid="wallet-key-unsupported">{t(phone ? 'signer.unsupportedWhyPhone' : 'signer.unsupportedWhy')}</p>
        </Modal>
      ) : null}
    </div>
  );
}
