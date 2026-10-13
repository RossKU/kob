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
import { SeenTracker, visibilityCheckSupported } from './seen';
import type { SummaryOrder } from './summary';

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

/** a part counts as fully on screen from this intersection ratio (sub-pixel layout keeps some exact fits just below 1) */
const FULL = 0.98;

/**
 * Chromium's IntersectionObserver v2 tells whether an element is really visible (not covered, not transparent, not transformed); its ratio tells
 * whether it is clipped. `onChange(true)` once every part of the summary has been fully visible for a moment and the buttons are now (seen.ts):
 * a page that clips the frame down to its buttons gets no approval.
 */
function watchRequest(parts: Element[], buttons: Element, onChange: (ready: boolean) => void): () => void {
  if (!visibilityCheckSupported()) {
    onChange(false); // no visibility check in this browser: never approvable (the app does not offer the private key here)
    return () => undefined;
  }
  const ids = new Map<Element, string>(parts.map((p, i) => [p, `part-${i}`]));
  const tracker = new SeenTracker([...ids.values()]);
  const now = () => performance.now();
  const io = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        const fully = (e as IntersectionObserverEntry & { isVisible?: boolean }).isVisible === true && e.isIntersecting && e.intersectionRatio >= FULL;
        if (e.target === buttons) tracker.buttons(fully, now());
        else tracker.part(ids.get(e.target) ?? '', fully, now());
      }
      onChange(tracker.ready(now()));
    },
    { threshold: [0, FULL, 1], trackVisibility: true, delay: 100 } as IntersectionObserverInit,
  );
  for (const p of parts) io.observe(p);
  io.observe(buttons);
  // a part that stays visible sends no further entry: its time is counted here
  const timer = setInterval(() => onChange(tracker.ready(now())), 100);
  return () => {
    io.disconnect();
    clearInterval(timer);
  };
}

let stopVisibility: () => void = () => undefined;

/** One order the request creates or amends: its title, token and terms (an if-done entry's committed exit nested below). */
function orderView(o: SummaryOrder, testid: string): HTMLElement {
  const rows = el('dl', { class: 'lines' });
  for (const r of o.rows) rows.append(el('div', { class: 'line' }, el('dt', {}, r.label), el('dd', {}, r.value)));
  return el(
    'section',
    { class: 'order', 'data-testid': testid },
    el('h2', {}, o.title),
    el('p', { class: 'muted token' }, o.token),
    rows,
    ...(o.exit ? [orderView(o.exit, `${testid}-exit`)] : []),
  );
}

function requestView(core: SignerCore, req: PendingRequest): void {
  stopVisibility();
  const s = req.summary;
  const list = el('dl', { class: 'lines', 'data-testid': 'signer-summary' });
  for (const l of s.lines) list.append(el('div', { class: `line ${l.dir ?? ''}` }, el('dt', {}, l.label), el('dd', {}, l.value)));
  list.append(el('div', { class: 'line fee' }, el('dt', {}, t('signer.fee')), el('dd', { 'data-testid': 'signer-fee' }, s.fee)));
  const reject = el('button', { type: 'button', 'data-testid': 'signer-reject' }, t('signer.reject'));
  const approve = el('button', { type: 'button', class: 'primary', disabled: '', 'data-testid': 'signer-approve' }, t('signer.approve'));
  const actions = el('div', { class: 'row end' }, reject, approve);
  const title = el('h1', { 'data-testid': 'signer-title' }, s.title);
  const orders = s.orders.map((o, i) => orderView(o, `signer-order-${i}`));
  const problems = s.problems > 0 ? [el('p', { class: 'error', 'data-testid': 'signer-problems' }, t('signer.problems', { count: s.problems }))] : [];
  if (!visibilityCheckSupported()) problems.push(el('p', { class: 'error', 'data-testid': 'signer-unsupported' }, t('signer.unsupported')));
  let delayOver = false;
  let visible = false;
  const update = () => {
    approve.disabled = !(delayOver && visible);
  };
  setTimeout(() => {
    delayOver = true;
    update();
  }, APPROVE_DELAY_MS);
  // every part of the summary: the title, each line, each order's title, token and terms, the problems note
  const parts: Element[] = [title, ...list.children, ...orders.flatMap((o) => [...o.querySelectorAll('h2, p.token, .line')]), ...problems];
  stopVisibility = watchRequest(parts, actions, (v) => {
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
  show(title, list, ...orders, ...problems, actions);
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
