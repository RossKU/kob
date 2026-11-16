// The step machine of the Delivery help's animation: play, pause, the loop with its hold on the last step, steps by hand, reduced motion.
import { describe, expect, it } from 'vitest';
import { has } from '../../i18n';
import { END_HOLD_MS, LAST_STEP, STEP_MS, boughtAt, captionOf, delayOf, initialSteps, stepBy, tick, toggle } from './delivery-help-steps';

describe('the Delivery help steps', () => {
  it('opens playing from the empty order; with reduced motion paused on the final state', () => {
    expect(initialSteps(false)).toEqual({ step: 0, playing: true });
    expect(initialSteps(true)).toEqual({ step: LAST_STEP, playing: false });
  });

  it('plays a step per 1.2 s, holds the last step longer and loops to the order', () => {
    let s = initialSteps(false);
    const seen = [s.step];
    for (let i = 0; i < 6; i++) seen.push((s = tick(s)).step);
    expect(seen).toEqual([0, 1, 2, 3, 4, 0, 1]);
    expect(delayOf(1)).toBe(STEP_MS);
    expect(STEP_MS).toBe(1200);
    expect(delayOf(LAST_STEP)).toBe(END_HOLD_MS);
    expect(END_HOLD_MS).toBeGreaterThan(STEP_MS);
  });

  it('pause stops the ticks; play resumes, and on the last step starts again from the order', () => {
    const paused = toggle({ step: 2, playing: true });
    expect(paused).toEqual({ step: 2, playing: false });
    expect(tick(paused)).toEqual(paused);
    expect(toggle(paused)).toEqual({ step: 2, playing: true });
    expect(toggle(initialSteps(true))).toEqual({ step: 0, playing: true });
  });

  it('steps by hand pause and stay within the steps', () => {
    expect(stepBy({ step: 2, playing: true }, 1)).toEqual({ step: 3, playing: false });
    expect(stepBy({ step: 0, playing: false }, -1)).toEqual({ step: 0, playing: false });
    expect(stepBy({ step: LAST_STEP, playing: false }, 1)).toEqual({ step: LAST_STEP, playing: false });
  });

  it('captions and amounts of each step', () => {
    expect(captionOf(0).key).toBe('ticket.dhelp.step.order');
    expect(captionOf(2)).toEqual({ key: 'ticket.dhelp.step.fill', params: { n: '2' } });
    expect(captionOf(LAST_STEP).key).toBe('ticket.dhelp.step.ends');
    for (let i = 0; i <= LAST_STEP; i++) expect(has(captionOf(i).key)).toBe(true);
    expect([0, 1, 2, 3, 4].map(boughtAt)).toEqual([0, 10, 20, 30, 30]);
  });

  it('every sentence of the help exists', () => {
    for (const m of ['each', 'collected', 'topup']) {
      for (const k of ['result', 'cost', 'aria', 'wait', 'for', 'tradeoff']) expect(has(`ticket.dhelp.${k}.${m}`), `${k}.${m}`).toBe(true);
      // the long form of the select and Settings, the short one of the help and the order details, and the hint under the select
      for (const k of ['opt', 'short']) expect(has(`ticket.${k}.delivery.${m}`), `${k}.${m}`).toBe(true);
      expect(has(`ticket.delivery.hint.${m}`), `hint.${m}`).toBe(true);
    }
  });
});
