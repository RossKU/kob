// The private-key signer page (signer.html). The app loads it in `<iframe sandbox="allow-scripts">` without allow-same-origin: this document
// runs in an opaque origin, so the app's scripts can neither read this DOM (the key field) nor this memory (the key), and this page has no
// storage at all. It talks to the app only through the validated messages of protocol.ts, and signs only after a trusted click on its own
// Approve button, shown next to its own decoding of the request.
//
// While the user reads, the app may send the same request planned again (a later clock, the market now): the table follows in place, each
// changed figure flashing, and Approve signs what is shown. An update worse for the user than what they were shown says so and holds Approve
// for UPDATE_HOLD_MS (core.ts). The times of the table are said from the app's clock ("in 28 s", the DAA score below), a display only. The
// frame reports its content height so the app sizes it to its content.
import './signer.css';
import { loadKaspaSdk } from '../data/kaspa-sdk';
import { loadKob } from '../kob/wasm.browser';
import { t } from '../i18n';
import { createSignerCore, SignerKeyError, type PendingRequest, type SignerCore } from './core';
import { fromParent, parseAppMessage, toApp, type SignerToApp } from './protocol';
import { SeenTracker, visibilityCheckSupported } from './seen';
import type { SignerSummary, SummaryOrder } from './summary';
import { DAA_PER_SECOND, untilWords } from './words';

/** the app's origin: the origin of this page's URL (this document's own origin is opaque, "null") */
const APP_ORIGIN = new URL(location.href).origin;
/**
 * an Approve button that appears under the pointer is not clickable at once (a click meant for the app must not land on it). The visibility
 * check holds it too (every part of the request fully on screen for SEEN_MIN_MS, 400 ms, after the frame is shown), so this is the floor after
 * the request arrives; it is longer than a double click.
 */
const APPROVE_DELAY_MS = 500;
/** how long Approve is held after an update that is worse for the user (no figure may change under a click about to land) */
const UPDATE_HOLD_MS = 500;
/** how long a changed figure flashes */
const FLASH_MS = 1200;

const root = document.getElementById('signer') as HTMLElement;

function el<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string> = {}, ...children: (Node | string)[]): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  for (const c of children) e.append(c);
  return e;
}

const show = (...nodes: Node[]) => root.replaceChildren(...nodes);

// the frame's content height, for the app to size the frame to it (up to the viewport; beyond that the frame scrolls)
let lastHeight = -1;
const reportHeight = () => {
  const px = Math.ceil(root.getBoundingClientRect().height);
  if (px === lastHeight) return;
  lastHeight = px;
  post(toApp({ type: 'height', px }));
};
if (typeof ResizeObserver !== 'undefined') new ResizeObserver(reportHeight).observe(root);

/** the app's clock (display only): its DAA score and when it came, for the times of the table */
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
  // what the key is, what forgets it and which key to use: said before the user pastes anything
  const guide = el('ul', { class: 'guide muted', 'data-testid': 'signer-guide' }, el('li', {}, t('signer.guide.what')), el('li', {}, t('signer.guide.forget')), el('li', {}, t('signer.guide.small')));
  show(el('div', { class: 'row' }, input, button), error, guide);
  input.focus();
}

function idleView(core: SignerCore): void {
  const pk = core.pubkey() ?? '';
  show(el('p', { class: 'muted', 'data-testid': 'signer-idle' }, t('signer.idle', { key: `${pk.slice(0, 8)}…${pk.slice(-8)}` })));
}

/** why Approve waits: a sentence a person can act on, and (small) the counts behind it for whoever is asked to report the problem */
interface Why {
  text: string;
  counts: string;
}

/** a part counts as fully on screen from this intersection ratio (sub-pixel layout keeps some exact fits just below 1) */
const FULL = 0.98;

/**
 * Chromium's IntersectionObserver v2 tells whether an element is really visible (not covered, not transparent, not transformed); its ratio tells
 * whether it is clipped. `onChange(true)` once every part of the summary has been fully visible for a moment and the buttons are now (seen.ts):
 * a page that clips the frame down to its buttons gets no approval.
 */
