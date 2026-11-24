// The private-key signer page (signer.html).
//
// The app loads it in `<iframe sandbox="allow-scripts">` without allow-same-origin. This document
// runs in an opaque origin, so the app's scripts can read neither this DOM, which holds the key
// field, nor this memory, which holds the key. This page has no storage at all. It talks to the
// app only through the validated messages of protocol.ts. It signs only after a trusted click on
// its own Approve button, shown next to its own decoding of the request.
//
// While the user reads, the app may send the same request planned again, with a later clock or
// the market now. The table follows in place, each changed figure flashes, and Approve signs what
// is shown. An update worse for the user than what they were shown says so and holds Approve for
// UPDATE_HOLD_MS (core.ts). The times of the table are said from the app's clock, as "in 28 s"
// with the DAA score below, for display only. The frame reports its content height so that the
// app sizes it to its content.
import './signer.css';
import { loadKaspaSdk } from '../data/kaspa-sdk';
import { loadKob } from '../kob/wasm.browser';
import { t } from '../i18n';
import { createSignerCore, SignerKeyError, type PendingRequest, type SignerCore } from './core';
import { noteSentences } from './notes';
import { fromParent, parseAppMessage, toApp, type SignerToApp } from './protocol';
import { SeenTracker, visibilityCheckSupported } from './seen';
import type { SignerSummary, SummaryOrder } from './summary';
import { DAA_PER_SECOND, untilWords } from './words';
import { privateKeySigningAllowed, showFrameGuard } from '../app/mirror';

/** The origin of this page's URL, the app's. This document's own origin is opaque ("null"). */
const APP_ORIGIN = new URL(location.href).origin;
/**
 * The delay before Approve becomes clickable after the request arrives. An Approve button that
 * appears under the pointer is not clickable at once, so a click meant for the app cannot land on
 * it. The visibility check holds it too, with every part of the request fully on screen for
 * SEEN_MIN_MS (400 ms) after the frame is shown. So this is the floor, and it is longer than a
 * double click.
 */
const APPROVE_DELAY_MS = 500;
/**
 * How long Approve is held after an update that is worse for the user, so that no figure changes
 * under a click about to land.
 */
const UPDATE_HOLD_MS = 500;
/** How long a changed figure flashes. */
const FLASH_MS = 1200;

const root = document.getElementById('signer') as HTMLElement;

/**
 * Applies the app's theme from the fragment of this page's URL, which the app changes without a
 * reload. It sets colours only, one of two fixed palettes (signer.css), else the OS's.
 */
function applyTheme(): void {
  const m = /(?:^#|&)theme=(dark|light)(?:&|$)/.exec(location.hash);
  if (m) document.documentElement.dataset.theme = m[1]!;
  else delete document.documentElement.dataset.theme;
}
applyTheme();
window.addEventListener('hashchange', applyTheme);

/**
 * The network the app runs on, from the same fragment (`net=`). It is a label only: neither the
 * key nor the transaction names a network, and one site serves both, so each request says which.
 */
const networkOf = (): string | null => /(?:^#|&)net=(mainnet|testnet-10)(?:&|$)/.exec(location.hash)?.[1] ?? null;

function el<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string> = {}, ...children: (Node | string)[]): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  for (const c of children) e.append(c);
  return e;
}

const show = (...nodes: Node[]) => root.replaceChildren(...nodes);

// The frame's content height, for the app to size the frame to it. That is up to the viewport, and
// beyond it the frame scrolls.
let lastHeight = -1;
const reportHeight = () => {
  const px = Math.ceil(root.getBoundingClientRect().height);
  if (px === lastHeight) return;
  lastHeight = px;
  post(toApp({ type: 'height', px }));
};
if (typeof ResizeObserver !== 'undefined') new ResizeObserver(reportHeight).observe(root);

/** The app's clock, as its DAA score and when it came, for the times of the table. Display only. */
let clock: { daa: bigint; at: number } | null = null;
const daaNow = (): bigint | null => (clock ? clock.daa + (BigInt(Math.floor(performance.now() - clock.at)) * DAA_PER_SECOND) / 1000n : null);
const until = (daa: bigint, now: bigint): string =>
  untilWords(daa, now, {
    now: t('signer.rel.now'),
    s: (n) => t('signer.rel.s', { n }),
    min: (n) => t('signer.rel.min', { n }),
    h: (n) => t('signer.rel.h', { n }),
    d: (n) => t('signer.rel.d', { n }),
  });

function post(m: SignerToApp): void {
  window.parent.postMessage(m, APP_ORIGIN);
}

