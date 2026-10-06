// The order-type matrix: one entry per order type and side of docs/spec/order-types.md, with what the ticket disclosure, the pre-sign confirmation
// screen, the accepted transaction (indexer state fields) and My orders must show. Every expected figure follows from the typed inputs and
// the protocol rules (prices in KAS per EXKCC; amounts in EXKCC, 1 EXKCC = 1e8 base units = the order scale; the default seed rests around 0.025 KAS
// with bids <= 0.0249 and asks >= 0.0251;
// e.g. a 3% market bound is 0.0249 x 0.97 = 0.024153, a sell-first IFO prefund is stop-worst 0.02781 - entry 0.026 = 0.00181): a wrong number in
// the UI fails the test.
//
// Price fields of the on-chain state are sompi per whole token (per `scale` = 1e8 base units): 0.027 KAS / EXKCC = 2_700_000; amounts are base
// units. All wallet defaults come from docs/spec/order-types.md and kob-wasm `defaultConstants` (tip 0, carrier 2 KAS per covenant UTXO, market 3%
// over 20 s, stop 3% band over 30 s, rest time R 5 s, trigger threshold = the minimum fill, keeper tip 0.021 KAS on the 8/8 program, refund tip
// 0.05 KAS). Minimum fill defaults (matcher.md 10): a resting order the amount worth 10 KAS (a notional, not a carrier) at its price, capped at the whole
// amount (so every 2-token order here is filled whole: ONE delivery carrier of a bid); IOC / FOK / market 1 base unit; an IFD entry a quarter of
// the amount (four fills: four delivery carriers and four prefunded exit carriers).
import type { FieldValues, Side } from '../helpers/ticket';

/** What a case may need from the browser / mock clock to type dates. */
export interface Ctx {
  /** `Date.getTimezoneOffset()` of the browser */
  tz: number;
  /** the mock node's UTC unix seconds when the test started */
  unix: number;
}

export type Timing =
  | { kind: 'gtc' }
  | { kind: 'day' }
  | { kind: 'gtd'; afterSeconds: number }
  | { kind: 'timed'; afterSeconds: number }
  | { kind: 'ioc' }
  | { kind: 'auction' }
  | { kind: 'none' };

export interface ExitExpect {
  /** the title line of the nested exit card */
  title: string;
  rows: Record<string, string>;
  /** kind and fields of the exit state committed inside the entry (decoded with kob-wasm) */
  state: { kind: string; fields: Record<string, string> };
}

export interface OrderCase {
  name: string;
  type: string;
  side: Side;
  fields: (c: Ctx) => FieldValues;
  /** covenant the transaction creates */
  kind: string;
  /** My orders type key (`data-type`) and its English label */
  listType: string;
  listLabel: string;
  /** live disclosure rows: each row id must exist and contain the substring (an empty substring only asserts existence); rows that must NOT exist */
  disc: Record<string, string>;
  discAbsent?: string[];
  notes: string[];
  carriers: Record<string, string>;
  locked: string;
  escrowed: string | null;
  /** confirmation screen: the created order's card */
  title: string;
  rows: Record<string, string>;
  exit?: ExitExpect;
  /** exact fields of the state the indexer holds after acceptance */
  state: Record<string, string>;
  timing: Timing;
}

const SELL_CARRIERS = { orderCarrier: '2 KAS', tokenCarrier: '2 KAS' };
const kas = (v: string) => `${v} KAS / EXKCC`;

// ------------------------------------------------------------------------------------------------ limit family
const limit = (side: Side): Pick<OrderCase, 'type' | 'side' | 'kind' | 'listType' | 'listLabel'> => ({
  type: 'limit', side, kind: side === 'sell' ? 'KobAsk' : 'KobBid', listType: 'limit', listLabel: 'Limit (GTC)',
});

const SELL = { locked: '4 KAS', escrowed: '2 EXKCC', carriers: SELL_CARRIERS } as const;
/** a 2-token resting buy at price p (KAS per token): escrow 2p, one delivery carrier (the default minimum fill is the whole amount) */
const buyLimit = (escrowKas: string) => ({ locked: `${(2 + Number(escrowKas)).toFixed(3).replace(/0+$/, '').replace(/\.$/, '')} KAS`, escrowed: null, carriers: { escrow: `${escrowKas} KAS`, deliveryCarrier: '2 KAS' } });

