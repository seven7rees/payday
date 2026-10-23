# Payday: a Bordrless Workshop pair

Hold the coin, get paid in SOL every week. Half of every creator fee fills a pot; each week the
pot pays holders of 1,000+ coins by amount and hold time. No long sell tax: 1% of a sell only in
its first 10 minutes of holding.

Status 2026-10-22: hook and script IMPLEMENTED and COMPILED for Solana (cargo build-sbf); host
tests and the full LiteSVM story TEST-EXECUTED against the programs built from the pinned source
and against the live mainnet program bytes. NOT submitted, NOT deployed, not audited.

## What is here

- `submission/`: the exact files for the Workshop submit page, with a README for Bordrless.
- `hook_variants/payday/`: the hook (hold clock, 1% brake for the first 10 minutes of a hold,
  the sell relay, record_payout). `hook.json` beside it.
- `script_variants/payday/`: the companion script (builder's quarter, weekly weighted holder
  payouts). `keeper.json` beside it.
- `tests/`: the host tests (t23 to t25) and the LiteSVM story (t11). They run from the build
  checkout, which needs `ref/` (`tools/fetch_refs.sh` recreates it).
- `art/`: logo, graphic and video for the listing.
- `WALLET.txt`: the builder's PUBLIC payout address only.

## Hygiene

No private keys or seed material. `declare_id!` holds local build addresses (public); Bordrless
writes the real ids at submission. `ref/`, `target*/` and keypair files are ignored.
