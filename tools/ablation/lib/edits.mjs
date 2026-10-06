// Edit DSL for contract mutations. Every edit is validated: it must match exactly where it says it matches and it must
// change the text; otherwise EditError is thrown (an ablation can never silently be a no-op).
//
//   del(str)                    remove `str` (must occur exactly once)
//   rep(from, to)               replace `from` by `to` (`from` must occur exactly once)
//   rx(anchor, pattern, to)     find `anchor` (exactly once), then replace the first match of `pattern` that lies at or
//                               after the anchor and before the next `entry` / `function` declaration (so a pattern
//                               can not leak into a neighbouring function) by `to` (string, or function(match) -> string)
//   rxDel(anchor, pattern)      rx with an empty replacement; the newline right after the match is removed too
//   fn(label, f)                arbitrary callback f(text) -> text (escape hatch, still checked for no-op)
//
// The object forms ({del}, {replace}, {regex}, {fn}) are what the catalogs may also spell out directly.

export class EditError extends Error {}

export const del = str => ({ del: str });
export const rep = (from, to) => ({ replace: [from, to] });
export const rx = (anchor, pattern, to) => ({ regex: { anchor, pattern, to } });
export const rxDel = (anchor, pattern) => ({ regex: { anchor, pattern, to: '' } });
export const fn = (label, f) => ({ fn: f, label });

const preview = s => JSON.stringify(s.length > 70 ? s.slice(0, 67) + '...' : s);

function countOf(text, s) {
  let n = 0;
  for (let i = text.indexOf(s); i >= 0; i = text.indexOf(s, i + s.length)) n++;
  return n;
}

function replaceOnce(text, from, to, what) {
  if (typeof from !== 'string' || from === '') throw new EditError(`${what}: search string must be a non-empty string`);
  if (typeof to !== 'string') throw new EditError(`${what}: replacement must be a string`);
  const n = countOf(text, from);
  if (n !== 1) throw new EditError(`${what}: ${preview(from)} matches ${n}x (need exactly 1)`);
  const i = text.indexOf(from);
  return { text: text.slice(0, i) + to + text.slice(i + from.length), what: `${what} ${preview(from)}` };
}

function applyRegex(text, { anchor, pattern, to = '' }) {
  if (typeof anchor !== 'string' || anchor === '') throw new EditError('regex: anchor must be a non-empty string');
  if (!(pattern instanceof RegExp)) throw new EditError('regex: pattern must be a RegExp');
  const n = countOf(text, anchor);
  if (n !== 1) throw new EditError(`regex: anchor ${preview(anchor)} occurs ${n}x (need exactly 1)`);
  const a = text.indexOf(anchor);
  // scope: from the anchor to the next entry / function declaration
  const rest = text.slice(a + anchor.length);
  const decl = /\n[ \t]*(?:entry|function)\b/.exec(rest);
  const end = a + anchor.length + (decl ? decl.index : rest.length);
  const scope = text.slice(a, end);
  const re = new RegExp(pattern.source, pattern.flags.replace(/[gy]/g, ''));
  const m = re.exec(scope);
  if (!m) throw new EditError(`regex: ${pattern} not found between anchor ${preview(anchor)} and the next declaration`);
  const repl = typeof to === 'function' ? to(m) : to;
  if (typeof repl !== 'string') throw new EditError('regex: replacement must be a string');
  let stop = a + m.index + m[0].length;
  if (repl === '' && text[stop] === '\n') stop++; // a deleted statement takes its line break with it
  return {
    text: text.slice(0, a + m.index) + repl + text.slice(stop),
    what: `regex ${pattern} after ${preview(anchor)} (matched ${preview(m[0].trim())})`,
  };
}

export function applyOp(text, op) {
  let r;
  if ('del' in op) r = replaceOnce(text, op.del, '', 'del');
  else if ('replace' in op) r = replaceOnce(text, op.replace[0], op.replace[1], 'replace');
  else if ('regex' in op) r = applyRegex(text, op.regex);
  else if ('fn' in op) {
    const out = op.fn(text);
    if (typeof out !== 'string') throw new EditError(`fn ${op.label ?? ''}: callback must return a string`);
    r = { text: out, what: `fn ${op.label ?? '(anonymous)'}` };
  } else throw new EditError(`unknown edit op: ${Object.keys(op).join(',')}`);
  if (r.text === text) throw new EditError(`${r.what}: edit does not change the source (no-op)`);
  return r;
}

/** Apply all ops in order; returns { text, log }. Throws EditError. */
export function applyEdits(src, ops) {
  if (!Array.isArray(ops) || ops.length === 0) throw new EditError('no edits given');
  let text = src;
  const log = [];
  for (const op of ops) {
    const r = applyOp(text, op);
    text = r.text;
    log.push(r.what);
  }
  if (text === src) throw new EditError('edits cancel each other out (result equals the source)');
  return { text, log };
}
