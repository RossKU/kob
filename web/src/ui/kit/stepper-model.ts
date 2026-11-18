// The model of the signing stepper (kit/Stepper.tsx): one entry per signature of a chained flow,
// each done, current, waiting or failed. The flows map their own state onto it
// (orders/flow-steps.ts, confirm/creation-steps.ts). Nothing here knows a flow.

export type StepperState = 'done' | 'current' | 'waiting' | 'failed';

export interface StepperItem {
  /** Short: "Prepare", "Merge 1 of 3", "Sell". */
  label: string;
  state: StepperState;
  /** One short line under the label ("Sign in your wallet", "Declined"). */
  note?: string;
  /** Working now: a spinner in its circle. */
  busy?: boolean;
  /** The item's test id, and the flow's own state for `data-state` (e2e reads both). */
  testid?: string;
  data?: string;
}

/**
 * Up to this many steps sit in a row (○→○→○). A longer run is a column, which scrolls in
 * its card.
 */
export const STEPPER_ROW_MAX = 3;

export const stepperLayout = (count: number): 'row' | 'column' => (count <= STEPPER_ROW_MAX ? 'row' : 'column');

/** The step that is current (aria-current), or -1. */
export const currentStep = (items: readonly StepperItem[]): number => items.findIndex((s) => s.state === 'current');

/** What the circle shows: a tick, a cross, or the step's number. */
export const stepGlyph = (s: Pick<StepperItem, 'state'>, index: number): string => (s.state === 'done' ? '✓' : s.state === 'failed' ? '✗' : String(index + 1));
