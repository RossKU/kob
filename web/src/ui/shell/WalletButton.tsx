import { useEffect, useRef, useState } from 'preact/hooks';
import { useWallet } from '../../app/context';
import { useServices } from '../../app/context';
import { useAddressUrl } from '../../app/explorer';
import { t } from '../../i18n';
import { visibilityCheckSupported } from '../../signer/seen';
import { privateKeySigningAllowed } from '../../app/mirror';
import { Badge, Button, Modal } from '../kit';
import { copyToClipboard } from '../kit/CopyText';
import { shortAddress, shortId } from '../kit/format';
import { Popover } from './Popover';
import { installable } from './wallet-model';
import { isPhoneBrowser } from './platform';
import { useNarrow } from './narrow';

/**
 * The address in the chip: its network prefix, then the start of the body and an ellipsis ("kaspatest:qq776pwutf…"), never cut in the
 * middle. A phone shows the start of the body alone ("qq776pwu…"); the network is the chip's word.
 */
export const chipAddress = (address: string, narrow = false): string => {
  const colon = address.indexOf(':');
  const body = address.slice(colon + 1);
  return narrow ? shortId(body, 8) : shortAddress(address, 12);
};

/** How the wallet is connected, for the chip's wallet icon (never a label of the address: "Private key" next to it reads as a key). */
const connectedWith = (id: string, label: string): string => t('shell.wallet.connectedVia', { wallet: id === 'local-key' ? t('shell.wallet.viaLocalKey') : label });

/** The mark of a wallet in the chooser: its initial on a tile (a key for the private key entry); a colour of its own keeps the rows apart at a glance. */
const MARK_TONES: Record<string, string> = { kasware: '#2c9e77', kaspire: '#7a5af8', kastle: '#c2410c' };
function WalletMark(props: { id: string; label: string }) {
  if (props.id === 'local-key') {
    return (
      <svg class="wallet-mark" viewBox="0 0 24 24" width="22" height="22" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <circle cx="8" cy="15" r="4" />
        <path d="M11 12l9-9M16 7l3 3" />
      </svg>
    );
  }
  return (
    // the letter is drawn by CSS (data-initial): it is a picture, not text, so it is not part of the row's name
    <span class="wallet-mark wallet-mark-tile" data-initial={props.label.slice(0, 1)} style={{ background: MARK_TONES[props.id] ?? 'var(--surface-3)' }} aria-hidden="true" />
  );
}

/** The chevron of the wallet chip: it is a menu. */
function Caret() {
  return (
    <svg class="wallet-caret" viewBox="0 0 24 24" width="12" height="12" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
      <path d="M6 9l6 6 6-6" />
    </svg>
  );
}

/**
 * Wallet area of the header. Not connected: one "Connect" button (ConnectBar). Connected: ONE chip with the address (shortened) as its text, a small wallet icon
 * whose tooltip says how it is connected, and the wallet's network ONLY when it differs from the app's (the header's network chip already names the app's). A
 * press opens a small menu under it (review L-06 / M-14): the full address and how it is connected, Copy address, the address on the block explorer, Disconnect.
 */
