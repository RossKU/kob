// How a token the registry does not list is named and its amounts are said (app/token-display.ts): never "base units".
export default {
  'token.note.issued': 'unverified',
  'token.note.issuedTitle': "Not in the token registry: its name and decimals ({decimals}) come from this browser's issuance of it.",
  'token.note.assumed': 'decimals assumed {decimals}',
  'token.amount.assumed': '{amount} {token} (decimals assumed {decimals})',
  'token.amount.plain': '{amount} tokens (decimals assumed {decimals})',
  'token.note.assumedTitle': 'Not in the token registry and nothing states its decimals: amounts are shown with {decimals} decimals (the Kaspa convention), named by its covenant id.',
};
