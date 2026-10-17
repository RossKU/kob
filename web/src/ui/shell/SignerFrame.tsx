// The private-key signer frame (signer.html) in the app: `<iframe sandbox="allow-scripts">` WITHOUT allow-same-origin, so it runs in an opaque
// origin and this page's scripts can read neither its DOM nor its memory. Mounted while a private-key session exists (removing it forgets the
// key), shown for the key entry and for each signing request, hidden in between. Messages go through `SignerHost` (wallet/local-key.ts). The
// frame is as tall as its content (it reports its height), up to the viewport less SIGNER_CHROME_PX; beyond that it scrolls.
import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { fromSigner } from '../../signer/protocol';
import { SignerHost, type SignerHostEvents } from '../../wallet/local-key';
import { Button } from '../kit';

export type SignerMode = 'closed' | 'connect' | 'hidden' | 'sign';

export interface SignerFrameProps {
  mode: SignerMode;
  /** the host of the mounted frame (null after unmount) */
  onHost(host: SignerHost | null): void;
  events: SignerHostEvents;
  /** the user closed the panel */
  onClose(): void;
}

export const SIGNER_URL = `${import.meta.env.BASE_URL}signer.html`;

/** What the panel around the frame takes of the viewport's height (its top offset, head and margins). */
const SIGNER_CHROME_PX = 110;

export function SignerFrame(props: SignerFrameProps) {
  if (props.mode === 'closed') return null;
  return <SignerPanel {...props} />;
}

function SignerPanel(props: SignerFrameProps) {
  const frame = useRef<HTMLIFrameElement>(null);
  const events = useRef(props.events);
  events.current = props.events;
  const [content, setContent] = useState<number | null>(null);
  const [viewport, setViewport] = useState(() => window.innerHeight);
  useEffect(() => {
    const onResize = () => setViewport(window.innerHeight);
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);

  useEffect(() => {
    const host = new SignerHost((m) => frame.current?.contentWindow?.postMessage(m, '*'), {
      onConnected: (pk) => events.current.onConnected?.(pk),
      onForgotten: () => events.current.onForgotten?.(),
      onRequest: (a) => events.current.onRequest?.(a),
      onHeight: (px) => setContent(px),
    });
    const onMessage = (e: MessageEvent) => {
      if (fromSigner(e, frame.current?.contentWindow ?? null)) host.receive(e.data);
    };
    window.addEventListener('message', onMessage);
    props.onHost(host);
    return () => {
      window.removeEventListener('message', onMessage);
      host.forget();
      props.onHost(null);
    };
  }, []);

  const visible = props.mode === 'connect' || props.mode === 'sign';
  return (
    <div class="signer-layer" hidden={!visible} data-mode={props.mode} data-testid="signer-overlay">
      <div class="signer-box" role="dialog" aria-modal="true" aria-label={t('signer.frameTitle')}>
        <div class="signer-head">
          <span class="small muted">{props.mode === 'connect' ? t('signer.recommend') : t('signer.option')}</span>
          <Button small variant="ghost" onClick={props.onClose} aria-label={t('signer.close')} title={t('signer.close')} data-testid="signer-close">
            {'×'}
          </Button>
        </div>
        <iframe
          ref={frame}
          class={`signer-frame signer-frame-${props.mode}`}
          src={SIGNER_URL}
          sandbox="allow-scripts"
          referrerpolicy="no-referrer"
          title={t('signer.frameTitle')}
          data-testid="signer-frame"
          {...(content !== null ? { style: { height: `${Math.max(60, Math.min(content, viewport - SIGNER_CHROME_PX))}px` } } : {})}
        />
      </div>
    </div>
  );
}