export function WalletButton() {
  const w = useWallet();
  const narrow = useNarrow();
  const addressUrl = useAddressUrl();
  const [copied, setCopied] = useState(false);
  // the "Copied" cue ends by itself; the timer is cleared with the chip
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => void (timer.current && clearTimeout(timer.current)), []);

  if (w.info && w.adapter) {
    const { address, id, label, network } = w.info;
    const via = connectedWith(id, label);
    const explorer = addressUrl(address);
    const copy = async () => {
      const ok = await copyToClipboard(address);
      setCopied(ok);
      if (timer.current) clearTimeout(timer.current);
      if (ok) timer.current = setTimeout(() => setCopied(false), 1500);
    };
    return (
      <Popover
        label={t('shell.wallet.menu')}
        anchor={narrow ? 'header' : 'self'}
        triggerClass="wallet-chip"
        triggerAttrs={{ 'data-testid': 'wallet-menu', 'aria-haspopup': 'true' }}
        wrapAttrs={{ 'data-testid': 'wallet-connected', 'data-network': network, 'data-wallet': id }}
        panelTestId="wallet-menu-panel"
        closeOnLink
        trigger={
          <>
            <span class="wallet-kind muted" data-testid="wallet-kind" role="img" aria-label={via} title={via}>
              <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M3 7h15a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2zM3 7l12-3v3M16 13.5h.01" />
              </svg>
            </span>
            <code class="mono small wallet-address" data-testid="wallet-address" data-value={address} title={address}>{chipAddress(address, narrow)}</code>
            {w.networkMismatch ? (
              <Badge tone="bad" data-testid="wallet-network" title={t('shell.wallet.networkOk', { network })}>
                {network}
              </Badge>
            ) : null}
            <Caret />
          </>
        }
      >
        <div class="stack-xs">
          <div class="small muted" data-testid="wallet-menu-via">{via}</div>
          <code class="mono small wrap-anywhere" data-testid="wallet-menu-address" aria-label={t('shell.wallet.address')}>{address}</code>
        </div>
        <div class="menu-actions">
          <button type="button" class="menu-item" data-testid="wallet-copy" onClick={copy}>
            {copied ? t('common.copied') : t('shell.wallet.copyAddress')}
          </button>
          {explorer ? (
            <a class="menu-item" href={explorer} target="_blank" rel="noopener noreferrer" data-testid="wallet-explorer">
              {t('shell.wallet.explorer')}
            </a>
          ) : null}
          <button type="button" class="menu-item menu-item-danger" data-testid="wallet-disconnect" onClick={w.disconnect}>
            {t('shell.wallet.disconnect')}
          </button>
        </div>
      </Popover>
    );
  }

  return <ConnectBar />;
}

/**
 * Not connected: ONE button, "Connect", on every width, and no menu in front of it.
 *  - an extension wallet detected (KasWare, Kaspire): a minimal pick (a dialog with one button each) between the extensions and the private key
 *    entry, the extensions first (founder 2026-11-11: the key entry itself recommends an extension); where the browser cannot run the signer, one
 *    extension connects at once and two or more are picked between;
 *  - none (every phone browser): it opens the private key entry, the sandboxed signer frame (SignerFrame.tsx), straight away, with its one line of caution
 *    there. There is no "no wallet detected" step. Where the browser cannot run the signer (it cannot check what the signer shows) the button says why.
 * The separate "Private key" button of the header is gone; on a desktop the private key mode is reachable from Settings too. The key is typed into the
 * signer frame, never into this page.
 */