// --- Views ---

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
  // What the key is, what forgets it and which key to use, said before the user pastes anything.
  const guide = el('ul', { class: 'guide muted', 'data-testid': 'signer-guide' }, el('li', {}, t('signer.guide.what')), el('li', {}, t('signer.guide.caution')));
  show(el('div', { class: 'row' }, input, button), error, guide);
  input.focus();
}

function idleView(core: SignerCore): void {
  const pk = core.pubkey() ?? '';
  show(el('p', { class: 'muted', 'data-testid': 'signer-idle' }, t('signer.idle', { key: `${pk.slice(0, 8)}…${pk.slice(-8)}` })));
}

/**
 * Why Approve waits. It is a sentence a person can act on, and in small print the counts behind it
 * for whoever is asked to report the problem.
 */
interface Why {
  text: string;
  counts: string;
}

/**
 * A part counts as fully on screen from this intersection ratio. Sub-pixel layout keeps some exact
 * fits just below 1.
 */
const FULL = 0.98;

/**
 * Watches whether the request is really on screen.
 *
 * Chromium's IntersectionObserver v2 tells whether an element is really visible, meaning not
 * covered, not transparent and not transformed. Its ratio tells whether it is clipped.
 * `onChange(true)` comes once every part of the summary has been fully visible for a moment and
 * the buttons are visible now (seen.ts). So a page that clips the frame down to its buttons gets
 * no approval.
 */
function watchRequest(parts: Element[], buttons: Element, onChange: (ready: boolean) => void, onWhy?: (why: Why | null) => void): () => void {
  if (!visibilityCheckSupported()) {
    onChange(false); // no visibility check in this browser: never approvable (the app does not offer the private key here)
    return () => undefined;
  }
  // What the browser last reported per element, only to say why Approve waits. It is display only,
  // and the decision is the tracker's.
  const last = new Map<Element, { visible: boolean | undefined; ratio: number }>();
  const why = (): Why => {
    const b = last.get(buttons);
    const notSeen = parts.filter((p) => { const l = last.get(p); return !l || l.visible !== true || l.ratio < FULL; });
    const covered = parts.map((p, i) => (last.get(p)?.visible === false ? String(i + 1) : '')).filter(Boolean);
    return {
      text: t('signer.waitWhy'),
      counts: t('signer.waitCounts', {
        buttons: !b ? '-' : b.visible !== true ? 'covered' : b.ratio < FULL ? `cut off (${Math.round(b.ratio * 100)} %)` : 'ok',
        lines: `${parts.length - notSeen.length}/${parts.length}`,
        covered: covered.length ? `${covered.length} (${covered.join(', ')} of ${parts.length})` : '0',
      }),
    };
  };
  const ids = new Map<Element, string>(parts.map((p, i) => [p, `part-${i}`]));
  const tracker = new SeenTracker([...ids.values()]);
  const now = () => performance.now();
  const io = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        const isVisible = (e as IntersectionObserverEntry & { isVisible?: boolean }).isVisible;
        last.set(e.target, { visible: isVisible, ratio: e.isIntersecting ? e.intersectionRatio : 0 });
        const fully = isVisible === true && e.isIntersecting && e.intersectionRatio >= FULL;
        if (e.target === buttons) tracker.buttons(fully, now());
        else tracker.part(ids.get(e.target) ?? '', fully, now());
      }
      onChange(tracker.ready(now()));
    },
    { threshold: [0, FULL, 1], trackVisibility: true, delay: 100 } as IntersectionObserverInit,
  );
  for (const p of parts) io.observe(p);
  io.observe(buttons);
  // A part that stays visible sends no further entry, so its time is counted here.
  const timer = setInterval(() => {
    const ready = tracker.ready(now());
    onChange(ready);
    onWhy?.(ready ? null : why());
  }, 100);
  return () => {
    io.disconnect();
    clearInterval(timer);
  };
}

let stopVisibility: () => void = () => undefined;

/**
 * The cells of a request's table by key, which an update fills in place, and the time cells, which
 * are said from the clock every second.
 */
interface Cells {
  text: Map<string, HTMLElement>;
  times: { el: HTMLElement; daa: bigint; value: string }[];
}

/**
 * A time cell. It says "in 28 s" from the clock with the DAA score below, or without a clock the
 * row's own text.
 */
function timeCell(cells: Cells, key: string, value: string, daa: string): HTMLElement {
  const dd = el('dd', { 'data-daa': daa });
  cells.text.set(key, dd);
  cells.times.push({ el: dd, daa: BigInt(daa), value });
  fillTime(dd, BigInt(daa), value);
  return dd;
}

