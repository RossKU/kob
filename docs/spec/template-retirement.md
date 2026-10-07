# Templates this build does not pin

Every byte of an order covenant is part of its template hash, so a contract change gives the orders placed afterwards a
new script while every order placed before keeps the old one until it is spent. A build supports only the templates it
pins: an order under any other template (an older one included) is unknown to it, like any other unknown template. The
builders, the CLI and the web wallet build nothing for it, the indexer never lists it nor offers it to a matcher or a
keeper, and a placement record of it is rejected. On chain nothing changes for such an order: its maker ends it with a
raw transaction that spends the order's own `cancel(sig)` entry (custody, strays and the carrier back to the maker), for
example built with the KOB release it was placed with, and its permissionless refund still validates after its expiry.
What this build therefore no longer does for such an order: no executor of this build fills it, arms or trails its stop,
runs its stop-loss exit (an if-done exit placed under an older template is such an order), merges its repeat, or refunds
it as a keeper (its keeper refund and IOC kill wait for someone who builds them with the older release). A maker with a
live stop, exit or resting order under an older template ends it by its cancel and places it again under the current
templates; the web wallet marks such orders `old-template`.
