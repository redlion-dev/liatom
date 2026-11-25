//! Fuzzing of the hand-written protobuf and denom parsing in `src/lsm.rs` (review of e896e13).
//!
//! Encoders and decoders here are written independently of the contract (the contract's `field` and
//! `read_varint` are private), so a round trip checks the bytes against the protobuf rules and not against
//! the implementation itself. `finding_*` tests are expected to FAIL on the current code; each one states the
//! behaviour a protobuf or chain client would expect.

use cosmwasm_std::{coin, Coin};
use liatom_hub::lsm::{
    parse_tokenize_response, redeem_msg, share_token_validator, tokenize_msg, REDEEM_TYPE_URL,
    TOKENIZE_TYPE_URL,
};
use proptest::prelude::*;

// ---------- independent protobuf helpers -------------------------------------------------------

fn enc_varint(mut n: u64, out: &mut Vec<u8>) {
    while n >= 0x80 {
        out.push((n as u8) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn enc_len_field(field: u64, bytes: &[u8], out: &mut Vec<u8>) {
    enc_varint((field << 3) | 2, out);
    enc_varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn enc_coin(denom: &str, amount: &str) -> Vec<u8> {
    let mut c = vec![];
    enc_len_field(1, denom.as_bytes(), &mut c);
    enc_len_field(2, amount.as_bytes(), &mut c);
    c
}

/// `MsgTokenizeSharesResponse { amount = 1 }`
fn enc_tokenize_response(denom: &str, amount: &str) -> Vec<u8> {
    let mut out = vec![];
    enc_len_field(1, &enc_coin(denom, amount), &mut out);
    out
}

/// Decodes a message made only of length-delimited fields. `None` on anything else.
fn decode_len_fields(mut b: &[u8]) -> Option<Vec<(u64, Vec<u8>)>> {
    let mut out = vec![];
    while !b.is_empty() {
        let (tag, n) = dec_varint(b)?;
        b = &b[n..];
        if tag & 7 != 2 {
            return None;
        }
        let (len, n) = dec_varint(b)?;
        b = &b[n..];
        let len = usize::try_from(len).ok()?;
        if len > b.len() {
            return None;
        }
        out.push((tag >> 3, b[..len].to_vec()));
        b = &b[len..];
    }
    Some(out)
}

/// Full-width varint decoder (up to 10 bytes), returns (value, bytes used).
fn dec_varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut n: u64 = 0;
    for (i, byte) in b.iter().enumerate().take(10) {
        if i == 9 && *byte > 1 {
            return None; // overflows u64
        }
        n |= ((byte & 0x7f) as u64) << (7 * i);
        if (byte & 0x80) == 0 {
            return Some((n, i + 1));
        }
    }
    None
}

fn any_text() -> impl Strategy<Value = String> {
    // any Unicode scalar values, including NUL and combining marks
    prop::collection::vec(any::<char>(), 0..200).prop_map(|cs| cs.into_iter().collect())
}

// ---------- share token denoms -----------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// Any string: no panic, and whatever is accepted is exactly `<prefix>/<digits>` with a non-empty prefix.
    #[test]
    fn share_denom_shape_is_exact(s in any_text()) {
        if let Some(v) = share_token_validator(&s) {
            let (prefix, id) = s.split_once('/').expect("accepted denoms contain a slash");
            prop_assert_eq!(v, prefix);            // returned byte for byte, no normalisation
            prop_assert!(!v.is_empty());
            prop_assert!(!v.contains('/'));
            prop_assert!((1..=20).contains(&id.len()));
            prop_assert!(id.bytes().all(|b| b.is_ascii_digit()));
        }
    }

    /// Valid-looking denoms with arbitrary prefixes (non-ASCII, uppercase, NULs) are all accepted by shape.
    #[test]
    fn well_formed_share_denoms_are_accepted(prefix in "[^/]{1,80}", id in 0u64..=u64::MAX) {
        let denom = format!("{prefix}/{id}");
        prop_assert_eq!(share_token_validator(&denom), Some(prefix.as_str()));
    }

    #[test]
    fn two_slashes_or_non_digits_are_refused(a in "[^/]{1,20}", b in "[^/]{1,20}", c in "[0-9]{1,5}") {
        let three = format!("{a}/{b}/{c}");
        prop_assert_eq!(share_token_validator(&three), None);
        let two = format!("{a}/{b}");
        if b.bytes().any(|x| !x.is_ascii_digit()) {
            prop_assert_eq!(share_token_validator(&two), None);
        }
    }
}

#[test]
fn share_denom_edge_cases() {
    for bad in ["", "/", "//", "/1", "a/", "a//1", "a/1/", "a/ 1", "a/1 ", "a/+1", "a/-1", "a/\u{0}1",
                "a/\u{FF11}\u{FF12}", // full-width digits are not ASCII digits
                "a/123456789012345678901"] // 21 digits
    {
        assert_eq!(share_token_validator(bad), None, "{bad:?}");
    }
    // u64::MAX has 20 digits and is accepted
    assert!(share_token_validator(&format!("v/{}", u64::MAX)).is_some());
}

// ---------- tokenize response parsing ----------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// Encoding then parsing returns the same coin, for any denom text and any u128 amount.
    #[test]
    fn tokenize_response_round_trips(denom in any_text(), amount in any::<u128>()) {
        let bytes = enc_tokenize_response(&denom, &amount.to_string());
        prop_assert_eq!(parse_tokenize_response(&bytes), Some(coin(amount, denom)));
    }

    /// Every strict prefix of a valid response is refused (the outer length always points past the end).
    #[test]
    fn every_truncation_is_refused(denom in "[a-z0-9]{0,40}/[0-9]{1,6}", amount in any::<u64>()) {
        let bytes = enc_tokenize_response(&denom, &amount.to_string());
        for cut in 0..bytes.len() {
            prop_assert_eq!(parse_tokenize_response(&bytes[..cut]), None, "cut at {}", cut);
        }
    }

    /// Arbitrary bytes: never a panic, and anything accepted is a fixed point of encode-then-parse.
    #[test]
    fn random_bytes_never_panic_and_accepted_ones_are_stable(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        if let Some(c) = parse_tokenize_response(&bytes) {
            let again = enc_tokenize_response(&c.denom, &c.amount.to_string());
            prop_assert_eq!(parse_tokenize_response(&again), Some(c));
        }
    }

    /// Adversarial prefixes made of continuation bytes and huge length claims: no panic, no allocation
    /// beyond the input size (the parser only copies what the input holds).
    #[test]
    fn hostile_varints_never_panic(
        tag in prop_oneof![Just(0x0au8), Just(0x12u8), Just(0x08u8), any::<u8>()],
        junk in prop::collection::vec(prop_oneof![Just(0x80u8), Just(0xffu8), any::<u8>()], 0..24),
    ) {
        let mut b = vec![tag];
        b.extend(junk);
        let _ = parse_tokenize_response(&b);
        let mut nested = vec![0x0a, 0x0a];
        nested.extend_from_slice(&b);
        let _ = parse_tokenize_response(&nested);
    }
}

#[test]
fn overlong_and_huge_lengths_are_refused() {
    // outer field 1, length as a 5-byte varint (2^32 - 1), then nothing
    assert_eq!(parse_tokenize_response(&[0x0a, 0xff, 0xff, 0xff, 0xff, 0x0f]), None);
    // length with six continuation bytes (overlong)
    assert_eq!(parse_tokenize_response(&[0x0a, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01]), None);
    // 10-byte varint = u64::MAX as length
    let mut huge = vec![0x0a];
    huge.extend_from_slice(&[0xff; 9]);
    huge.push(0x01);
    assert_eq!(parse_tokenize_response(&huge), None);
    // a 1 MiB run of continuation bytes must fail fast
    assert_eq!(parse_tokenize_response(&vec![0x80u8; 1 << 20]), None);
    // non-UTF-8 denom, with a valid amount, so only the UTF-8 check can refuse it
    let mut coin_bytes = enc_len_field_vec(1, &[0xff, 0xfe]);
    enc_len_field(2, b"5", &mut coin_bytes);
    let bad = enc_len_field_vec(1, &coin_bytes);
    assert_eq!(parse_tokenize_response(&bad), None);
    // amount that is not a u128
    assert_eq!(parse_tokenize_response(&enc_tokenize_response("v/1", "340282366920938463463374607431768211456")), None);
    // empty input and tag without length
    assert_eq!(parse_tokenize_response(&[]), None);
    assert_eq!(parse_tokenize_response(&[0x0a]), None);
}

fn enc_len_field_vec(field: u64, bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    enc_len_field(field, bytes, &mut out);
    out
}

#[test]
fn lengths_around_varint_boundaries_round_trip() {
    for n in [0usize, 1, 127, 128, 129, 16_383, 16_384, 16_385, 70_000] {
        let denom = format!("{}/1", "v".repeat(n));
        let bytes = enc_tokenize_response(&denom, "12345");
        assert_eq!(parse_tokenize_response(&bytes), Some(coin(12_345, denom.clone())), "len {n}");
    }
}

// ---------- message builders -------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// `redeem_msg` must encode exactly `delegator=1` and `Coin{denom=1, amount=2}` as `amount=2`.
    #[test]
    fn redeem_message_fields_round_trip(delegator in any_text(), denom in any_text(), amount in any::<u128>()) {
        let m = redeem_msg(&delegator, &coin(amount, denom.clone()));
        prop_assert_eq!(m.type_url.as_str(), REDEEM_TYPE_URL);
        let fields = decode_len_fields(&m.value).expect("well-formed protobuf");
        prop_assert_eq!(fields.len(), 2);
        prop_assert_eq!(fields[0].0, 1);
        prop_assert_eq!(&fields[0].1, delegator.as_bytes());
        prop_assert_eq!(fields[1].0, 2);
        let c = decode_len_fields(&fields[1].1).expect("coin is protobuf");
        prop_assert_eq!(&c[0], &(1u64, denom.into_bytes()));
        prop_assert_eq!(&c[1], &(2u64, amount.to_string().into_bytes()));
    }

    /// `tokenize_msg`: delegator=1, validator=2, amount=3 (Coin), owner=4.
    #[test]
    fn tokenize_message_fields_round_trip(
        delegator in any_text(), validator in any_text(), denom in any_text(),
        amount in any::<u128>(), owner in any_text(),
    ) {
        let m = tokenize_msg(&delegator, &validator, &coin(amount, denom.clone()), &owner);
        prop_assert_eq!(m.type_url.as_str(), TOKENIZE_TYPE_URL);
        let fields = decode_len_fields(&m.value).expect("well-formed protobuf");
        prop_assert_eq!(fields.len(), 4);
        prop_assert_eq!(&fields[0], &(1u64, delegator.into_bytes()));
        prop_assert_eq!(&fields[1], &(2u64, validator.into_bytes()));
        prop_assert_eq!(&fields[3], &(4u64, owner.into_bytes()));
        let c = decode_len_fields(&fields[2].1).expect("coin is protobuf");
        prop_assert_eq!(&c[1], &(2u64, amount.to_string().into_bytes()));
        prop_assert_eq!(&c[0], &(1u64, denom.into_bytes()));
    }
}