function fillTime(dd: HTMLElement, daa: bigint, value: string): void {
  const now = daaNow();
  if (now === null) {
    if (dd.textContent !== value) dd.replaceChildren(value);
    return;
  }
  const rel = until(daa, now);
  const small = t('signer.rel.daa', { daa: daa.toString() });
  const [a, b] = [dd.children[0], dd.children[1]];
  if (a && b) {
    if (a.textContent !== rel) a.textContent = rel;
    if (b.textContent !== small) b.textContent = small;
  } else dd.replaceChildren(el('span', { class: 'rel' }, rel), el('span', { class: 'daa' }, small));
}

/**
 * One order the request creates or amends, with its title, token and terms. An if-done entry's
 * committed exit is nested below.
 */
function orderView(o: SummaryOrder, testid: string, cells: Cells, key: string): HTMLElement {
  const rows = el('dl', { class: 'lines' });
  o.rows.forEach((r, j) => {
    const k = `${key}:r:${j}`;
    const dd = r.daa ? timeCell(cells, k, r.value, r.daa) : el('dd', {}, r.value);
    if (!r.daa) cells.text.set(k, dd);
    rows.append(el('div', { class: 'line' }, el('dt', {}, r.label), dd));
  });
  const h2 = el('h2', {}, o.title);
  const token = el('p', { class: 'muted token' }, o.token);
  cells.text.set(`${key}:title`, h2);
  cells.text.set(`${key}:token`, token);
  return el('section', { class: 'order', 'data-testid': testid }, h2, token, rows, ...(o.exit ? [orderView(o.exit, `${testid}-exit`, cells, `${key}:x`)] : []));
}

/** The shape of a table. An update of the same shape is filled in place, any other drawn anew. */
const shapeOf = (s: SignerSummary): string => {
  const order = (o: SummaryOrder): string => `[${o.rows.map((r) => `${r.label}${r.daa ? '@' : ''}`).join('|')}${o.exit ? order(o.exit) : ''}]`;
  return [s.lines.map((l) => `${l.label}/${l.dir ?? ''}`).join('|'), s.problems, ...s.orders.map(order)].join('#');
};

/** The texts of a table by the keys of `Cells`. */
function textsOf(s: SignerSummary): Map<string, { value: string; daa?: string }> {
  const out = new Map<string, { value: string; daa?: string }>();
  out.set('title', { value: s.title });
  s.lines.forEach((l, i) => out.set(`l:${i}`, { value: l.value }));
  out.set('fee', { value: s.fee });
  const order = (o: SummaryOrder, key: string) => {
    out.set(`${key}:title`, { value: o.title });
    out.set(`${key}:token`, { value: o.token });
    o.rows.forEach((r, j) => out.set(`${key}:r:${j}`, r.daa ? { value: r.value, daa: r.daa } : { value: r.value }));
    if (o.exit) order(o.exit, `${key}:x`);
  };
  s.orders.forEach((o, i) => order(o, `o:${i}`));
  return out;
}

/**
 * Flashes a changed figure. It uses a background and never an opacity, because the visibility
 * check counts opacity.
 */
function flash(e: HTMLElement): void {
  e.classList.remove('changed');
  void e.offsetWidth;
  e.classList.add('changed');
  setTimeout(() => e.classList.remove('changed'), FLASH_MS);
}

/** The request on screen, which an update fills in place. */
let onScreen: { id: string; shape: string; cells: Cells; hold(): void } | null = null;

/**
 * Shows an update of the request on screen. It is filled in place when its table has the same
 * shape, else drawn anew and seen again before Approve.
 */
function updateView(core: SignerCore, req: PendingRequest): void {
  const v = onScreen;
  if (!v || v.id !== req.id || v.shape !== shapeOf(req.summary)) {
    requestView(core, req);
    return;
  }
  for (const [k, next] of textsOf(req.summary)) {
    const cell = v.cells.text.get(k);
    if (!cell) continue;
    if (next.daa) {
      const time = v.cells.times.find((x) => x.el === cell);
      if (time && (time.daa.toString() !== next.daa || time.value !== next.value)) {
        time.daa = BigInt(next.daa);
        time.value = next.value;
        cell.setAttribute('data-daa', next.daa);
        // A time moves with every plan, so it is said afresh and not flashed.
        fillTime(cell, time.daa, time.value);
      }
    } else if (cell.textContent !== next.value) {
      cell.textContent = next.value;
      flash(cell);
    }
  }
  // On a change worse for the user, the changed figures flash (above) and Approve waits a moment.
  // There is no banner over the table, because it would move Approve.
  if (req.change === 'shown') v.hold();
}

