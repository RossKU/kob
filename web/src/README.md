# web/src module map

`kob/` is protocol and transaction logic with no DOM; `ui/` is the Preact components. The protocol itself is in Rust
(`crates/kob-wasm`); `kob/` plans and checks over it.

| Folder | Role |
|---|---|
| `main.tsx`, `config.ts`, `styles.css` | Entry point, the layered app configuration, the stylesheet |
| `app/` | Composition root (`services.ts`, `context.tsx`), `App.tsx`, hash router, app-wide helpers |
| `kob/` | Protocol and transaction logic over kob-wasm: order planners, pre-sign decoding, registry, balances, issuance |
| `ui/` | Preact components by screen; `kit/` holds the shared widgets |
| `data/` | Clients: node RPC (official SDK), indexer REST and WebSocket, UTXOs, token tracker, genesis check |
| `wallet/` | Browser-wallet adapters and the signing path |
| `signer/` | The sandboxed private-key signer page (`signer.html`): its own decoding, approval and signing |
| `i18n/` | English texts by UI area and the typed lookup |
| `testing/` | Test-only fixtures, mock node and mock wallets |
| `third-party/` | Hallmarks v1.0 (identicon of a covenant id): vendored wordlist and vectors, `UPSTREAM.md` |
