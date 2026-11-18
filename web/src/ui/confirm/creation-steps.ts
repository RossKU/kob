// The stepper of a token creation (KCC-23: the preparation, then the genesis). It shows while the
// creation is signed on the confirmation screen (ConfirmSign.tsx, from the chain runner's stages).
// For a creation not finished it shows from its record (data/creations.ts) on the Create token
// page.
import { t } from '../../i18n';
import type { StepperItem } from '../kit/stepper-model';
import type { CreationStep } from '../../wallet/create';
import type { SignStage } from '../../wallet/sign';

export interface CreationProgress {
  /** The chain runner's stage, null before the first signature. */
  stage: SignStage | null;
  /** Which transaction the stage is of. */
  step: CreationStep | null;
  /** The wallet is not asked yet, because its node does not see the preparation. */
  waiting?: boolean;
  /** The node took the preparation. */
  prepSent: boolean;
  /** The wallet signs both on one approval (the private-key signer). */
  oneApproval?: boolean;
  /** The flow stopped at the step it was on. */
  stopped?: 'declined' | 'failed' | null;
}

const prepItem = () => ({ label: t('stepper.step.prepare'), testid: 'create-step-prep' });
const genesisItem = () => ({ label: t('stepper.step.create'), testid: 'create-step-genesis' });

function stageNote(stage: SignStage, o: { waiting?: boolean; both?: boolean }): string {
  if (stage === 'signing') return t(o.waiting ? 'stepper.note.waitWallet' : o.both ? 'stepper.note.signBoth' : 'stepper.note.sign');
  if (stage === 'submitting') return t('stepper.note.sending');
  return t('stepper.note.checking');
}

const stoppedItem = (stopped: 'declined' | 'failed'): Pick<StepperItem, 'state' | 'note'> => ({ state: 'failed', note: t(stopped === 'declined' ? 'stepper.note.declined' : 'stepper.note.failed') });

/** Prepare, Create token: on the confirmation screen while the creation is signed and sent. */
export function creationStepperItems(p: CreationProgress): StepperItem[] {
  if (p.stage === 'submitted') return [{ ...prepItem(), state: 'done' }, { ...genesisItem(), state: 'done' }];
  const prepDone = p.prepSent || p.step === 'genesis';
  const prep: StepperItem = prepDone
    ? { ...prepItem(), state: 'done' }
    : p.stopped
      ? { ...prepItem(), ...stoppedItem(p.stopped) }
      : p.stage
        ? { ...prepItem(), state: 'current', busy: true, note: stageNote(p.stage, { both: !!p.oneApproval }) }
        : { ...prepItem(), state: 'waiting' };
  const genesis: StepperItem = !prepDone
    ? { ...genesisItem(), state: 'waiting' }
    : p.stopped
      ? { ...genesisItem(), ...stoppedItem(p.stopped) }
      : { ...genesisItem(), state: 'current', busy: true, note: stageNote(p.stage ?? 'signing', { waiting: !!p.waiting }) };
  return [prep, genesis];
}

export interface PendingCreation {
  /** The record says the node took the preparation. */
  prepSent: boolean;
  /** The record holds the genesis signed. */
  signed: boolean;
  /** "Finish the creation" runs. */
  resuming: boolean;
  /**
   * What the wallet is asked for meanwhile (IssueView's resume note): its node catching up, or the
   * genesis.
   */
  note: 'wait' | 'genesis' | null;
  /** The last attempt failed. */
  failed: boolean;
}

/**
 * Prepare, Create token: for a creation not finished, from its record. A page opened later shows
 * the step it stopped at.
 */
export function pendingCreationItems(r: PendingCreation): StepperItem[] {
  // The wallet is asked for the genesis only once the preparation is on the node.
  const prepDone = r.prepSent || (r.resuming && r.note !== null);
  const prep: StepperItem = prepDone
    ? { ...prepItem(), state: 'done' }
    : r.resuming
      ? { ...prepItem(), state: 'current', busy: true, note: t('stepper.note.sending') }
      : r.failed
        ? { ...prepItem(), ...stoppedItem('failed') }
        : { ...prepItem(), state: 'current', note: t('stepper.note.notSent') };
  const genesis: StepperItem = !prepDone
    ? { ...genesisItem(), state: 'waiting' }
    : r.resuming
      ? { ...genesisItem(), state: 'current', busy: true, note: t(r.note === 'wait' ? 'stepper.note.waitWallet' : r.note === 'genesis' ? 'stepper.note.sign' : 'stepper.note.sending') }
      : r.failed
        ? { ...genesisItem(), ...stoppedItem('failed') }
        : { ...genesisItem(), state: 'current', note: t(r.signed ? 'stepper.note.signedNotSent' : 'stepper.note.notSigned') };
  return [prep, genesis];
}
