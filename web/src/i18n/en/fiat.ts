export default {
  'settings.currency.title': 'Display currency',
  'settings.currency.explain':
    'Show the values you read (the market list\'s price, volume, market cap and depth, a market\'s last price and 24 h statistics, the Portfolio\'s summary and balances) in KAS or as approximate US dollars. The order book, the order ticket, the order and fill tables and the Activity feed always stay in KAS, and every price you type or sign is in KAS and is never converted.',
  'settings.currency.label': 'Show values in',
  'settings.currency.kas': 'KAS',
  'settings.currency.usd': 'USD',
  'settings.currency.hint':
    'USD uses the KAS price your indexer fetches (CoinGecko, or a volume-weighted average of several exchanges when CoinGecko is out of reach): it is an estimate, not a quote. Your browser asks only your indexer, never a price site. Applies at once and is kept in this browser. If the price is missing or older than 10 minutes, KAS is shown.',
  'fiat.age.under': 'under a minute',
  'fiat.age.min': '{n} min',
  'fiat.age.hours': '{h} h {m} min',
  'fiat.note': 'USD at the KAS price from {source}, {age} ago. An estimate, not a quote.',
  'fiat.note.usdt': 'USD (USDT) at the KAS price from {source}, {age} ago. An estimate, not a quote.',
  'fiat.note.testnet': 'Testnet KAS has no value: priced as mainnet KAS, for display only.',
  'fiat.fallback.loading': 'Fetching the KAS price for the USD display; showing KAS meanwhile.',
  'fiat.fallback.unavailable': 'This indexer serves no KAS price, so USD is not available: showing KAS.',
  'fiat.fallback.missing': 'The KAS price could not be read, so USD is not available: showing KAS.',
  'fiat.fallback.stale': 'The KAS price is {age} old, too old to show: showing KAS.',
  'fiat.cell.rate': 'USD estimate at {price} per KAS',
};
