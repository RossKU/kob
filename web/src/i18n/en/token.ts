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
  'token.note.assumed': 'decimals assumed {decimals}',
  'token.amount.assumed': '{amount} {token} (decimals assumed {decimals})',
  'token.amount.plain': '{amount} tokens (decimals assumed {decimals})',
  'token.note.assumedTitle': 'Not in the token registry and nothing states its decimals: amounts are shown with {decimals} decimals (the Kaspa convention), named by its covenant id.',
};