#[test]
fn message_lengths_use_multibyte_varints_correctly() {
    let long = "x".repeat(16_400);
    let m = redeem_msg(&long, &Coin::new(1u128, "v/1"));
    let fields = decode_len_fields(&m.value).unwrap();
    assert_eq!(fields[0].1.len(), 16_400);
    // the amount must still be well formed after a 3-byte length
    assert!(decode_len_fields(&fields[1].1).is_some());
}

// ---------- findings (expected to fail on the current code) ------------------------------------

/// Protobuf says unknown fields are skipped. A chain that adds a varint field (wire type 0) to
/// `MsgTokenizeSharesResponse` would make `RedeemToShares` fail for every user (the parser returns None,
/// the reply reports BadTokenization). Severity Low: the chain's message is fixed in gaia v29, so the
/// exposure is a future upgrade, and only `RedeemToShares` is affected (Redeem still works).
#[test]
fn finding_unknown_varint_field_in_tokenize_response_is_skipped() {
    let mut bytes = enc_tokenize_response("val/12", "5");
    bytes.extend_from_slice(&[0x10, 0x01]); // field 2, varint 1 (unknown to the contract)
    assert_eq!(parse_tokenize_response(&bytes), Some(coin(5, "val/12")));
}

/// Same rule one level down: an unknown varint field inside the Coin.
#[test]
fn finding_unknown_varint_field_inside_coin_is_skipped() {
    let mut coin_bytes = enc_coin("val/12", "5");
    coin_bytes.extend_from_slice(&[0x18, 0x07]); // field 3, varint 7
    let mut bytes = vec![];
    enc_len_field(1, &coin_bytes, &mut bytes);
    assert_eq!(parse_tokenize_response(&bytes), Some(coin(5, "val/12")));
}

/// `u128::from_str` accepts a leading '+', so the parser takes "+5" as 5. The chain never writes that, so
/// there is no exploit; it is listed because a strict parser should refuse a non-canonical amount. Severity Info.
#[test]
fn finding_non_canonical_amount_is_refused() {
    assert_eq!(parse_tokenize_response(&enc_tokenize_response("val/12", "+5")), None);
}