/** Every second, says the times of the request on screen from the clock. */
setInterval(() => {
  if (onScreen) for (const x of onScreen.cells.times) fillTime(x.el, x.daa, x.value);
}, 1000);

function requestView(core: SignerCore, req: PendingRequest): void {
  stopVisibility();
  const s = req.summary;
  const cells: Cells = { text: new Map(), times: [] };
  const list = el('dl', { class: 'lines', 'data-testid': 'signer-summary' });
  s.lines.forEach((l, i) => {
    const dd = el('dd', {}, l.value);
    cells.text.set(`l:${i}`, dd);
    list.append(el('div', { class: `line ${l.dir ?? ''}` }, el('dt', {}, l.label), dd));
  });
  const fee = el('dd', { 'data-testid': 'signer-fee' }, s.fee);
  cells.text.set('fee', fee);
  list.append(el('div', { class: 'line fee' }, el('dt', {}, t('signer.fee')), fee));
  // The KAS of the order UTXOs, custodies and strays this transaction spends is not covered by the
  // wallet's signature. The frame reads no node: it is the app's report, and the KAS lines and the fee
  // are computed with it. An understated input makes the real fee larger than shown (R8F2b-2), so the
  // line says whose word it is rather than calling the fee a bound.
  if (s.unsignedKas) list.append(el('div', { class: 'line fee muted', 'data-testid': 'signer-unsigned-kas' }, el('dt', {}, t('signer.unsignedKas'))));
  const reject = el('button', { type: 'button', 'data-testid': 'signer-reject' }, t('signer.reject'));
  // On a merge session's screen, Allow opens the session and that click signs nothing. The fixed
  // line says what still asks.
  const approve = el('button', { type: 'button', class: 'primary', disabled: '', 'data-testid': 'signer-approve' }, t(req.session ? 'signer.allow' : 'signer.approve'));
  const sessionNote = req.session ? [el('p', { class: 'muted', 'data-testid': 'signer-session-note' }, t('signer.session.note'))] : [];
  const actions = el('div', { class: 'row end' }, reject, approve);
  // Why Approve still waits, once it has waited a while. The visibility check keeps it disabled
  // under an overlay, a zoom or a cut-off frame.
  const waitText = el('p', { class: 'muted' }, '');
  const waitCounts = el('p', { class: 'muted small', 'data-testid': 'signer-wait-counts' }, '');
  const waitWhy = el('div', { 'data-testid': 'signer-wait-why' }, waitText, waitCounts);
  waitWhy.hidden = true;
  const shownAt = performance.now();
  const title = el('h1', { 'data-testid': 'signer-title' }, s.title);
  cells.text.set('title', title);
  const orders = s.orders.map((o, i) => orderView(o, `signer-order-${i}`, cells, `o:${i}`));
  const problems = s.problems > 0 ? [el('p', { class: 'error', 'data-testid': 'signer-problems' }, t(s.problems === 1 ? 'signer.problems.one' : 'signer.problems.other', { count: s.problems, codes: s.problemCodes.join(', ') }))] : [];
  if (!visibilityCheckSupported()) problems.push(el('p', { class: 'error', 'data-testid': 'signer-unsupported' }, t('signer.unsupported')));
  // The app's hints go under the signer's own rows and orders, muted. They are never part of what
  // Approve waits for.
  const hints = noteSentences(req.notes);
  const hintsView = hints.length
    ? [el('div', { class: 'app-hints', 'data-testid': 'signer-app-hints' }, el('p', {}, t('signer.hint.label')), el('ul', {}, ...hints.map((h) => el('li', { 'data-testid': 'signer-app-hint' }, h))))]
    : [];
  let delayOver = false;
  let visible = false;
  let heldUntil = 0;
  const update = () => {
    // A finding the decoder blocks signing on keeps Approve off, so the user can only reject. The
    // app's confirmation screen refuses the same transaction.
    approve.disabled = !(delayOver && visible && performance.now() >= heldUntil && s.problems === 0);
  };
  setTimeout(() => {
    delayOver = true;
    update();
  }, APPROVE_DELAY_MS);
  const hold = () => {
    heldUntil = performance.now() + UPDATE_HOLD_MS;
    update();
    setTimeout(update, UPDATE_HOLD_MS + 10);
  };
  if (req.change === 'shown') hold();
  // Every part of the summary is watched: the title, each line, each order's title, token and
  // terms, and the problems note.
  //
  // The lines are observed by their label and value, not by the row. A row's box touches its
  // neighbours'. On a phone with a fractional device pixel ratio, the browser can report rows as
  // covered by the next one, which would keep Approve disabled. The label and value sit inside the
  // row's padding, so only a real overlay covers them, and every word must still show.
  const cellsOf = (root: Element): Element[] => [...root.querySelectorAll(':scope > .line > dt, :scope > .line > dd')];
  const parts: Element[] = [title, ...cellsOf(list), ...orders.flatMap((o) => [...o.querySelectorAll('h2, p.token'), ...o.querySelectorAll('.line > dt, .line > dd')]), ...sessionNote, ...problems];
  stopVisibility = watchRequest(
    parts,
    actions,
    (v) => {
      visible = v;
      update();
    },
    (w) => {
      // Once shown, the line keeps its place and only its visibility changes. A frame that shrank
      // the moment Approve opens would move under the pointer and be reported not visible again,
      // so the click would land on a disabled button.
      const show = approve.disabled && performance.now() - shownAt > 3000 && w !== null;
      if (show) {
        waitWhy.hidden = false;
        waitWhy.style.visibility = 'visible';
        if (waitText.textContent !== w.text) waitText.textContent = w.text;
        if (waitCounts.textContent !== w.counts) waitCounts.textContent = w.counts;
      } else if (!waitWhy.hidden) waitWhy.style.visibility = 'hidden';
    },
  );
  approve.addEventListener('click', (e) => {
    // Only a real user click on this button signs. The app cannot click into this cross-origin
    // frame, and synthetic events are not trusted.
    if (!e.isTrusted || approve.disabled) return;
    stopVisibility();
    onScreen = null;
    core.approve(req.id);
    idleView(core);
  });
  reject.addEventListener('click', () => {
    stopVisibility();
    onScreen = null;
    core.reject(req.id, 'user');
    idleView(core);
  });
  onScreen = { id: req.id, shape: shapeOf(s), cells, hold };
  // Which network this is, under the title. Mainnet says that the funds are real.
  const net = networkOf();
  const netLine = net ? [el('p', { class: net === 'mainnet' ? 'net net-main' : 'net', 'data-testid': 'signer-network', 'data-network': net }, t(net === 'mainnet' ? 'signer.network.mainnetFrame' : 'signer.network.otherFrame', { network: net }))] : [];
  show(title, ...netLine, list, ...orders, ...hintsView, ...sessionNote, ...problems, actions, waitWhy);
}

