// The audit log: one JSON line per tool call (arguments, outcome, plan id, transaction ids).

import { appendFileSync, mkdirSync } from 'node:fs';
import { dirname } from 'node:path';

export class Audit {
  constructor(file) {
    this.file = file;
  }

  log(e) {
    mkdirSync(dirname(this.file), { recursive: true });
    appendFileSync(this.file, `${JSON.stringify({ ts: new Date().toISOString(), ...e })}\n`);
  }
}