export function ConnectBar(props: { gate?: boolean } = {}) {
  const { config } = useServices();
  const w = useWallet();
  const narrow = useNarrow();
  const [dialog, setDialog] = useState<'wallets' | 'key' | null>(null);
  // the GitHub Pages mirror offers no private key (app/mirror.ts): Connect without an extension says why, as where the browser cannot run the signer
  const keyAllowed = privateKeySigningAllowed();
  const supported = keyAllowed && visibilityCheckSupported();
  const phone = isPhoneBrowser();
  // the private key is an option beside the extensions wherever the signer runs: then even one extension is picked, not connected at once
  const only = w.detected.length === 1 && !supported ? w.detected[0]! : null;
  const choose = w.detected.length > 1 || (w.detected.length > 0 && supported);
  const noExtension = w.detected.length === 0;

  const connectOne = (id: Parameters<typeof w.connect>[0]) => void w.connect(id).then(() => setDialog(null));
  // a click before the first look for extensions is over (they inject a moment after the page loads) waits for it, then decides: "no wallet" is never
  // taken from a list that has not been filled yet, so a user who has an extension is not sent to the private key entry
  const [waiting, setWaiting] = useState(false);
  const decide = () => {
    if (only) void w.connect(only.id);
    else if (choose) setDialog('wallets');
    else if (supported) w.openPrivateKey();
    else setDialog('key');
  };
  const onConnect = () => {
    if (!w.discovered) setWaiting(true);
    else decide();
  };
  useEffect(() => {
    if (!waiting || !w.discovered) return;
    setWaiting(false);
    decide();
  }, [waiting, w.discovered, w.detected]);
  // no extension: the button opens the private key entry, and is named for it (the tests and the page's own scripts find it by that name)
  // the pages' gates (`gate`: the Portfolio's, the Create token page's) are the same button and action, with their own test ids (the header's are on the same page)
  const testid = props.gate
    ? only ? `issue-connect-${only.id}` : choose ? 'issue-connect-choose' : 'issue-connect-local-key'
    : narrow ? 'wallet-connect' : only ? `wallet-connect-${only.id}` : choose ? 'wallet-connect' : 'wallet-connect-local-key';

  return (
    <div class="wallet-actions" data-testid="wallet-connect-list" data-network={config.network}>
      <Button
        small
        variant="primary"
        // the one Connect of the app, the header's and the pages': the landing page's pill (styles.css .btn-pill), the same size and colours in both themes
        data-testid={testid}
        loading={w.connecting || waiting}
        onClick={onConnect}
        class={noExtension && !supported ? 'btn-pill btn-off' : 'btn-pill'}
        {...(noExtension && !supported ? { 'data-state': 'unavailable', title: t(keyAllowed ? 'signer.unsupported' : 'signer.mirrorOff') } : {})}
      >
        {w.connecting || waiting ? t('shell.wallet.connecting') : t('shell.wallet.connectShort')}
      </Button>
      {dialog === 'wallets' ? (
        <Modal title={t('shell.wallet.chooseTitle')} onClose={() => setDialog(null)} data-testid="wallet-dialog">
          <div class="stack-sm" data-testid="wallet-choose">
            {/* the detected extensions first, each a row with its mark; the private key after them, with the one line that recommends an extension */}
            {w.detected.map((a) => (
              <Button key={a.id} variant="primary" block class="wallet-row" data-testid={`wallet-connect-${a.id}`} loading={w.connecting} onClick={() => connectOne(a.id)}>
                <WalletMark id={a.id} label={a.label} />
                <span class="wallet-row-text">{w.connecting ? t('shell.wallet.connecting') : t('shell.wallet.connect', { wallet: a.label })}</span>
              </Button>
            ))}
            {supported ? (
              <Button block class="wallet-row" data-testid="wallet-connect-local-key" onClick={() => { setDialog(null); w.openPrivateKey(); }}>
                <WalletMark id="local-key" label={t('shell.wallet.connectKey')} />
                <span class="wallet-row-text">{t('shell.wallet.connectKey')}</span>
              </Button>
            ) : null}
            {supported ? <p class="small muted wallet-recommend" data-testid="wallet-recommend">{t('signer.recommend')}</p> : null}
            {/* an extension this browser has not got: a link to get it (a row, not a button: it leaves the page) */}
            {installable(w.detected.map((a) => a.id), config.features.kastle).map((k) => (
              <a key={k.id} class="btn btn-block wallet-row wallet-install" href={k.installUrl} target="_blank" rel="noopener noreferrer" data-testid={`wallet-install-${k.id}`}>
                <WalletMark id={k.id} label={k.label} />
                <span class="wallet-row-text">{t('shell.wallet.install', { wallet: k.label })}</span>
              </a>
            ))}
          </div>
        </Modal>
      ) : null}
      {dialog === 'key' ? (
        <Modal title={t(keyAllowed ? 'signer.unsupportedTitle' : 'signer.mirrorOffTitle')} onClose={() => setDialog(null)} data-testid="wallet-dialog">
          {keyAllowed ? (
            <p data-testid="wallet-key-unsupported">{t(phone ? 'signer.unsupportedWhyPhone' : 'signer.unsupportedWhy')}</p>
          ) : (
            <p data-testid="wallet-key-mirror">{t('signer.mirrorOff')}</p>
          )}
        </Modal>
      ) : null}
    </div>
  );
}
