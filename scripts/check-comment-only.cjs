// Checks that an edit changed only full-line `//` comments and blank lines.
//   node scripts/check-comment-only.cjs <rev> <file...>   compare each file with its version at git <rev>
//   node scripts/check-comment-only.cjs --files <old> <new>   compare two files
// Run it from the repository root. See "Code style" in docs/development.md.
//
// A line is dropped when, after trimming, it is empty or starts with `//`. Every other line must be
// byte-identical and in the same order. That includes indentation, trailing comments and trailing
// whitespace. This is the rule of `strip_sil` in scripts/build-argent.sh. Exit code 1 on any difference.
//
// Works for .sil, .ag, .rs and .ts files. Limits:
// - A `/* */` block, or a multi-line string literal whose lines start with `//`, counts as comment text.
// - Do not use it on files that embed contract source in string literals (kob-protocol/src/kcc20.rs).
const fs = require('fs');
const { execFileSync } = require('child_process');

// --block (first argument, .ts and .tsx only) also drops JSDoc lines: trimmed text starting with `/*` or `*`.
let block = false;
if (process.argv[2] === '--block') { block = true; process.argv.splice(2, 1); }
const view = (src) => src.replace(/\r\n/g, '\n').split('\n').filter((l) => {
  const t = l.trim();
  return t !== '' && !t.startsWith('//') && !(block && (t.startsWith('/*') || t.startsWith('*')));
});

function compare(name, a, b) {
  const x = view(a), y = view(b);
  for (let k = 0; k < Math.max(x.length, y.length); k++) {
    if (x[k] !== y[k]) {
      console.log(`CODE CHANGED ${name}: code line ${k + 1}\n  - ${x[k] ?? '<none>'}\n  + ${y[k] ?? '<none>'}`);
      return false;
    }
  }
  console.log(`same code    ${name} (${x.length} code lines)`);
  return true;
}

const args = process.argv.slice(2);
let ok = true;
if (args[0] === '--files') {
  ok = compare(args[2], fs.readFileSync(args[1], 'utf8'), fs.readFileSync(args[2], 'utf8'));
} else {
  const [rev, ...files] = args;
  if (!rev || files.length === 0) {
    console.log('usage: check-comment-only.cjs [--block] <rev> <file...> | --files <old> <new>');
    process.exit(2);
  }
  for (const f of files) {
    let old;
    try {
      old = execFileSync('git', ['show', `${rev}:${f}`], { encoding: 'utf8', maxBuffer: 1 << 28 });
    } catch {
      console.log(`NEW FILE     ${f}`);
      ok = false;
      continue;
    }
    ok = compare(f, old, fs.readFileSync(f, 'utf8')) && ok;
  }
}
process.exit(ok ? 0 : 1);
