//! ISO 20022 CAMT.053 (`bank-to-customer statement`) adapter producing
//! [`recon_kit::Statement`]s.
//!
//! # Why CAMT is the format the model was waiting for
//!
//! OFX publishes the closing balance and leaves the opening one to be derived,
//! which makes the model's balance identity trivially true and its
//! truncated-import check inert. **CAMT.053 carries both**: an `OPBD`
//! (opening-balance-booked) `Bal` block and a `CLBD` (closing-balance-booked)
//! one. The adapter takes both at face value, so a statement whose import lost
//! an entry fails [`Statement::validate`] on arrival instead of reconciling
//! cleanly against incomplete data. That check is the single most valuable
//! property of the format, and it is the reason this adapter exists alongside
//! the OFX one.
//!
//! # What is filtered, and why
//!
//! **Booked entries only.** A `<Ntry>` with `<Sts><Cd>PDNG</Cd>` (pending) is
//! money the bank has *seen* but not *booked*; it has no booking date in the
//! model's sense and including it would reconcile against transactions that
//! may still be reversed. The `<Sts>` block is the filter; `INFO`-status
//! entries (memo-only, like a soft hold) are excluded by the same rule.
//!
//! **Signs become direction.** CAMT does not sign amounts — it carries an
//! explicit `<CdtDbtInd>` of `CRDT`/`DBIT`. The model wants exactly that, so
//! the mapping is the identity the format already provides.
//!
//! # Dates and identity
//!
//! Booking date is `<BookgDt><Dt>` (or `DtTm`, truncated to its date) — the
//! date the bank treats as effective, which is what the matching engine
//! compares. Entry identity prefers `<AcctSvcrRef>` (the account servicer's
//! reference, the FITID analogue), then `<NtryRef>`, then `<AddtlNtryInf>` as
//! a last resort with the reason recorded in the error if all are absent —
//! idempotent re-import keys off this id, so a fabricated one is worse than a
//! refusal.
//!
//! # Scope
//!
//! Namespace-agnostic (local-name matching), so `camt.053.001.02` through
//! `.08` and the pre-ISO Swift variants all parse; multi-statement documents
//! return one [`Statement`] per `<Stmt>` block. Structural XML validation is
//! deliberately not attempted — a bank statement is scanned, not web-scraped,
//! and the balance identity is the real well-formedness test.

use recon_kit::{Direction, Statement, StatementLine};

/// Why a CAMT document cannot become a trustworthy statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CamtError {
    /// No `<BkToCstmrStmt>` in the document — this is not a camt.053.
    NotAStatement,
    /// A `<Stmt>` block carries no `<Id>`.
    MissingStatementId,
    /// A `<Stmt>` has no `OPBD` (opening balance) block.
    MissingOpeningBalance,
    /// A `<Stmt>` has no `CLBD` (closing balance) block.
    MissingClosingBalance,
    /// Opening balance plus the booked entries does not reach the closing
    /// balance the bank declared. This is the truncated-import check, and the
    /// reason CAMT.053 is worth an adapter: both balances are on the document,
    /// so a lost entry fails on arrival.
    BalanceIdentityFailed {
        /// The declared opening balance.
        opening: i64,
        /// What the entries produce.
        computed: i64,
        /// The declared closing balance.
        closing: i64,
    },
    /// An `<Amt>` was not a decimal string this adapter accepts.
    MalformedAmount {
        /// The raw text.
        raw: String,
    },
    /// An amount carried more fraction digits than the declared scale.
    TooManyFractionDigits {
        /// The raw text.
        raw: String,
        /// The scale in force.
        scale: u32,
    },
    /// A date was not `YYYY-MM-DD`.
    MalformedDate {
        /// The raw text.
        raw: String,
    },
    /// A booked entry carries no identifier (AcctSvcrRef, NtryRef, and
    /// AddtlNtryInf all absent).
    UnidentifiableEntry,
    /// A `<CdtDbtInd>` was neither `CRDT` nor `DBIT`.
    UnknownIndicator {
        /// The raw text.
        raw: String,
    },
    /// The `<Amt Ccy=…>` attribute disagrees between entries in one statement,
    /// or with the balance blocks.
    CurrencyMismatch {
        /// The statement's currency.
        expected: String,
        /// The offending amount's currency.
        found: String,
    },
}

/// Parse a CAMT.053 document at the common two-decimal scale.
pub fn parse_camt(input: &str) -> Result<Vec<Statement>, CamtError> {
    parse_camt_with_scale(input, 2)
}

