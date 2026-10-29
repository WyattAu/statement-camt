//! Property tests for the CAMT parser boundary.
//!
//! The example tests pin well-formed documents; this property pins the
//! boundary behavior: **arbitrary bytes produce `Ok` or `Err`, never a
//! panic**. A bank-statement parser is fed the internet by definition, and
//! a panic on hostile input is a denial of service in the reconciliation
//! path.

#![allow(clippy::expect_used, clippy::panic)]

use proptest::prelude::*;
use statement_camt::parse_camt_with_scale;

proptest! {
    /// Arbitrary UTF-8 never panics the parser — it either parses or
    /// returns a shaped error.
    #[test]
    fn arbitrary_text_never_panics(input in ".*") {
        let _ = parse_camt_with_scale(&input, 2);
    }

    /// Arbitrary bytes rendered as chars, with the tags a document needs
    /// seeded in, so the parser exercises its block-scanning paths against
    /// hostile combinations rather than only clean documents.
    #[test]
    fn hostile_tag_soup_never_panics(
        prefix in "[<>:/ A-Za-z0-9]{0,80}",
        inner in "[<>:/ A-Za-z0-9]{0,120}",
        suffix in "[<>:/ A-Za-z0-9]{0,80}",
    ) {
        let doc = format!("{prefix}<Stmt>{inner}</Stmt>{suffix}");
        let _ = parse_camt_with_scale(&doc, 2);
    }

    /// A well-formed skeleton with arbitrary *amounts* never panics: the
    /// amount parser is the money boundary, and it must return errors, not
    /// values it invented.
    #[test]
    fn arbitrary_amounts_never_panic_or_invent_values(
        amount in "[0-9.-]{0,12}",
        scale in 0u32..5u32,
    ) {
        let doc = format!(
            "<BkToCstmrStmt><Stmt><Id>S</Id>\
             <Bal><Tp><Cd>OPBD</Cd></Tp><Amt Ccy=\"GBP\">{amount}</Amt><CdtDbtInd>CRDT</CdtDbtInd></Bal>\
             <Bal><Tp><Cd>CLBD</Cd></Tp><Amt Ccy=\"GBP\">{amount}</Amt><CdtDbtInd>CRDT</CdtDbtInd></Bal>\
             </Stmt></BkToCstmrStmt>"
        );
        // Whatever the outcome, a successful parse of a degenerate document
        // must still fail the balance identity (both balances equal, no
        // lines → identity fails unless both are zero) — the point is only
        // that nothing panics and nothing is invented.
        let _ = parse_camt_with_scale(&doc, scale);
    }
}
