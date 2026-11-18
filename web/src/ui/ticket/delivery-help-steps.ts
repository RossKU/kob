// The step machine of the Delivery help's animation (DeliveryHelp.tsx), pure so it is tested
// without a browser. A buy of 30 filled three times by 10: step 0 is the empty order, steps 1 to 3
// are the fills, and step 4 ends the order (Collected sends its custody to the wallet, every
// lane shows its result). It plays STEP_MS per step and holds the last step END_HOLD_MS before it
// loops. With reduced motion it opens paused on the last step. The step buttons still work.

/** The steps: 0 the order, 1..3 the fills, 4 the order ends. */
export const LAST_STEP = 4;
export const STEP_MS = 1200;
/** The last step stays this long before the loop starts again. */
export const END_HOLD_MS = 2400;

export interface StepState {
  step: number;
  playing: boolean;
}

/**
 * Where the help opens: playing from the empty order, or (reduced motion) paused on the final
 * state.
 */
export const initialSteps = (reducedMotion: boolean): StepState => (reducedMotion ? { step: LAST_STEP, playing: false } : { step: 0, playing: true });

/** How long the step shows before the next one while playing. */
export const delayOf = (step: number): number => (step >= LAST_STEP ? END_HOLD_MS : STEP_MS);

/** The timer's next step: one on, and the last one loops to the order. */
export const nextStep = (step: number): number => (step >= LAST_STEP ? 0 : step + 1);

/** Play / pause. Play on the last step starts the loop again from the order. */
export const toggle = (s: StepState): StepState => (s.playing ? { ...s, playing: false } : { step: s.step >= LAST_STEP ? 0 : s.step, playing: true });

/** A step by hand (back / forward). It pauses, within 0..LAST_STEP. */
export const stepBy = (s: StepState, by: -1 | 1): StepState => ({ step: Math.min(LAST_STEP, Math.max(0, s.step + by)), playing: false });

/** The timer ticked. Paused: nothing changes. */
export const tick = (s: StepState): StepState => (s.playing ? { ...s, step: nextStep(s.step) } : s);

/** What the caption says: the order, "Fill n of 3", the order ends. */
export const captionOf = (step: number): { key: string; params?: Record<string, string> } =>
  step <= 0 ? { key: 'ticket.dhelp.step.order' } : step >= LAST_STEP ? { key: 'ticket.dhelp.step.ends' } : { key: 'ticket.dhelp.step.fill', params: { n: String(step) } };

/** Tokens bought so far at a step (10 per fill, at most 30). */
export const boughtAt = (step: number): number => 10 * Math.min(3, Math.max(0, step));
