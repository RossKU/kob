// The trigger of the dead-node note (NodeDownNote.tsx): only a node that is known to be unreachable counts; a read that has not answered yet does not.
import { describe, expect, it } from 'vitest';
import { has } from '../../i18n';
import { nodeIsDown } from './status';

describe('the dead-node note', () => {
  it('shows for an unreachable node only: not while checking (undefined), not for a reachable one', () => {
    expect(nodeIsDown({ reachable: false })).toBe(true);
    expect(nodeIsDown({ reachable: true })).toBe(false);
    expect(nodeIsDown({ reachable: undefined })).toBe(false);
  });

  it('its sentence and the Retry label are in the dictionary', () => {
    expect(has('shell.nodeDown.note')).toBe(true);
    expect(has('common.retry')).toBe(true);
  });
});
