# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [Unreleased]

## [0.1.0] - 2026-10-07

Initial release.

### Added

- `parse_camt` / `parse_camt_with_scale` — CAMT.053 documents to
  `recon_kit::Statement`s, one per `<Stmt>` block.
- The balance identity with teeth: `OPBD` and `CLBD` blocks are read from the
  document, so a lost entry fails with `BalanceIdentityFailed` naming opening,
  computed and declared closing — the check OFX cannot offer.
- Booked-entries-only filtering (`BOOK`); pending and memo entries are
  excluded, and a missing status block fails closed.
- `CdtDbtInd` → `Direction`; `AcctSvcrRef` → line identity; float-free
  decimal parsing with scale bounds.