/// Parse a CAMT.053 document with an explicit minor-unit scale
/// (JPY accounts use `0`; the common case is `2`).
pub fn parse_camt_with_scale(input: &str, scale: u32) -> Result<Vec<Statement>, CamtError> {
    let body = tag_block(input, "BkToCstmrStmt").ok_or(CamtError::NotAStatement)?;
    let mut statements = Vec::new();
    for stmt_block in split_blocks(&body, "Stmt") {
        statements.push(parse_stmt(&stmt_block, scale)?);
    }
    if statements.is_empty() {
        return Err(CamtError::NotAStatement);
    }
    Ok(statements)
}

fn parse_stmt(stmt: &str, scale: u32) -> Result<Statement, CamtError> {
    // The statement Id is a well-formedness gate, not part of the model:
    // sequence tracking across statements (ElctrncSeqNb) is the importer's
    // state, and a document that cannot even name its statement is not one to
    // reconcile against.
    if tag_block(stmt, "Id")
        .and_then(|i| i.lines().next().map(|line| line.trim().to_string()))
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Err(CamtError::MissingStatementId);
    }

    // The balance blocks: OPBD is the opening booked balance, CLBD the
    // closing. Their dates are the statement's bounds; their amounts are the
    // identity the import is checked against.
    let mut opening = None;
    let mut closing = None;
    for bal in split_blocks(stmt, "Bal") {
        let code = tag_block(&bal, "Tp")
            .and_then(|tp| first_tag_value(&tp, "Cd").or_else(|| first_tag_value(&tp, "Prtry")))
            .unwrap_or_default();
        let amount = tag_block(&bal, "Amt");
        let Some(amount_text) = amount.as_ref().and_then(|a| a.lines().next()) else {
            continue;
        };
        let amount_text = amount_text.trim();
        let signed = parse_amount(amount_text, scale)?;
        let signed = match first_tag_value(&bal, "CdtDbtInd").as_deref() {
            Some("DBIT") => -signed,
            _ => signed,
        };
        match code.as_str() {
            "OPBD" => opening = Some(signed),
            "CLBD" => closing = Some(signed),
            _ => {}
        }
    }
    let opening_balance = opening.ok_or(CamtError::MissingOpeningBalance)?;
    let closing_balance = closing.ok_or(CamtError::MissingClosingBalance)?;

    let currency = first_attr_value(stmt, "Amt", "Ccy").unwrap_or_else(|| "UNKNOWN".to_string());

    let mut lines = Vec::new();
    for ntry in split_blocks(stmt, "Ntry") {
        // Booked entries only: PDNG (pending) and INFO are not bookings. An
        // entry with no status block is malformed; failing closed skips it
        // rather than reconciling against something the bank may reverse.
        let status = tag_block(&ntry, "Sts").map(|block| {
            let trimmed = block.trim().to_string();
            if trimmed.starts_with('<') {
                // `<Sts><Cd>BOOK</Cd></Sts>` — the normal form.
                first_tag_value(&block, "Cd").unwrap_or_default()
            } else {
                // `<Sts>BOOK</Sts>` — the shorthand some renderers use.
                trimmed
            }
        });
        match status.as_deref() {
            Some("BOOK") | Some("BOOKED") => {}
            _ => continue,
        }
        let amount_text = first_tag_value(&ntry, "Amt").ok_or(CamtError::MalformedAmount {
            raw: "<Amt> absent".into(),
        })?;
        let magnitude = parse_amount(&amount_text, scale)?;
        let direction = match first_tag_value(&ntry, "CdtDbtInd").as_deref() {
            Some("DBIT") => Direction::Debit,
            Some("CRDT") => Direction::Credit,
            other => {
                return Err(CamtError::UnknownIndicator {
                    raw: other.unwrap_or("absent").to_string(),
                });
            }
        };
        // Entry identity, in order of authority.
        let entry_id = first_tag_value(&ntry, "AcctSvcrRef")
            .or_else(|| first_tag_value(&ntry, "NtryRef"))
            .or_else(|| first_tag_value(&ntry, "AddtlNtryInf"))
            .ok_or(CamtError::UnidentifiableEntry)?;
        let booked_on = booking_date(&ntry)?;
        let description = first_tag_value(&ntry, "AddtlNtryInf")
            .or_else(|| first_tag_value(&ntry, "NtryRef"))
            .unwrap_or_default();
        lines.push(StatementLine {
            id: entry_id,
            booked_on,
            amount: magnitude as u64,
            direction,
            description,
        });
    }
    lines.sort_by(|a, b| a.booked_on.cmp(&b.booked_on).then(a.id.cmp(&b.id)));

    let statement = Statement {
        currency,
        opening_balance,
        closing_balance,
        lines,
    };
    if statement.computed_closing() != statement.closing_balance {
        return Err(CamtError::BalanceIdentityFailed {
            opening: statement.opening_balance,
            computed: statement.computed_closing(),
            closing: statement.closing_balance,
        });
    }
    Ok(statement)
}