function watchRequest(parts: Element[], buttons: Element, onChange: (ready: boolean) => void, onWhy?: (why: Why | null) => void): () => void {
  if (!visibilityCheckSupported()) {
    onChange(false); // no visibility check in this browser: never approvable (the app does not offer the private key here)
    return () => undefined;
  }
  // what the browser last reported per element, only to say why Approve waits (display only: the decision is the tracker's)
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
  // a part that stays visible sends no further entry: its time is counted here
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

/** The cells of a request's table by key (what an update fills in place), and the time cells (said from the clock every second). */
interface Cells {
  text: Map<string, HTMLElement>;
  times: { el: HTMLElement; daa: bigint; value: string }[];
}

/** A time cell: "in 28 s" from the clock, the DAA score below; without a clock the row's own text. */
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

/** One order the request creates or amends: its title, token and terms (an if-done entry's committed exit nested below). */
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

/** The shape of a table: an update with the same shape is filled in place, any other is drawn anew. */
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

/** A changed figure flashes (a background, never an opacity: the visibility check counts opacity). */
function flash(e: HTMLElement): void {
  e.classList.remove('changed');
  void e.offsetWidth;
  e.classList.add('changed');
  setTimeout(() => e.classList.remove('changed'), FLASH_MS);
}

/** The request on screen: what an update fills in place. */
let onScreen: { id: string; shape: string; cells: Cells; hold(): void; note: HTMLElement } | null = null;

/** An update of the request on screen: filled in place when its table has the same shape, else drawn anew (seen again before Approve). */
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
        // a time moves with every plan: said afresh, not flashed
        fillTime(cell, time.daa, time.value);
      }
    } else if (cell.textContent !== next.value) {
      cell.textContent = next.value;
      flash(cell);
    }
  }
  if (req.change === 'shown') {
    v.note.hidden = false;
    flash(v.note);
    v.hold();
  }
}

/** Every second: the times of the request on screen, from the clock. */
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
  const reject = el('button', { type: 'button', 'data-testid': 'signer-reject' }, t('signer.reject'));
  const approve = el('button', { type: 'button', class: 'primary', disabled: '', 'data-testid': 'signer-approve' }, t('signer.approve'));
  const actions = el('div', { class: 'row end' }, reject, approve);
  // why Approve still waits, once it has waited a while (the visibility check: an overlay, a zoom or a cut-off frame keeps it disabled)
  const waitText = el('p', { class: 'muted' }, '');
  const waitCounts = el('p', { class: 'muted small', 'data-testid': 'signer-wait-counts' }, '');
  const waitWhy = el('div', { 'data-testid': 'signer-wait-why' }, waitText, waitCounts);
  waitWhy.hidden = true;
  const shownAt = performance.now();
  const title = el('h1', { 'data-testid': 'signer-title' }, s.title);
  cells.text.set('title', title);
  const orders = s.orders.map((o, i) => orderView(o, `signer-order-${i}`, cells, `o:${i}`));
  // an update worse for the user than what was shown says so (drawn anew when its table changed shape)
  const note = el('p', { class: 'updated', 'data-testid': 'signer-updated' }, t('signer.updated'));
  note.hidden = req.change !== 'shown';
  const problems = s.problems > 0 ? [el('p', { class: 'error', 'data-testid': 'signer-problems' }, t('signer.problems', { count: s.problems }))] : [];
  if (!visibilityCheckSupported()) problems.push(el('p', { class: 'error', 'data-testid': 'signer-unsupported' }, t('signer.unsupported')));
  let delayOver = false;
  let visible = false;
  let heldUntil = 0;
  const update = () => {
    approve.disabled = !(delayOver && visible && performance.now() >= heldUntil);
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
  // every part of the summary: the title, each line, each order's title, token and terms, the problems note
  // the lines are observed by their label and value, not by the row: a row's box touches its neighbours', and on a phone with a fractional
  // device pixel ratio the browser reported rows as covered by the next one (Android Chrome 2026-10-23: "lines seen 2/5, covered parts 3"), which
  // kept Approve disabled; the label and value sit inside the row's padding, so only a real overlay covers them, and every word must still show
  const cellsOf = (root: Element): Element[] => [...root.querySelectorAll(':scope > .line > dt, :scope > .line > dd')];
  const parts: Element[] = [title, ...cellsOf(list), ...orders.flatMap((o) => [...o.querySelectorAll('h2, p.token'), ...o.querySelectorAll('.line > dt, .line > dd')]), ...problems];
  stopVisibility = watchRequest(
    parts,
    actions,
    (v) => {
      visible = v;
      update();
    },
    (w) => {
      // once shown, the line keeps its place (only its visibility changes): a frame that shrank the moment Approve opens would move under the
      // pointer and be reported not visible again, so the click would land on a disabled button
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
    // only a real user click on this button signs (the app cannot click into this cross-origin frame; synthetic events are not trusted)
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
  onScreen = { id: req.id, shape: shapeOf(s), cells, hold, note };
  show(note, title, list, ...orders, ...problems, actions, waitWhy);
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
}

boot().catch(() => show(el('p', { class: 'error' }, t('signer.loadFailed'))));
