// Parser for the output of one ablation-mode test run (`--nocapture`, KOB_ABLATION=1).
//
// Lines of interest (printed by the harness in crates/kob-tests/tests):
//   === KRON template <file>                                  section marker (KRON suites loop over both pinned templates)
//   ABLATION-PASS <scenario name> all_inputs_ok=<bool>         an attack scenario was ACCEPTED (run_bad in ablation mode)
//   ABLATION-POS-FAIL <scenario name> (...)                    a positive scenario was rejected (every suite)
//   NEGATIVE <scenario name>  [REJECTED ...]                   an attack scenario was rejected (normal outcome)
// A scenario id is the first whitespace-delimited token of the scenario name.

export const idOf = name => name.trim().split(/\s+/)[0];

export const shortTemplate = name => name.replace(/^kron_token_/, '').replace(/\.bin$/, '');

/**
 * @param {string} stdout   test stdout
 * @param {string} stderr   test stderr (panic messages, compiler errors)
 * @param {RegExp|null} marker  suite.templateMarker (capture group 1 = section name) or null for single-section suites
 */
export function parseOutput(stdout, stderr, marker) {
  const sections = [];
  const newSection = name => {
    const s = { name, passes: [], negatives: [], posFails: [] };
    sections.push(s);
    return s;
  };
  let cur = marker ? null : newSection('');
  for (const raw of stdout.split('\n')) {
    // --nocapture leaves libtest's "test <name> ... " in front of the first line the test prints
    const line = raw.replace(/\r$/, '').replace(/^test \S+ \.\.\. /, '');
    if (marker) {
      const t = marker.exec(line);
      if (t) {
        cur = newSection(t[1]);
        continue;
      }
    }
    let m;
    if ((m = /^ABLATION-PASS (.*) all_inputs_ok=(true|false)\s*$/.exec(line))) {
      (cur ??= newSection('?')).passes.push({ id: idOf(m[1]), name: m[1], allOk: m[2] === 'true' });
    } else if ((m = /^ABLATION-POS-FAIL (.*?)(?: \(.*)?$/.exec(line))) {
      (cur ??= newSection('?')).posFails.push({ id: idOf(m[1]), name: m[1] });
    } else if ((m = /^NEGATIVE (.*?)\s+\[REJECTED/.exec(line))) {
      (cur ??= newSection('?')).negatives.push(idOf(m[1]));
    }
  }
  const all = stdout + '\n' + stderr;
  const panics = [];
  const lines = all.split('\n');
  for (let i = 0; i < lines.length; i++) {
    if (/panicked at/.test(lines[i])) {
      const msg = (lines[i + 1] ?? '').trim();
      panics.push(msg.length > 200 ? msg.slice(0, 197) + '...' : msg);
    }
  }
  const compileError = /error(\[E\d+\])?: could not compile|error: linking with/.test(all);
  const running = /^running (\d+) tests?\s*$/m.exec(stdout);
  const result = /^test result: (ok|FAILED)\./m.exec(stdout);
  return {
    sections,
    panics: panics.slice(0, 3),
    compileError,
    ran: running ? Number(running[1]) : null,
    testResult: result ? result[1] : null,
  };
}

/** State of scenario `id` in one section: flipped | partial | rejected | mixed | absent. */
export function idState(section, id) {
  const passes = section.passes.filter(p => p.id === id);
  const negs = section.negatives.filter(n => n === id).length;
  if (passes.length === 0) return negs > 0 ? 'rejected' : 'absent';
  if (passes.some(p => !p.allOk)) return 'partial';
  return negs > 0 ? 'mixed' : 'flipped';
}