/// `<BookgDt><Dt>` or `<BookgDt><DtTm>` — the booking date, `YYYY-MM-DD`.
fn booking_date(ntry: &str) -> Result<String, CamtError> {
    let block = tag_block(ntry, "BookgDt");
    let raw = block
        .as_deref()
        .and_then(|b| first_tag_value(b, "Dt").or_else(|| first_tag_value(b, "DtTm")))
        .or_else(|| first_tag_value(ntry, "ValDt"))
        .ok_or_else(|| CamtError::MalformedDate {
            raw: "absent".into(),
        })?;
    let date_part = raw.trim().chars().take(10).collect::<String>();
    let mut chars = date_part.chars();
    let well_formed = date_part.len() == 10
        && chars.by_ref().enumerate().all(|(i, c)| match i {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        });
    if well_formed {
        Ok(date_part)
    } else {
        Err(CamtError::MalformedDate { raw })
    }
}

/// Parse an unsigned-or-signed decimal string into minor units, float-free.
fn parse_amount(raw: &str, scale: u32) -> Result<i64, CamtError> {
    let trimmed = raw.trim();
    let (sign, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let (integer_part, fraction_part) = match digits.split_once('.') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    if integer_part.is_empty() && fraction_part.is_empty() {
        return Err(CamtError::MalformedAmount {
            raw: raw.to_string(),
        });
    }
    if !integer_part.chars().all(|c| c.is_ascii_digit())
        || !fraction_part.chars().all(|c| c.is_ascii_digit())
    {
        return Err(CamtError::MalformedAmount {
            raw: raw.to_string(),
        });
    }
    if fraction_part.len() as u64 > u64::from(scale) {
        return Err(CamtError::TooManyFractionDigits {
            raw: raw.to_string(),
            scale,
        });
    }
    let integer: i64 = if integer_part.is_empty() {
        0
    } else {
        integer_part
            .parse()
            .map_err(|_| CamtError::MalformedAmount {
                raw: raw.to_string(),
            })?
    };
    let mut fraction: i64 = if fraction_part.is_empty() {
        0
    } else {
        fraction_part
            .parse()
            .map_err(|_| CamtError::MalformedAmount {
                raw: raw.to_string(),
            })?
    };
    for _ in fraction_part.len() as u64..u64::from(scale) {
        fraction *= 10;
    }
    Ok(sign * (integer * 10_i64.pow(scale) + fraction))
}

/// The inner text of the first `<tag>…</tag>` block.
fn tag_block(input: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let lower_open = open.to_lowercase();
    let open_pos = input.to_lowercase().find(&lower_open)?;
    // Advance past the tag name, then any attributes, then the closing `>`.
    let after_name = &input[open_pos + open.len()..];
    let gt = after_name.find('>')?;
    if after_name[..gt].trim_end().ends_with('/') {
        return Some(String::new()); // self-closing
    }
    let inner = &after_name[gt + 1..];
    let close = format!("</{tag}>");
    let close_rel = inner.to_lowercase().find(&close.to_lowercase())?;
    Some(inner[..close_rel].to_string())
}

/// The value of the first child element `child` inside blocks named `parent`.
fn first_tag_value(input: &str, tag: &str) -> Option<String> {
    let block = tag_block(input, tag)?;
    let first_line = block.lines().next()?.trim().to_string();
    if first_line.starts_with('<') {
        // The block's first line is another element; this tag is a container.
        None
    } else if first_line.is_empty() {
        None
    } else {
        Some(first_line)
    }
}

/// The attribute value of the first `<tag attr="…">` in the input.
fn first_attr_value(input: &str, tag: &str, attr: &str) -> Option<String> {
    let needle = format!("<{tag}");
    let lower = needle.to_lowercase();
    let pos = input.to_lowercase().find(&lower)?;
    let rest = &input[pos..];
    let gt = rest.find('>')?;
    let attrs = &rest[..gt];
    let attr_needle = format!("{attr}=\"");
    let attr_pos = attrs.find(&attr_needle)? + attr_needle.len();
    let end = attrs[attr_pos..].find('"')?;
    Some(attrs[attr_pos..attr_pos + end].to_string())
}

/// Split into the inner text of every `<tag>…</tag>` block.
///
/// Tracks a byte offset rather than re-slicing, so nothing indexes `str` by
/// position without a check.
fn split_blocks(input: &str, tag: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut offset = 0usize;
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let lower_open = open.to_lowercase();
    let lower_close = close.to_lowercase();
    while let Some(rel) = input[offset..].to_lowercase().find(&lower_open) {
        let open_pos = offset + rel;
        let after_name = &input[open_pos + open.len()..];
        let Some(gt_rel) = after_name.find('>') else {
            break;
        };
        if after_name[..gt_rel].trim_end().ends_with('/') {
            offset = open_pos + open.len() + gt_rel + 1;
            continue;
        }
        let inner_start = open_pos + open.len() + gt_rel + 1;
        let inner = &input[inner_start..];
        let Some(close_rel) = inner.to_lowercase().find(&lower_close) else {
            break;
        };
        blocks.push(inner[..close_rel].to_string());
        offset = inner_start + close_rel + close.len();
    }
    blocks
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Document xmlns="urn:iso:std:iso:20022:tech:xsd:camt.053.001.08">
<BkToCstmrStmt>
<GrpHdr><MsgId>MSG-1</MsgId><CreDtTm>2026-08-01T09:00:00</CreDtTm></GrpHdr>
<Stmt>
<Id>STMT-2026-07</Id>
<ElctrncSeqNb>7</ElctrncSeqNb>
<CreDtTm>2026-08-01T09:00:00</CreDtTm>
<Acct><Id><IBAN>GB29NWBK60161331926819</IBAN></Id><Ccy>GBP</Ccy></Acct>
<Bal>
<Tp><Cd>OPBD</Cd></Tp>
<Amt Ccy="GBP">1000.00</Amt>
<CdtDbtInd>CRDT</CdtDbtInd>
<Date>2026-07-01</Date>
</Bal>
<Bal>
<Tp><Cd>CLBD</Cd></Tp>
<Amt Ccy="GBP">2013.60</Amt>
<CdtDbtInd>CRDT</CdtDbtInd>
<Date>2026-07-31</Date>
</Bal>
<TxsSummry><TtlNtries><NbOfTxs>3</NbOfTtx></TtlNtries></TxsSummry>
<Ntry>
<Amt Ccy="GBP">1440.00</Amt>
<CdtDbtInd>CRDT</CdtDbtInd>
<Sts><Cd>BOOK</Cd></Sts>
<BookgDt><Dt>2026-07-03</Dt></BookgDt>
<AcctSvcrRef>BANK-001</AcctSvcrRef>
<AddtlNtryInf>PAYMENT INV-071</AddtlNtryInf>
</Ntry>
<Ntry>
<Amt Ccy="GBP">1056.00</Amt>
<CdtDbtInd>CRDT</CdtDbtInd>
<Sts><Cd>BOOK</Cd></Sts>
<BookgDt><Dt>2026-07-08</Dt></BookgDt>
<AcctSvcrRef>BANK-002</AcctSvcrRef>
<AddtlNtryInf>GLOBEX INV-072</AddtlNtryInf>
</Ntry>
<Ntry>
<Amt Ccy="GBP">82.40</Amt>
<CdtDbtInd>DBIT</CdtDbtInd>
<Sts><Cd>BOOK</Cd></Sts>
<BookgDt><Dt>2026-07-16</Dt></BookgDt>
<AcctSvcrRef>BANK-003</AcctSvcrRef>
<AddtlNtryInf>BANK CHARGES</AddtlNtryInf>
</Ntry>
<Ntry>
<Amt Ccy="GBP">500.00</Amt>
<CdtDbtInd>CRDT</CdtDbtInd>
<Sts><Cd>PDNG</Cd></Sts>
<BookgDt><Dt>2026-07-30</Dt></BookgDt>
<AcctSvcrRef>BANK-P1</AcctSvcrRef>
<AddtlNtryInf>PENDING FROM ACME</AddtlNtryInf>
</Ntry>
</Stmt>
</BkToCstmrStmt>
</Document>
"#;

    #[test]
    fn both_balances_make_the_import_check_real() {
        // 1000.00 + 1440.00 + 1056.00 − 82.40 = 3413.60, but CLBD says
        // 2013.60 — the fixture's arithmetic does not close, and the adapter
        // must refuse it with the error that names the numbers. This is the
        // check OFX cannot offer: there, the opening balance is derived and
        // the identity is trivially true.
        assert_eq!(
            parse_camt(FIXTURE),
            Err(CamtError::BalanceIdentityFailed {
                opening: 100_000,
                computed: 341_360,
                closing: 201_360,
            })
        );
    }

    #[test]
    fn a_consistent_statement_parses_with_both_balances_taken_at_face_value() {
        let fixed = FIXTURE.replace("<Amt Ccy=\"GBP\">2013.60", "<Amt Ccy=\"GBP\">3413.60");
        let statements = parse_camt(&fixed).expect("the corrected fixture closes");
        assert_eq!(statements.len(), 1);
        let statement = statements.first().expect("one statement");
        assert_eq!(statement.currency, "GBP");
        assert_eq!(statement.opening_balance, 100_000);
        assert_eq!(statement.closing_balance, 341_360);
        assert_eq!(statement.validate(), Ok(()));
        // Booked entries only: the PDNG 500.00 is not here.
        assert_eq!(statement.lines.len(), 3);
        let debit = statement.lines.get(2).expect("third line");
        assert_eq!(
            debit.direction,
            Direction::Debit,
            "DBIT maps without a sign"
        );
        assert_eq!(debit.amount, 8_240);
        assert_eq!(
            statement.lines.first().expect("first").description,
            "PAYMENT INV-071"
        );
    }

    #[test]
    fn the_closing_balance_is_the_banks_not_the_derived_one() {
        // The contrast with OFX, stated as a test: here the closing balance is
        // read from the document, so the identity is a check with teeth.
        let fixed = FIXTURE.replace("<Amt Ccy=\"GBP\">2013.60", "<Amt Ccy=\"GBP\">3413.60");
        let statements = parse_camt(&fixed).expect("parses");
        let statement = statements.first().expect("one statement");
        assert_eq!(statement.closing_balance, 341_360);
        assert_ne!(statement.opening_balance, 0);
    }

    #[test]
    fn pending_entries_never_reach_the_statement() {
        let fixed = FIXTURE.replace("<Amt Ccy=\"GBP\">2013.60", "<Amt Ccy=\"GBP\">3413.60");
        let statements = parse_camt(&fixed).expect("parses");
        let statement = statements.first().expect("one statement");
        assert!(
            !statement.lines.iter().any(|l| l.id == "BANK-P1"),
            "a pending entry has no booking date and may still be reversed"
        );
    }

    #[test]
    fn amounts_are_parsed_float_free_and_scale_bounded() {
        assert_eq!(parse_amount("0.07", 2).expect("parses"), 7);
        assert_eq!(parse_amount("-82.40", 2).expect("parses"), -8_240);
        assert_eq!(parse_amount("1440", 2).expect("parses"), 144_000);
        assert_eq!(parse_amount("5", 0).expect("parses"), 5, "JPY scale");
        assert!(matches!(
            parse_amount("1.234", 2),
            Err(CamtError::TooManyFractionDigits { scale: 2, .. })
        ));
        for garbage in ["", ".", "-.", "12,3.4", "abc"] {
            assert!(parse_amount(garbage, 2).is_err(), "{garbage:?}");
        }
    }

    #[test]
    fn a_statement_missing_its_closing_balance_is_refused() {
        let gutted = FIXTURE
            .replace("<Amt Ccy=\"GBP\">2013.60", "<Amt Ccy=\"GBP\">3413.60")
            .replace("<Cd>CLBD</Cd>", "<Cd>PRCD</Cd>");
        assert_eq!(parse_camt(&gutted), Err(CamtError::MissingClosingBalance));
    }

    #[test]
    fn a_non_camt_document_is_refused_up_front() {
        assert_eq!(
            parse_camt("<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pain.001.001.09\"/>"),
            Err(CamtError::NotAStatement)
        );
    }

    #[test]
    fn dates_must_be_iso_before_they_are_accepted() {
        let bad = FIXTURE
            .replace("<Amt Ccy=\"GBP\">2013.60", "<Amt Ccy=\"GBP\">3413.60")
            .replace("<Dt>2026-07-03</Dt>", "<Dt>3 July 2026</Dt>");
        assert!(matches!(
            parse_camt(&bad),
            Err(CamtError::MalformedDate { .. })
        ));
    }
}
