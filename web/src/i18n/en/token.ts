// How a token the registry does not list is named and its amounts are said (app/token-display.ts): never "base units".
export default {
  'token.note.declared': 'declared, bound by ancestry',
  'token.note.declaredTitle': "Not in the token registry: its name, ticker and decimals ({decimals}) are a declaration recorded on chain (KCC-23) and bound to the token by ancestry: it sits in a transaction whose output the token's creation spent, so whoever funded that coin chose it, which is not always the creator. Nobody has checked it, and no list has reviewed this token.",
  'token.note.conflict': 'facts disagree',
  'token.note.conflictTitle': 'The token registry (or the price scale of its market) and what the token recorded on chain at its creation disagree on: {fields}. The on-chain values are shown; trading waits until they agree when the decimals differ.',
  'token.note.field.decimals': 'decimals',
  'token.note.field.symbol': 'ticker',
  'token.note.field.name': 'name',
  'token.note.programTitle': 'Not in the token registry and it declares no decimals on chain: its amounts use {decimals} decimals, the {source} for the tokens of its program.',
  'token.note.issued': 'unverified',
  'token.note.issuedTitle': "Not in the token registry: its name and decimals ({decimals}) come from this browser's creation of it.",
  'token.note.assumed': 'raw amount',
  'token.note.scale': '{decimals} decimals, from its market',
  'token.note.scaleTitle': 'It declares no decimals on chain: its amounts use {decimals} decimals, the price scale of its market, as the order ticket does.',
  'token.amount.assumed': '{amount} {token} (raw amount)',
  'token.amount.plain': '{amount} (raw amount)',
  'token.note.assumedTitle': "Nothing states its decimals (no declaration, no program default, no order): the chain's integer amount is shown, with its covenant id.",
};
