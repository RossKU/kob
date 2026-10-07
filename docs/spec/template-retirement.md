# Templates this build does not pin

Every byte of an order covenant is part of its template hash, so a contract change gives the orders placed afterwards a
new script while every order placed before keeps the old one until it is spent. A build supports only the templates it
pins: an order under any other template (an older one included) is unknown to it, like any other unknown template. The
builders, the CLI and the web wallet build nothing for it, the indexer never lists it nor offers it to a matcher or a
keeper, and a placement record of it is rejected. On chain nothing changes for such an order: its maker ends it with a
raw transaction that spends the order's own `cancel(sig)` entry (custody, strays and the carrier back to the maker), for
example built with the KOB release it was placed with, and its permissionless refund still validates after its expiry.
