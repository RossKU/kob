// Web layer: every destination key of a build request must be the wallet key (or the user's explicit change address).
import { describe, expect, it } from 'vitest';
import { guardRequest, requestKeyProblem } from './request-guard';
import { KobError } from './wasm';
import type { ActionRequest, CancelOrderRequest, CreateOrderRequest } from './types';
import { MAKER, OTHER, goldenRequest, keyUtxo } from '../testing/chain-fixtures';

const create = () => goldenRequest<CreateOrderRequest>('create.ask', MAKER.pk);
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

describe('requestKeyProblem / guardRequest', () => {
  it('an honest create request passes and carries ownKeys for kob-wasm', () => {
    const req = create();
    expect(requestKeyProblem(req, { maker: MAKER.pk })).toBeNull();
    const g = guardRequest(req, { maker: MAKER.pk });
    expect(g.ownKeys).toEqual([MAKER.pk]);
    expect(req).not.toHaveProperty('ownKeys'); // the input is not mutated
  });

  it('an order whose maker is a foreign key is refused before the build', () => {
    const req = clone(create());
    req.order.state.maker = OTHER.pk;
    expect(requestKeyProblem(req, { maker: MAKER.pk })).toMatch(/order maker .* not a key of the connected wallet/);
    expect(() => guardRequest(req, { maker: MAKER.pk })).toThrow(KobError);
  });

  it('a foreign change key is refused unless it is the explicit change address of the user', () => {
    const req = { ...clone(create()), change: OTHER.pk };
    expect(requestKeyProblem(req, { maker: MAKER.pk })).toMatch(/change key/);
    expect(requestKeyProblem(req, { maker: MAKER.pk, changeTo: OTHER.pk })).toBeNull();
    expect(guardRequest(req, { maker: MAKER.pk, changeTo: OTHER.pk }).ownKeys).toEqual([MAKER.pk, OTHER.pk]);
    // ...but a foreign ORDER MAKER is never excused by a change address
    const evil = clone(req);
    evil.order.state.maker = OTHER.pk;
    expect(requestKeyProblem(evil, { maker: MAKER.pk, changeTo: OTHER.pk })).toMatch(/order maker/);
  });

  it('funding inputs of another key, a replacement order of another maker and a cancel of a foreign order are refused', () => {
    const req = clone(create());
    req.funding = [keyUtxo(OTHER.pk, 100_000_000n)];
    expect(requestKeyProblem(req, { maker: MAKER.pk })).toMatch(/funding input/);
    const cancel = { action: 'cancelOrder', order: { state: { kind: 'KobAsk', state: { maker: MAKER.pk } } }, replace: { order: { state: { maker: OTHER.pk } } } } as unknown as CancelOrderRequest;
    expect(requestKeyProblem(cancel, { maker: MAKER.pk })).toMatch(/replacement order maker/);
    const foreign = { action: 'cancelOrder', order: { state: { state: { maker: OTHER.pk } } } } as unknown as ActionRequest;
    expect(requestKeyProblem(foreign, { maker: MAKER.pk })).toMatch(/belongs to/);
  });
});
