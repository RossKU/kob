import { describe, expect, it } from 'vitest';
import { has } from '../../i18n';
import { MAX_SYSTEM_NOTICES, addNotice, noticeOfSwitch } from './system-notices';

const off = { active: false, reason: null } as const;
const auto = { active: true, reason: 'auto' } as const;

describe('system notifications', () => {
  it('one per switch to the node and one per return; a change that is no switch makes none', () => {
    const to = noticeOfSwitch(off, auto, 1000)!;
    expect(to).toMatchObject({ kind: 'node', reason: 'auto', at: 1000, read: false });
    expect(noticeOfSwitch(auto, off, 2000)).toMatchObject({ kind: 'indexer', reason: null });
    expect(noticeOfSwitch(auto, { active: true, reason: 'manual' }, 3000)).toBeNull();
    expect(noticeOfSwitch(off, { active: true, reason: 'auto', behind: true }, 4000)).toMatchObject({ kind: 'node', behind: true });
    expect(noticeOfSwitch(off, off, 3000)).toBeNull();
    expect(noticeOfSwitch(off, auto, 1000)!.id).not.toBe(to.id);
  });

  it('newest first, a bounded list', () => {
    let list = addNotice([], noticeOfSwitch(off, auto, 1)!);
    list = addNotice(list, noticeOfSwitch(auto, off, 2)!);
    expect(list.map((n) => n.kind)).toEqual(['indexer', 'node']);
    for (let i = 0; i < 30; i++) list = addNotice(list, noticeOfSwitch(off, auto, i)!);
    expect(list).toHaveLength(MAX_SYSTEM_NOTICES);
  });

  it('has its words', () => {
    for (const k of ['notify.system.node.title', 'notify.system.node.auto', 'notify.system.node.manual', 'notify.system.indexer.title', 'notify.system.indexer.body']) expect(has(k), k).toBe(true);
  });
});