// --- Boot ---

async function boot(): Promise<void> {
  // A mirror host that cannot send this page's header policy (app/mirror.ts) gets no key entry,
  // framed or not. It shows only the reason and a link to the app, outside any frame.
  if (!privateKeySigningAllowed()) {
    showFrameGuard(document, root, new URL('./', location.href).href, { title: t('signer.mirrorOffTitle'), body: t('signer.mirrorOff'), link: t('signer.mirrorOpenApp') });
    return;
  }
  if (window.parent === window) {
    show(el('p', { class: 'muted' }, t('signer.notFramed')));
    return;
  }
  show(el('p', { class: 'muted' }, t('signer.loading')));
  const [sdk, kob] = await Promise.all([loadKaspaSdk(), loadKob()]);
  // The frame's own monotonic clock. Re-time windows and a session's expiry never follow the app's
  // clock or a change of the system time.
  const core = createSignerCore({ sdk, kob, post, registryHash: __KOB_REGISTRY_TABLE_HASH__, now: () => performance.now() });
  window.addEventListener('message', (e) => {
    if (!fromParent(e, window.parent, APP_ORIGIN)) return;
    const m = parseAppMessage(e.data);
    if (!m) return;
    if (m.type === 'clock') {
      clock = { daa: BigInt(m.daa), at: performance.now() };
      if (onScreen) for (const x of onScreen.cells.times) fillTime(x.el, x.daa, x.value);
      return;
    }
    const req = core.receive(m);
    if (req && m.type === 'update') updateView(core, req);
    else if (req) requestView(core, req);
    else if (m.type === 'update') return;
    else if (m.type === 'forget') connectView(core);
    else if (core.hasKey() && !core.pending()) {
      stopVisibility();
      onScreen = null;
      idleView(core);
    }
  });
  connectView(core);
  post(toApp({ type: 'ready' }));
  post(toApp({ type: 'hello', caps: ['merge-session'] }));
}

boot().catch(() => show(el('p', { class: 'error' }, t('signer.loadFailed'))));
