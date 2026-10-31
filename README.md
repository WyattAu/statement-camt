# statement-camt

ISO 20022 CAMT.053 (`bank-to-customer statement`) adapter producing
[`recon-kit`](https://crates.io/crates/recon-kit) statements.

## Why CAMT is the format the model was waiting for

OFX publishes the closing balance and leaves the opening one to be derived,
which makes the balance identity trivially true and the truncated-import check
inert. **CAMT.053 carries both**: an `OPBD` (opening-balance-booked) `Bal`
block and a `CLBD` (closing-balance-booked) one. The adapter takes both at
face value, so a statement whose import lost an entry fails on arrival with a
`BalanceIdentityFailed` error naming all three numbers; opening, computed,
declared closing.

## What is filtered, and why

**Booked entries only.** A `<Ntry>` with status `PDNG` is money the bank has
*seen* but not *booked*; it may still be reversed, and it has no booking date
in the model's sense. `INFO` (memo-only) is excluded by the same rule. An
entry with no status block fails closed: skipped, not reconciled.

**Signs become direction.** CAMT does not sign amounts; it carries an explicit
`<CdtDbtInd>`. The mapping is the identity the format already provides.

## Identity and dates

Entry identity prefers `<AcctSvcrRef>` (the FITID analogue), then `<NtryRef>`,
then `<AddtlNtryInf>`; idempotent re-import keys off this, so a fabricated id
is worse than a refusal. Booking date is `<BookgDt><Dt>` (or `DtTm`, truncated
to its date).

## Scope

Namespace-agnostic (local-name matching): `camt.053.001.02` through `.08` and
pre-ISO Swift variants all parse. Multi-statement documents return one
`Statement` per `<Stmt>` block. Structural XML validation is deliberately not
attempted; the balance identity is the real well-formedness test.

## Licence

MIT OR Apache-2.0.