export const CASES: OrderCase[] = [
  // ---- limit GTC
  {
    name: 'limit GTC sell', ...limit('sell'), ...SELL, fields: () => ({ amount: '2', price: '0.027' }),
    disc: { limit: kas('0.027'), allInPrice: kas('0.027'), allInTotal: '0.054 KAS', refundTip: '0.05 KAS', 'expiry-extra': '' },
    discAbsent: ['tip', 'activates'], notes: ['gtc', 'carrierReturned'],
    title: 'Sell limit order', rows: { amount: '2 EXKCC', price: kas('0.027'), allIn: kas('0.027'), refundTip: '0.05 KAS', value: '2 KAS' },
    state: { price: '2700000', tip: '0', tif: '0', activeFrom: '0', amountLeft: '200000000', refundTip: '5000000', scale: '100000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'limit GTC buy', ...limit('buy'), ...buyLimit('0.049'), fields: () => ({ amount: '2', price: '0.0245' }),
    disc: { limit: kas('0.0245'), allInPrice: kas('0.0245'), allInTotal: '0.049 KAS', refundTip: '0.05 KAS' },
    discAbsent: ['tip', 'activates'], notes: ['gtc', 'carrierReturned'],
    title: 'Buy limit order', rows: { price: kas('0.0245'), allIn: kas('0.0245'), refundTip: '0.05 KAS', value: '2.049 KAS' },
    state: { price: '2450000', tip: '0', tif: '0', activeFrom: '0', reserve: '0', deliveryCarrier: '200000000', refundTip: '5000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'limit sell with a priority tip: all-in is limit minus tip', ...limit('sell'), ...SELL, fields: () => ({ amount: '2', price: '0.027', tip: '0.001' }),
    disc: { limit: kas('0.027'), tip: kas('0.001'), allInPrice: kas('0.026'), allInTotal: '0.052 KAS' },
    notes: ['gtc'],
    title: 'Sell limit order', rows: { price: kas('0.027'), allIn: kas('0.026'), tip: kas('0.001') },
    state: { price: '2700000', tip: '100000', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'limit buy with a priority tip: all-in is limit plus tip', ...limit('buy'), ...buyLimit('0.051'), fields: () => ({ amount: '2', price: '0.0245', tip: '0.001' }),
    disc: { limit: kas('0.0245'), tip: kas('0.001'), allInPrice: kas('0.0255'), allInTotal: '0.051 KAS' },
    notes: ['gtc'],
    title: 'Buy limit order', rows: { price: kas('0.0245'), allIn: kas('0.0255'), tip: kas('0.001') },
    state: { price: '2450000', tip: '100000' },
    timing: { kind: 'gtc' },
  },
  // ---- limit GTD (date)
  {
    name: 'limit GTD sell', ...limit('sell'), listType: 'limit-gtd', listLabel: 'Limit (until date)', ...SELL,
    fields: (c) => ({ amount: '2', price: '0.027', lifetime: 'gtd', lifetimeAt: localAfter(c, 3 * 86_400) }),
    disc: { limit: kas('0.027'), allInPrice: kas('0.027') }, discAbsent: ['expiry-extra', 'tip'], notes: ['gtd', 'carrierReturned'],
    title: 'Sell limit order', rows: { price: kas('0.027') },
    state: { price: '2700000', tif: '0', amountLeft: '200000000' },
    timing: { kind: 'gtd', afterSeconds: 3 * 86_400 },
  },
  {
    name: 'limit GTD buy', ...limit('buy'), listType: 'limit-gtd', listLabel: 'Limit (until date)', ...buyLimit('0.049'),
    fields: (c) => ({ amount: '2', price: '0.0245', lifetime: 'gtd', lifetimeAt: localAfter(c, 3 * 86_400) }),
    disc: { limit: kas('0.0245'), allInTotal: '0.049 KAS' }, discAbsent: ['expiry-extra'], notes: ['gtd'],
    title: 'Buy limit order', rows: { price: kas('0.0245') },
    state: { price: '2450000', tif: '0' },
    timing: { kind: 'gtd', afterSeconds: 3 * 86_400 },
  },
  // ---- limit day (until 00:00 UTC)
  {
    name: 'limit day sell (ends 00:00 UTC)', ...limit('sell'), listType: 'limit-day', listLabel: 'Limit (day order)', ...SELL,
    fields: () => ({ amount: '2', price: '0.027', lifetime: 'day' }),
    disc: { limit: kas('0.027'), expiry: '00:00 UTC' }, discAbsent: ['expiry-extra'], notes: ['dayOrder', 'carrierReturned'],
    title: 'Sell day limit order', rows: { price: kas('0.027'), deadline: '00:00 UTC' },
    state: { price: '2700000', tif: '0' },
    timing: { kind: 'day' },
  },
  {
    name: 'limit day buy (ends 00:00 UTC)', ...limit('buy'), listType: 'limit-day', listLabel: 'Limit (day order)', ...buyLimit('0.049'),
    fields: () => ({ amount: '2', price: '0.0245', lifetime: 'day' }),
    disc: { limit: kas('0.0245'), expiry: '00:00 UTC' }, discAbsent: ['expiry-extra'], notes: ['dayOrder'],
    title: 'Buy day limit order', rows: { price: kas('0.0245'), deadline: '00:00 UTC' },
    state: { price: '2450000', tif: '0' },
    timing: { kind: 'day' },
  },
  // ---- timed activation
  {
    name: 'limit with timed activation, sell', ...limit('sell'), listType: 'timed', listLabel: 'Limit (timed start)', ...SELL,
    fields: (c) => ({ amount: '2', price: '0.027', activeFrom: localAfter(c, 2 * 3600) }),
    disc: { limit: kas('0.027') }, notes: ['gtc'],
    title: 'Sell limit order', rows: { price: kas('0.027') },
    state: { price: '2700000', tif: '0' },
    timing: { kind: 'timed', afterSeconds: 2 * 3600 },
  },
  {
    name: 'limit with timed activation, buy', ...limit('buy'), listType: 'timed', listLabel: 'Limit (timed start)', ...buyLimit('0.049'),
    fields: (c) => ({ amount: '2', price: '0.0245', activeFrom: localAfter(c, 2 * 3600) }),
    disc: { limit: kas('0.0245') }, notes: ['gtc'],
    title: 'Buy limit order', rows: { price: kas('0.0245') },
    state: { price: '2450000', tif: '0' },
    timing: { kind: 'timed', afterSeconds: 2 * 3600 },
  },
  // ---- IOC / FOK
  {
    name: 'IOC sell', type: 'ioc', side: 'sell', kind: 'KobAsk', listType: 'ioc', listLabel: 'IOC', ...SELL,
    fields: () => ({ amount: '2', price: '0.0245' }),
    disc: { limit: kas('0.0245'), allInPrice: kas('0.0245'), allInTotal: '0.049 KAS' }, notes: ['iocRemainderReturned', 'carrierReturned'],
    title: 'Sell IOC limit order', rows: { price: kas('0.0245'), tif: 'immediate or cancel' },
    state: { price: '2450000', tif: '1', amountLeft: '200000000' },
    timing: { kind: 'ioc' },
  },
  {
    name: 'IOC buy', type: 'ioc', side: 'buy', kind: 'KobBid', listType: 'ioc', listLabel: 'IOC', locked: '2.053 KAS', escrowed: null, carriers: { escrow: '0.053 KAS', deliveryCarrier: '2 KAS' },
    fields: () => ({ amount: '2', price: '0.0265' }),
    disc: { limit: kas('0.0265'), allInTotal: '0.053 KAS' }, notes: ['iocRemainderReturned'],
    title: 'Buy IOC limit order', rows: { price: kas('0.0265'), tif: 'immediate or cancel' },
    state: { price: '2650000', tif: '1', deliveryCarrier: '200000000' },
    timing: { kind: 'ioc' },
  },
  {
    name: 'FOK sell', type: 'fok', side: 'sell', kind: 'KobAsk', listType: 'fok', listLabel: 'FOK', ...SELL,
    fields: () => ({ amount: '2', price: '0.0245' }),
    disc: { limit: kas('0.0245'), allInTotal: '0.049 KAS' }, notes: ['fokAllOrNothing', 'carrierReturned'],
    title: 'Sell FOK limit order', rows: { price: kas('0.0245'), tif: 'fill or kill' },
    state: { price: '2450000', tif: '2', amountLeft: '200000000' },
    timing: { kind: 'ioc' },
  },
  {
    name: 'FOK buy', type: 'fok', side: 'buy', kind: 'KobBid', listType: 'fok', listLabel: 'FOK', locked: '2.053 KAS', escrowed: null, carriers: { escrow: '0.053 KAS', deliveryCarrier: '2 KAS' },
    fields: () => ({ amount: '2', price: '0.0265' }),
    disc: { limit: kas('0.0265'), allInTotal: '0.053 KAS' }, notes: ['fokAllOrNothing'],
    title: 'Buy FOK limit order', rows: { price: kas('0.0265'), tif: 'fill or kill' },
    state: { price: '2650000', tif: '2' },
    timing: { kind: 'ioc' },
  },
  // ---- market (expected vs worst: the 3% slippage bound of the auction)
  {
    name: 'market sell: expected best bid, worst 3% below', type: 'market', side: 'sell', kind: 'KobAsk', listType: 'market', listLabel: 'Market', ...SELL,
    fields: () => ({ amount: '2' }),
    disc: { expected: kas('0.0249'), worst: kas('0.024153'), allInPrice: kas('0.024153'), allInTotal: '0.048306 KAS' }, notes: ['market', 'auction', 'iocRemainderReturned'],
    title: 'Sell market order (auction)', rows: { price: kas('0.0249'), auction: `${kas('0.024153')} | the price moves over 20 s`, tif: 'immediate or cancel' },
    state: { price: '2490000', priceEnd: '2415300', tif: '1', amountLeft: '200000000' },
    timing: { kind: 'auction' },
  },
  {
    name: 'market buy: expected best ask, worst 3% above', type: 'market', side: 'buy', kind: 'KobBid', listType: 'market', listLabel: 'Market', locked: '2.051706 KAS', escrowed: null, carriers: { escrow: '0.051706 KAS', deliveryCarrier: '2 KAS' },
    fields: () => ({ amount: '2' }),
    disc: { expected: kas('0.0251'), worst: kas('0.025853'), allInPrice: kas('0.025853'), allInTotal: '0.051706 KAS' }, notes: ['market', 'auction'],
    title: 'Buy market order (auction)', rows: { price: kas('0.0251'), auction: `${kas('0.025853')} | the price moves over 20 s`, tif: 'immediate or cancel' },
    state: { price: '2510000', priceEnd: '2585300', tif: '1' },
    timing: { kind: 'auction' },
  },
  {
    name: 'market sell with a 1% slippage bound', type: 'market', side: 'sell', kind: 'KobAsk', listType: 'market', listLabel: 'Market', ...SELL,
    fields: () => ({ amount: '2', slippageBps: '1' }),
    disc: { expected: kas('0.0249'), worst: kas('0.024651') }, notes: ['market'],
    title: 'Sell market order (auction)', rows: { auction: `${kas('0.024651')} | the price moves over 20 s` },
    state: { price: '2490000', priceEnd: '2465100', tif: '1' },
    timing: { kind: 'auction' },
  },
  // ---- streaming (quote and execute from the displayed price)
  {
    name: 'streaming buy from the displayed price', type: 'streaming', side: 'buy', kind: 'KobBid', listType: 'market', listLabel: 'Market', locked: '2.051706 KAS', escrowed: null, carriers: { escrow: '0.051706 KAS', deliveryCarrier: '2 KAS' },
    fields: () => ({ amount: '2', displayedPrice: '0.0251' }),
    disc: { expected: kas('0.0251'), worst: kas('0.025853') }, notes: ['streaming', 'auction'],
    title: 'Buy market order (auction)', rows: { price: kas('0.0251'), auction: `${kas('0.025853')} | the price moves over 20 s` },
    state: { price: '2510000', priceEnd: '2585300', tif: '1' },
    timing: { kind: 'auction' },
  },
  {
    name: 'streaming sell from the displayed price', type: 'streaming', side: 'sell', kind: 'KobAsk', listType: 'market', listLabel: 'Market', ...SELL,
    fields: () => ({ amount: '2', displayedPrice: '0.0249' }),
    disc: { expected: kas('0.0249'), worst: kas('0.024153') }, notes: ['streaming', 'auction'],
    title: 'Sell market order (auction)', rows: { price: kas('0.0249'), auction: `${kas('0.024153')} | the price moves over 20 s` },
    state: { price: '2490000', priceEnd: '2415300', tif: '1' },
    timing: { kind: 'auction' },
  },
  {
    name: 'close (sell the held tokens at market)', type: 'close', side: 'sell', kind: 'KobAsk', listType: 'market', listLabel: 'Market', ...SELL,
    fields: () => ({ amount: '2' }),
    disc: { expected: kas('0.0249'), worst: kas('0.024153') }, notes: ['close', 'market', 'auction'],
    title: 'Sell market order (auction)', rows: { price: kas('0.0249'), auction: `${kas('0.024153')} | the price moves over 20 s` },
    state: { price: '2490000', priceEnd: '2415300', tif: '1', amountLeft: '200000000' },
    timing: { kind: 'auction' },
  },
  // ---- TWAP / DCA
  {
    name: 'TWAP: sell 6 EXKCC, at most 2 per 10 minutes', type: 'twap', side: 'sell', kind: 'KobAsk', listType: 'twap', listLabel: 'TWAP', locked: '4 KAS', escrowed: '6 EXKCC', carriers: SELL_CARRIERS,
    fields: () => ({ amount: '6', sliceAmount: '2', interval: '10', price: '0.0245' }),
    disc: { limit: kas('0.0245'), allInTotal: '0.147 KAS' }, notes: ['twap', 'gtc'],
    title: 'Sell TWAP schedule', rows: { amount: '6 EXKCC', price: kas('0.0245'), schedule: 'at most 2 EXKCC every 600 s' },
    state: { price: '2450000', interval: '6000', maxFill: '200000000', amountLeft: '600000000', tif: '0' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'DCA: buy 6 EXKCC, at most 2 per 10 minutes', type: 'dca', side: 'buy', kind: 'KobBid', listType: 'dca', listLabel: 'DCA', locked: '6.15600002 KAS', escrowed: null, carriers: { escrow: '0.15600002 KAS', deliveryCarrier: '2 KAS x 3 = 6 KAS' },
    fields: () => ({ amount: '6', sliceAmount: '2', interval: '10', price: '0.026' }),
    disc: { limit: kas('0.026'), allInTotal: '0.156 KAS' }, notes: ['dca', 'gtc'],
    title: 'Buy DCA schedule', rows: { price: kas('0.026'), schedule: 'at most 2 EXKCC every 600 s' },
    state: { price: '2600000', interval: '6000', maxFill: '200000000', tif: '0', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  // ---- Dutch / rising bid
  {
    name: 'Dutch sell: 0.027 falling to 0.0255 over 10 minutes', type: 'dutch', side: 'sell', kind: 'KobAsk', listType: 'dutch', listLabel: 'Dutch / price decay', ...SELL,
    fields: () => ({ amount: '2', price: '0.027', priceEnd: '0.0255', duration: '10' }),
    disc: { expected: kas('0.027'), worst: kas('0.0255'), allInPrice: kas('0.0255') }, notes: ['dutch', 'auction'],
    title: 'Sell declining-price (Dutch) order', rows: { price: kas('0.027'), auction: `${kas('0.0255')} | the price moves over 600 s` },
    state: { price: '2700000', priceEnd: '2550000', tif: '0', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'rising bid: buy at 0.024 rising to 0.0255 over 10 minutes', type: 'dutch', side: 'buy', kind: 'KobBid', listType: 'dutch', listLabel: 'Dutch / price decay', locked: '2.051 KAS', escrowed: null, carriers: { escrow: '0.051 KAS', deliveryCarrier: '2 KAS' },
    fields: () => ({ amount: '2', price: '0.024', priceEnd: '0.0255', duration: '10' }),
    disc: { expected: kas('0.024'), worst: kas('0.0255'), allInPrice: kas('0.0255') }, notes: ['rising', 'auction'],
    title: 'Buy rising-bid order', rows: { price: kas('0.024'), auction: `${kas('0.0255')} | the price moves over 600 s` },
    state: { price: '2400000', priceEnd: '2550000', tif: '0' },
    timing: { kind: 'gtc' },
  },
  // ---- stop-market / stop-limit / trailing
  {
    name: 'stop-market sell', type: 'stopMarket', side: 'sell', kind: 'KobCondAsk', listType: 'stop', listLabel: 'Stop', ...SELL,
    fields: () => ({ amount: '2', stop: '0.023' }),
    disc: { stop: kas('0.023'), stopWorst: kas('0.02231'), trigger: '5 s', keeper: '0.021 KAS x 1', allInPrice: kas('0.02231') }, notes: ['stopTrigger', 'stopAuction', 'triggerExposure', 'keeperReserve'],
    title: 'Sell stop order', rows: { stop: kas('0.023'), band: '3% band, 30 s auction', exposure: '5 s', keeperTip: '0.021 KAS', armed: 'not yet armed' },
    state: { stopPrice: '2300000', tpPrice: '0', slipBps: '300', bandDaa: '300', minRestDaa: '50', armed: '0', keeperTip: '2100000', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'stop-market buy', type: 'stopMarket', side: 'buy', kind: 'KobCondBid', listType: 'stop', listLabel: 'Stop', locked: '2.07662 KAS', escrowed: null,
    carriers: { escrow: '0.05562 KAS', deliveryCarrier: '2 KAS', keeperReserve: '0.021 KAS' },
    fields: () => ({ amount: '2', stop: '0.027' }),
    disc: { stop: kas('0.027'), stopWorst: kas('0.02781'), trigger: '5 s', keeper: '0.021 KAS x 1' }, notes: ['stopTrigger', 'stopAuction', 'keeperReserve'],
    title: 'Buy stop order', rows: { stop: kas('0.027'), band: '3% band, 30 s auction', armed: 'not yet armed' },
    state: { stopPrice: '2700000', tpPrice: '0', slipBps: '300', bandDaa: '300', armed: '0', keeperTip: '2100000', amountLeft: '200000000', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'stop-limit sell: the band is the distance to the limit', type: 'stopLimit', side: 'sell', kind: 'KobCondAsk', listType: 'stop', listLabel: 'Stop', ...SELL,
    fields: () => ({ amount: '2', stop: '0.023', limit: '0.0225' }),
    disc: { stop: kas('0.023'), stopWorst: kas('0.0225009'), trigger: '2.17% band' }, notes: ['stopTrigger', 'stopLimitMayNotFill', 'keeperReserve'],
    title: 'Sell stop order', rows: { stop: kas('0.023'), band: '2.17% band, 30 s auction' },
    state: { stopPrice: '2300000', slipBps: '217', bandDaa: '300', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'stop-limit buy: the band is the distance to the limit', type: 'stopLimit', side: 'buy', kind: 'KobCondBid', listType: 'stop', listLabel: 'Stop', locked: '2.075999 KAS', escrowed: null,
    carriers: { escrow: '0.054999 KAS', deliveryCarrier: '2 KAS', keeperReserve: '0.021 KAS' },
    fields: () => ({ amount: '2', stop: '0.027', limit: '0.0275' }),
    disc: { stop: kas('0.027'), stopWorst: kas('0.0274995'), trigger: '1.85% band' }, notes: ['stopLimitMayNotFill'],
    title: 'Buy stop order', rows: { stop: kas('0.027'), band: '1.85% band, 30 s auction' },
    state: { stopPrice: '2700000', slipBps: '185', bandDaa: '300', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'trailing stop sell', type: 'trailingStop', side: 'sell', kind: 'KobCondAsk', listType: 'trailing', listLabel: 'Trailing stop', ...SELL,
    fields: () => ({ amount: '2', stop: '0.023', 'trail.step': '0.0005', 'trail.gap': '0.001' }),
    disc: { stop: kas('0.023'), trail: 'step 0.0005 KAS, gap 0.001 KAS', keeper: '0.021 KAS x 21' }, notes: ['trailing', 'stopTrigger', 'keeperReserve'],
    title: 'Sell trailing stop', rows: { stop: kas('0.023'), trail: 'step 0.0005 KAS, gap 0.001 KAS, one update per 10 min at most' },
    state: { stopPrice: '2300000', trailStep: '50000', trailGap: '100000', trailWait: '6000', armed: '0', keeperTip: '2100000', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'trailing stop buy', type: 'trailingStop', side: 'buy', kind: 'KobCondBid', listType: 'trailing', listLabel: 'Trailing stop', locked: '2.49662 KAS', escrowed: null,
    carriers: { escrow: '0.05562 KAS', deliveryCarrier: '2 KAS', keeperReserve: '0.021 KAS x 21 = 0.441 KAS' },
    fields: () => ({ amount: '2', stop: '0.027', 'trail.step': '0.0005', 'trail.gap': '0.001' }),
    disc: { stop: kas('0.027'), trail: 'step 0.0005 KAS, gap 0.001 KAS', keeper: '0.021 KAS x 21' }, notes: ['trailing', 'keeperReserve'],
    title: 'Buy trailing stop', rows: { stop: kas('0.027'), trail: 'step 0.0005 KAS, gap 0.001 KAS, one update per 10 min at most' },
    state: { stopPrice: '2700000', trailStep: '50000', trailGap: '100000', trailWait: '6000', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  // ---- take-profit / OCO
  {
    name: 'take-profit sell', type: 'takeProfit', side: 'sell', kind: 'KobCondAsk', listType: 'take-profit', listLabel: 'Take-profit', ...SELL,
    fields: () => ({ amount: '2', price: '0.027' }),
    disc: { takeProfit: kas('0.027'), allInPrice: kas('0.027'), allInTotal: '0.054 KAS' }, notes: ['takeProfitLeg', 'carrierReturned'],
    title: 'Sell take-profit order', rows: { price: kas('0.027'), allIn: kas('0.027'), value: '2 KAS' },
    state: { tpPrice: '2700000', stopPrice: '0', slipBps: '0', amountLeft: '200000000', keeperTip: '0' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'take-profit buy', type: 'takeProfit', side: 'buy', kind: 'KobCondBid', listType: 'take-profit', listLabel: 'Take-profit', locked: '2.046 KAS', escrowed: null,
    carriers: { escrow: '0.046 KAS', deliveryCarrier: '2 KAS' },
    fields: () => ({ amount: '2', price: '0.023' }),
    disc: { takeProfit: kas('0.023'), allInTotal: '0.046 KAS' }, notes: ['takeProfitLeg'],
    title: 'Buy take-profit order', rows: { price: kas('0.023'), allIn: kas('0.023') },
    state: { tpPrice: '2300000', stopPrice: '0', amountLeft: '200000000', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'OCO sell: take-profit above, stop below', type: 'oco', side: 'sell', kind: 'KobCondAsk', listType: 'oco', listLabel: 'OCO', ...SELL,
    fields: () => ({ amount: '2', takeProfit: '0.027', stop: '0.023' }),
    disc: { takeProfit: kas('0.027'), stop: kas('0.023'), stopWorst: kas('0.02231') }, notes: ['oco', 'partialFillsKeepLegs', 'stopTrigger', 'keeperReserve'],
    title: 'Sell OCO order (take-profit and stop)', rows: { price: kas('0.027'), stop: kas('0.023'), band: '3% band, 30 s auction' },
    state: { tpPrice: '2700000', stopPrice: '2300000', slipBps: '300', armed: '0', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'OCO buy: take-profit below, stop above', type: 'oco', side: 'buy', kind: 'KobCondBid', listType: 'oco', listLabel: 'OCO', locked: '2.07662 KAS', escrowed: null,
    carriers: { escrow: '0.05562 KAS', deliveryCarrier: '2 KAS', keeperReserve: '0.021 KAS' },
    fields: () => ({ amount: '2', takeProfit: '0.023', stop: '0.027' }),
    disc: { takeProfit: kas('0.023'), stop: kas('0.027'), stopWorst: kas('0.02781') }, notes: ['oco', 'partialFillsKeepLegs'],
    title: 'Buy OCO order (take-profit and stop)', rows: { price: kas('0.023'), stop: kas('0.027') },
    state: { tpPrice: '2300000', stopPrice: '2700000', slipBps: '300', amountLeft: '200000000', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  // ---- IFD / IFO
  {
    name: 'IFD buy-first: buy 0.0245, then sell 0.026', type: 'ifd', side: 'buy', kind: 'KobIfdBid', listType: 'ifd', listLabel: 'IFD', locked: '16.049 KAS', escrowed: null,
    carriers: { escrow: '0.049 KAS', deliveryCarrier: '2 KAS x 4 = 8 KAS', exitCarrier: '2 KAS x 4 = 8 KAS' },
    fields: () => ({ amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' }),
    disc: { limit: kas('0.0245'), allInTotal: '0.049 KAS', minFill: '0.5 EXKCC', entryFills: 'at most 4', exitMinFill: '0.5 EXKCC', exitTakeProfit: kas('0.026') }, notes: ['ifd', 'position', 'buyFirst', 'minFill', 'exitGtc'],
    title: 'Buy IFD entry', rows: { amount: '2 EXKCC', price: kas('0.0245'), allIn: kas('0.0245'), minFill: '0.5 EXKCC', exitCarrier: '2 KAS', deliveryCarrier: '2 KAS', value: '16.049 KAS' },
    exit: { title: 'Exit created after each entry fill: Sell take-profit order', rows: { amount: '2 EXKCC', price: kas('0.026'), expiry: 'when the exit is created' }, state: { kind: 'KobCondAsk', fields: { tpPrice: '2600000', stopPrice: '0' } } },
    state: { price: '2450000', amountLeft: '200000000', minFill: '50000000', entryStop: '0', rptAmount: '0', exitCarrier: '200000000', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'IFD sell-first: sell 0.026, then buy back 0.0245', type: 'ifd', side: 'sell', kind: 'KobIfdAsk', listType: 'ifd', listLabel: 'IFD', locked: '12.00000003 KAS', escrowed: '2 EXKCC',
    carriers: { orderCarrier: '2 KAS', exitCarrier: '2 KAS x 4 = 8 KAS', tokenCarrier: '2 KAS' },
    fields: () => ({ amount: '2', price: '0.026', 'exit.takeProfit': '0.0245' }),
    disc: { limit: kas('0.026'), allInTotal: '0.052 KAS', minFill: '0.5 EXKCC', entryFills: 'at most 4', exitMinFill: '0.5 EXKCC', exitTakeProfit: kas('0.0245'), prefund: kas('0') }, notes: ['ifd', 'position', 'sellFirst', 'minFill', 'prefund'],
    title: 'Sell IFD entry', rows: { amount: '2 EXKCC', price: kas('0.026'), minFill: '0.5 EXKCC', exitCarrier: '2 KAS', value: '10.00000003 KAS' },
    exit: { title: 'Exit created after each entry fill: Buy take-profit order', rows: { amount: '2 EXKCC', price: kas('0.0245') }, state: { kind: 'KobCondBid', fields: { tpPrice: '2450000', stopPrice: '0' } } },
    state: { price: '2600000', amountLeft: '200000000', minFill: '50000000', prefund: '0', exitCarrier: '200000000', rptAmount: '0' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'IFD buy-first with a stop exit', type: 'ifd', side: 'buy', kind: 'KobIfdBid', listType: 'ifd', listLabel: 'IFD', locked: '16.049 KAS', escrowed: null,
    carriers: { escrow: '0.049 KAS', deliveryCarrier: '2 KAS x 4 = 8 KAS', exitCarrier: '2 KAS x 4 = 8 KAS' },
    fields: () => ({ amount: '2', price: '0.0245', 'exit.kind': 'stop', 'exit.stop': '0.023' }),
    disc: { exitStop: kas('0.023'), exitStopWorst: kas('0.02231') }, notes: ['ifd', 'exitStop', 'triggerExposure'],
    title: 'Buy IFD entry', rows: { price: kas('0.0245') },
    exit: { title: 'Exit created after each entry fill: Sell stop order', rows: { stop: kas('0.023'), band: '3% band, 30 s auction' }, state: { kind: 'KobCondAsk', fields: { stopPrice: '2300000', tpPrice: '0', slipBps: '300' } } },
    state: { price: '2450000', amountLeft: '200000000', minFill: '50000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'IFO buy-first (bracket): entry, then take-profit 0.026 or stop 0.023', type: 'ifo', side: 'buy', kind: 'KobIfdBid', listType: 'ifo', listLabel: 'IFO', locked: '16.049 KAS', escrowed: null,
    carriers: { escrow: '0.049 KAS', deliveryCarrier: '2 KAS x 4 = 8 KAS', exitCarrier: '2 KAS x 4 = 8 KAS' },
    fields: () => ({ amount: '2', price: '0.0245', 'exit.takeProfit': '0.026', 'exit.stop': '0.023' }),
    disc: { exitTakeProfit: kas('0.026'), exitStop: kas('0.023'), exitStopWorst: kas('0.02231') }, notes: ['ifo', 'position', 'buyFirst', 'exitStop'],
    title: 'Buy IFO entry', rows: { price: kas('0.0245'), minFill: '0.5 EXKCC' },
    exit: { title: 'Exit created after each entry fill: Sell OCO order (take-profit and stop)', rows: { price: kas('0.026'), stop: kas('0.023') }, state: { kind: 'KobCondAsk', fields: { tpPrice: '2600000', stopPrice: '2300000', slipBps: '300' } } },
    state: { price: '2450000', amountLeft: '200000000', minFill: '50000000', rptAmount: '0' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'IFO sell-first (bracket): the prefund covers a stop buy-back above the entry', type: 'ifo', side: 'sell', kind: 'KobIfdAsk', listType: 'ifo', listLabel: 'IFO', locked: '12.00362003 KAS', escrowed: '2 EXKCC',
    carriers: { orderCarrier: '2 KAS', prefund: '0.00362003 KAS', exitCarrier: '2 KAS x 4 = 8 KAS', tokenCarrier: '2 KAS' },
    fields: () => ({ amount: '2', price: '0.026', 'exit.takeProfit': '0.0245', 'exit.stop': '0.027' }),
    disc: { exitTakeProfit: kas('0.0245'), exitStop: kas('0.027'), exitStopWorst: kas('0.02781'), prefund: kas('0.00181') }, notes: ['ifo', 'sellFirst', 'prefund'],
    title: 'Sell IFO entry', rows: { price: kas('0.026'), prefund: kas('0.00181') },
    exit: { title: 'Exit created after each entry fill: Buy OCO order (take-profit and stop)', rows: { price: kas('0.0245'), stop: kas('0.027') }, state: { kind: 'KobCondBid', fields: { tpPrice: '2450000', stopPrice: '2700000', slipBps: '300' } } },
    state: { price: '2600000', amountLeft: '200000000', prefund: '181000', rptAmount: '0' },
    timing: { kind: 'gtc' },
  },
  // ---- IFD with a stop entry
  {
    name: 'IFD with a stop entry, buy-first', type: 'ifd', side: 'buy', kind: 'KobIfdBid', listType: 'ifd', listLabel: 'IFD', locked: '16.076 KAS', escrowed: null,
    carriers: { escrow: '0.055 KAS', deliveryCarrier: '2 KAS x 4 = 8 KAS', exitCarrier: '2 KAS x 4 = 8 KAS', keeperReserve: '0.021 KAS' },
    fields: () => ({ amount: '2', price: '0.0275', 'entry.stop': '0.027', 'exit.takeProfit': '0.029' }),
    disc: { entryStop: kas('0.027'), exitTakeProfit: kas('0.029'), expected: kas('0.027'), worst: kas('0.0275') }, notes: ['ifd', 'stopEntry', 'stopEntryAuction'],
    title: 'Buy IFD stop entry', rows: { price: kas('0.0275'), entryStop: `${kas('0.027')} | then a 30 s auction to the limit` },
    exit: { title: 'Exit created after each entry fill: Sell take-profit order', rows: { price: kas('0.029') }, state: { kind: 'KobCondAsk', fields: { tpPrice: '2900000' } } },
    state: { price: '2750000', entryStop: '2700000', bandDaa: '300', keeperTip: '2100000', armed: '0', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'IFD with a stop entry, sell-first', type: 'ifd', side: 'sell', kind: 'KobIfdAsk', listType: 'ifd', listLabel: 'IFD', locked: '12.00000003 KAS', escrowed: '2 EXKCC',
    carriers: { orderCarrier: '2 KAS', exitCarrier: '2 KAS x 4 = 8 KAS', tokenCarrier: '2 KAS' },
    fields: () => ({ amount: '2', price: '0.0225', 'entry.stop': '0.023', 'exit.takeProfit': '0.021' }),
    disc: { entryStop: kas('0.023'), exitTakeProfit: kas('0.021'), expected: kas('0.023'), worst: kas('0.0225') }, notes: ['ifd', 'stopEntry', 'sellFirst'],
    title: 'Sell IFD stop entry', rows: { price: kas('0.0225'), entryStop: `${kas('0.023')} | then a 30 s auction to the limit` },
    exit: { title: 'Exit created after each entry fill: Buy take-profit order', rows: { price: kas('0.021') }, state: { kind: 'KobCondBid', fields: { tpPrice: '2100000' } } },
    state: { price: '2250000', entryStop: '2300000', bandDaa: '300', keeperTip: '2100000', armed: '0', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  // ---- repeat IFD / IFO (default unlimited within 90 days; explicit count). The merge tip is charged per exit minimum fill (cond-common.ts
  // mergeTipRate): with the IFD default minimum fill (a quarter of the amount, 0.5 EXKCC) it would double per token and turn these spreads into a
  // loss, so the cases set a 1 EXKCC minimum fill (merge tip 0.01 KAS per token buy-first, 0.015 sell-first; a 0.0155 KAS spread is the smallest
  // profitable one here).
  {
    name: 'repeat IFD buy-first, default unlimited (within 90 days)', type: 'repeatIfd', side: 'buy', kind: 'KobIfdBid', listType: 'repeat', listLabel: 'Repeat IFD', locked: '10.049 KAS', escrowed: null,
    carriers: { escrow: '0.049 KAS', deliveryCarrier: '2 KAS x 2 = 4 KAS', exitCarrier: '2 KAS x 3 = 6 KAS' },
    fields: () => ({ amount: '2', minFill: '1', price: '0.0245', 'exit.takeProfit': '0.04' }),
    disc: { repeat: 'unlimited (within 90 days)', profitPerToken: kas('0.0055'), exitTakeProfit: kas('0.04'), exitTakeProfitAllIn: kas('0.03') }, notes: ['repeat', 'repeatUnlimited', 'repeatReBuys', 'mergeTip', 'cancelPosition'],
    title: 'Buy repeat IFD entry', rows: { repeat: 'unlimited (within 90 days)', price: kas('0.0245') },
    exit: { title: 'Exit created after each entry fill: Sell take-profit order', rows: { price: kas('0.04'), allIn: kas('0.03'), tip: kas('0.01') }, state: { kind: 'KobCondAsk', fields: { tpPrice: '4000000', tip: '1000000' } } },
    state: { price: '2450000', amountLeft: '200000000', rptAmount: '2000000000000001', exitCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'repeat IFD buy-first with an explicit count of 3', type: 'repeatIfd', side: 'buy', kind: 'KobIfdBid', listType: 'repeat', listLabel: 'Repeat IFD', locked: '10.049 KAS', escrowed: null,
    carriers: { escrow: '0.049 KAS', deliveryCarrier: '2 KAS x 2 = 4 KAS', exitCarrier: '2 KAS x 3 = 6 KAS' },
    fields: () => ({ amount: '2', minFill: '1', price: '0.0245', 'exit.takeProfit': '0.04', 'repeat.count': '3' }),
    disc: { repeat: '3 times | 2 EXKCC per cycle', profitPerToken: kas('0.0055') }, notes: ['repeat', 'repeatCounted', 'repeatReBuys'],
    title: 'Buy repeat IFD entry', rows: { repeat: 're-arms up to 3 times (2 EXKCC per cycle)' },
    exit: { title: 'Exit created after each entry fill: Sell take-profit order', rows: { price: kas('0.04') }, state: { kind: 'KobCondAsk', fields: { tpPrice: '4000000' } } },
    state: { price: '2450000', amountLeft: '200000000', rptAmount: '600000001' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'repeat IFD sell-first, default unlimited', type: 'repeatIfd', side: 'sell', kind: 'KobIfdAsk', listType: 'repeat', listLabel: 'Repeat IFD', locked: '8.00000001 KAS', escrowed: '2 EXKCC',
    carriers: { orderCarrier: '2 KAS', exitCarrier: '2 KAS x 2 = 4 KAS', tokenCarrier: '2 KAS' },
    fields: () => ({ amount: '2', minFill: '1', price: '0.04', 'exit.takeProfit': '0.0245' }),
    disc: { repeat: 'unlimited (within 90 days)', profitPerToken: kas('0.0005'), exitTakeProfitAllIn: kas('0.0395') }, notes: ['repeat', 'repeatUnlimited', 'repeatReSells', 'prefund'],
    title: 'Sell repeat IFD entry', rows: { repeat: 'unlimited (within 90 days)', price: kas('0.04') },
    exit: { title: 'Exit created after each entry fill: Buy take-profit order', rows: { price: kas('0.0245'), allIn: kas('0.0395'), tip: kas('0.015') }, state: { kind: 'KobCondBid', fields: { tpPrice: '2450000', tip: '1500000' } } },
    state: { price: '4000000', amountLeft: '200000000', rptAmount: '2000000000000001', prefund: '0' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'repeat IFO buy-first, default unlimited', type: 'repeatIfo', side: 'buy', kind: 'KobIfdBid', listType: 'repeat', listLabel: 'Repeat IFD', locked: '10.049 KAS', escrowed: null,
    carriers: { escrow: '0.049 KAS', deliveryCarrier: '2 KAS x 2 = 4 KAS', exitCarrier: '2 KAS x 3 = 6 KAS' },
    fields: () => ({ amount: '2', minFill: '1', price: '0.0245', 'exit.takeProfit': '0.04', 'exit.stop': '0.023' }),
    disc: { repeat: 'unlimited (within 90 days)', exitStop: kas('0.023'), exitStopWorst: kas('0.02231') }, notes: ['ifo', 'repeat', 'repeatUnlimited', 'repeatStopLossEnds'],
    title: 'Buy repeat IFO entry', rows: { repeat: 'unlimited (within 90 days)' },
    exit: { title: 'Exit created after each entry fill: Sell OCO order (take-profit and stop)', rows: { price: kas('0.04'), stop: kas('0.023') }, state: { kind: 'KobCondAsk', fields: { tpPrice: '4000000', stopPrice: '2300000' } } },
    state: { price: '2450000', amountLeft: '200000000', rptAmount: '2000000000000001' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'repeat IFO sell-first with an explicit count of 3', type: 'repeatIfo', side: 'sell', kind: 'KobIfdAsk', listType: 'repeat', listLabel: 'Repeat IFD', locked: '8.05300001 KAS', escrowed: '2 EXKCC',
    carriers: { orderCarrier: '2 KAS', prefund: '0.05300001 KAS', exitCarrier: '2 KAS x 2 = 4 KAS', tokenCarrier: '2 KAS' },
    fields: () => ({ amount: '2', minFill: '1', price: '0.04', 'exit.takeProfit': '0.0245', 'exit.stop': '0.05', 'repeat.count': '3' }),
    disc: { repeat: '3 times | 2 EXKCC per cycle', exitStop: kas('0.05'), exitStopWorst: kas('0.0515'), prefund: kas('0.0265') }, notes: ['ifo', 'repeat', 'repeatCounted', 'repeatReSells'],
    title: 'Sell repeat IFO entry', rows: { repeat: 're-arms up to 3 times (2 EXKCC per cycle)', prefund: kas('0.0265') },
    exit: { title: 'Exit created after each entry fill: Buy OCO order (take-profit and stop)', rows: { price: kas('0.0245'), stop: kas('0.05') }, state: { kind: 'KobCondBid', fields: { tpPrice: '2450000', stopPrice: '5000000' } } },
    state: { price: '4000000', amountLeft: '200000000', rptAmount: '600000001', prefund: '2650000' },
    timing: { kind: 'gtc' },
  },
];

/** `datetime-local` text of `c.unix + seconds` in the browser's wall clock (truncated to the minute, like the input itself). */
function localAfter(c: Ctx, seconds: number): string {
  const d = new Date((c.unix + seconds - c.tz * 60) * 1000);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())}T${p(d.getUTCHours())}:${p(d.getUTCMinutes())}`;
}
