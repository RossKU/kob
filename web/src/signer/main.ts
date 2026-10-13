// The private-key signer page (signer.html). The app loads it in `<iframe sandbox="allow-scripts">` without allow-same-origin: this document
// runs in an opaque origin, so the app's scripts can neither read this DOM (the key field) nor this memory (the key), and this page has no
// storage at all. It talks to the app only through the validated messages of protocol.ts, and signs only after a trusted click on its own
// Approve button, shown next to its own decoding of the request.
import './signer.css';
import { loadKaspaSdk } from '../data/kaspa-sdk';
import { loadKob } from '../kob/wasm.browser';
import { t } from '../i18n';
import { createSignerCore, SignerKeyError, type PendingRequest, type SignerCore } from './core';
import { fromParent, parseAppMessage, toApp, type SignerToApp } from './protocol';

/** the app's origin: the origin of this page's URL (this document's own origin is opaque, "null") */
const APP_ORIGIN = new URL(location.href).origin;
/** an Approve button that appears under the pointer is not clickable at once (a click meant for the app must not land on it) */
const APPROVE_DELAY_MS = 700;

const root = document.getElementById('signer') as HTMLElement;

function el<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string> = {}, ...children: (Node | string)[]): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  for (const c of children) e.append(c);
  return e;
}

const show = (...nodes: Node[]) => root.replaceChildren(...nodes);

function post(m: SignerToApp): void {
  window.parent.postMessage(m, APP_ORIGIN);
}

// ------------------------------------------------------------------------------------------------ views

function connectView(core: SignerCore): void {
  const input = el('input', {
    type: 'password', id: 'signer-key', class: 'key', autocomplete: 'off', spellcheck: 'false', autocapitalize: 'off', autocorrect: 'off',
    'data-1p-ignore': 'true', 'data-lpignore': 'true', 'data-form-type': 'other', 'aria-label': t('signer.key.label'), placeholder: t('signer.key.placeholder'),
    'data-testid': 'signer-key',
  });
  const error = el('p', { class: 'error', role: 'alert', 'data-testid': 'signer-key-error' });
  const button = el('button', { type: 'button', class: 'primary', 'data-testid': 'signer-connect' }, t('signer.key.connect'));
  const go = () => {
    const text = input.value;
    input.value = '';
    try {
      core.importKey(text);
      idleView(core);
    } catch (e) {
      error.textContent = e instanceof SignerKeyError ? t('signer.key.invalid') : t('signer.key.failed');
      input.focus();
    }
  };
  button.addEventListener('click', go);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') go();
  });
  show(el('div', { class: 'row' }, input, button), error);
  input.focus();
}

function idleView(core: SignerCore): void {
  const pk = core.pubkey() ?? '';
  show(el('p', { class: 'muted', 'data-testid': 'signer-idle' }, t('signer.idle', { key: `${pk.slice(0, 8)}…${pk.slice(-8)}` })));
}

/** Chromium's IntersectionObserver v2 tells whether the frame is really visible (not covered, not transparent, not transformed). */
function watchVisibility(target: Element, onChange: (visible: boolean) => void): () => void {
  if (typeof IntersectionObserver === 'undefined' || !('isVisible' in IntersectionObserverEntry.prototype)) {
    onChange(true); // not supported: the delay and the trusted click remain
    return () => undefined;
  }
  const io = new IntersectionObserver((entries) => onChange((entries.at(-1) as IntersectionObserverEntry & { isVisible?: boolean }).isVisible === true), {
    threshold: [0],
    trackVisibility: true,
    delay: 100,
  } as IntersectionObserverInit);
  io.observe(target);
  return () => io.disconnect();
}

let stopVisibility: () => void = () => undefined;

function requestView(core: SignerCore, req: PendingRequest): void {
  stopVisibility();
  const s = req.summary;
  const list = el('dl', { class: 'lines', 'data-testid': 'signer-summary' });
  for (const l of s.lines) list.append(el('div', { class: `line ${l.dir ?? ''}` }, el('dt', {}, l.label), el('dd', {}, l.value)));
  list.append(el('div', { class: 'line fee' }, el('dt', {}, t('signer.fee')), el('dd', { 'data-testid': 'signer-fee' }, s.fee)));
  const reject = el('button', { type: 'button', 'data-testid': 'signer-reject' }, t('signer.reject'));
  const approve = el('button', { type: 'button', class: 'primary', disabled: '', 'data-testid': 'signer-approve' }, t('signer.approve'));
  const actions = el('div', { class: 'row end' }, reject, approve);
  let delayOver = false;
  let visible = false;
  const update = () => {
    approve.disabled = !(delayOver && visible);
  };
  setTimeout(() => {
    delayOver = true;
    update();
  }, APPROVE_DELAY_MS);
  stopVisibility = watchVisibility(actions, (v) => {
    visible = v;
    update();
  });
  approve.addEventListener('click', (e) => {
    // only a real user click on this button signs (the app cannot click into this cross-origin frame; synthetic events are not trusted)
    if (!e.isTrusted || approve.disabled) return;
    stopVisibility();
    core.approve(req.id);
    idleView(core);
  });
  reject.addEventListener('click', () => {
    stopVisibility();
    core.reject(req.id, 'user');
    idleView(core);
  });
  show(
    el('h1', { 'data-testid': 'signer-title' }, s.title),
    list,
    ...(s.problems > 0 ? [el('p', { class: 'error', 'data-testid': 'signer-problems' }, t('signer.problems', { count: s.problems }))] : []),
    actions,
  );
}

// ------------------------------------------------------------------------------------------------ boot

async function boot(): Promise<void> {
  if (window.parent === window) {
    show(el('p', { class: 'muted' }, t('signer.notFramed')));
    return;
  }
  show(el('p', { class: 'muted' }, t('signer.loading')));
  const [sdk, kob] = await Promise.all([loadKaspaSdk(), loadKob()]);
  const core = createSignerCore({ sdk, kob, post });
  window.addEventListener('message', (e) => {
    if (!fromParent(e, window.parent, APP_ORIGIN)) return;
    const m = parseAppMessage(e.data);
    if (!m) return;
    const req = core.receive(m);
    if (req) requestView(core, req);
    else if (m.type === 'forget') connectView(core);
    else if (core.hasKey() && !core.pending()) {
      stopVisibility();
      idleView(core);
    }
  });
  connectView(core);
  post(toApp({ type: 'ready' }));
}

boot().catch(() => show(el('p', { class: 'error' }, t('signer.loadFailed'))));
