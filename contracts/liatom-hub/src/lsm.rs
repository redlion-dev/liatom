//! Liquid staking module (LSM, `x/liquid` on the Cosmos Hub): redeeming tokenized shares.
//!
//! A user tokenizes a delegation (`MsgTokenizeShares`), which pays out a share token with the denom
//! `<validator operator address>/<record id>`. Sending that token to the hub lets the contract redeem it
//! (`MsgRedeemTokensForShares`): the chain unbonds the record's delegation and at once delegates the same
//! amount of ATOM, as the redeemer's own delegation, to the same validator. No ATOM waits in an unbonding
//! queue and none touches the contract balance.

use cosmwasm_std::{AnyMsg, Binary, Coin};

/// Type URL of the message in gaia v29 (`proto/gaia/liquid/v1beta1/tx.proto`).
pub const REDEEM_TYPE_URL: &str = "/gaia.liquid.v1beta1.MsgRedeemTokensForShares";
pub const TOKENIZE_TYPE_URL: &str = "/gaia.liquid.v1beta1.MsgTokenizeShares";

/// The validator of a share token denom `<valoper>/<record id>`, or `None` if `denom` is not shaped like one.
///
/// Only the shape is checked here. The chain refuses a denom that is not a share token (the redeem
/// fails and takes the whole transaction with it).
pub fn share_token_validator(denom: &str) -> Option<&str> {
    let (validator, id) = denom.split_once('/')?;
    if validator.is_empty()
        || id.is_empty()
        || id.len() > 20
        || !id.bytes().all(|b| b.is_ascii_digit())
        || validator.contains('/')
    {
        return None;
    }
    Some(validator)
}

fn varint(mut n: usize, out: &mut Vec<u8>) {
    while n >= 0x80 {
        out.push((n as u8 & 0x7f) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn field(tag: u8, bytes: &[u8], out: &mut Vec<u8>) {
    out.push(tag);
    varint(bytes.len(), out);
    out.extend_from_slice(bytes);
}

/// `MsgRedeemTokensForShares { delegator_address = 1, amount = 2 (Coin { denom = 1, amount = 2 }) }`.
pub fn redeem_msg(delegator: &str, amount: &Coin) -> AnyMsg {
    let mut coin = vec![];
    field(0x0a, amount.denom.as_bytes(), &mut coin);
    field(0x12, amount.amount.to_string().as_bytes(), &mut coin);
    let mut value = vec![];
    field(0x0a, delegator.as_bytes(), &mut value);
    field(0x12, &coin, &mut value);
    AnyMsg {
        type_url: REDEEM_TYPE_URL.to_string(),
        value: Binary::from(value),
    }
}

/// `MsgTokenizeShares { delegator_address = 1, validator_address = 2, amount = 3, tokenized_share_owner = 4 }`.
pub fn tokenize_msg(delegator: &str, validator: &str, amount: &Coin, owner: &str) -> AnyMsg {
    let mut coin = vec![];
    field(0x0a, amount.denom.as_bytes(), &mut coin);
    field(0x12, amount.amount.to_string().as_bytes(), &mut coin);
    let mut value = vec![];
    field(0x0a, delegator.as_bytes(), &mut value);
    field(0x12, validator.as_bytes(), &mut value);
    field(0x1a, &coin, &mut value);
    field(0x22, owner.as_bytes(), &mut value);
    AnyMsg {
        type_url: TOKENIZE_TYPE_URL.to_string(),
        value: Binary::from(value),
    }
}

fn read_varint(b: &[u8], pos: &mut usize) -> Option<usize> {
    // u64 so that a 5-byte varint is not truncated on a 32-bit target (wasm32)
    let mut n: u64 = 0;
    let mut shift = 0;
    loop {
        let byte = *b.get(*pos)?;
        *pos += 1;
        n |= ((byte & 0x7f) as u64).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return usize::try_from(n).ok();
        }
        shift += 7;
        if shift > 28 {
            return None;
        }
    }
}

/// Skips a field of the given wire type (protobuf requires unknown fields to be skipped, so that a chain
/// that adds a field to a response does not break the parser).
fn skip_field(b: &[u8], pos: &mut usize, wire_type: usize) -> Option<()> {
    match wire_type {
        0 => {
            read_varint(b, pos)?;
        }
        1 => *pos = pos.checked_add(8).filter(|p| *p <= b.len())?,
        2 => {
            let len = read_varint(b, pos)?;
            *pos = pos.checked_add(len).filter(|p| *p <= b.len())?;
        }
        5 => *pos = pos.checked_add(4).filter(|p| *p <= b.len())?,
        _ => return None,
    }
    Some(())
}

/// Reads the share token out of `MsgTokenizeSharesResponse { amount = 1 (Coin { denom = 1, amount = 2 }) }`.
pub fn parse_tokenize_response(b: &[u8]) -> Option<Coin> {
    let mut pos = 0;
    let mut coin_bytes: Option<&[u8]> = None;
    while pos < b.len() {
        let tag = read_varint(b, &mut pos)?;
        if tag >> 3 == 1 && tag & 7 == 2 {
            let len = read_varint(b, &mut pos)?;
            let end = pos.checked_add(len)?;
            coin_bytes = Some(b.get(pos..end)?);
            pos = end;
        } else {
            skip_field(b, &mut pos, tag & 7)?;
        }
    }
    let c = coin_bytes?;
    let (mut denom, mut amount) = (None, None);
    let mut p = 0;
    while p < c.len() {
        let tag = read_varint(c, &mut p)?;
        if tag & 7 == 2 && matches!(tag >> 3, 1 | 2) {
            let len = read_varint(c, &mut p)?;
            let end = p.checked_add(len)?;
            let body = std::str::from_utf8(c.get(p..end)?).ok()?;
            match tag >> 3 {
                1 => denom = Some(body.to_string()),
                // digits only: Rust's parser would also take a leading "+"
                _ => {
                    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    amount = Some(body.parse::<u128>().ok()?)
                }
            }
            p = end;
        } else {
            skip_field(c, &mut p, tag & 7)?;
        }
    }
    Some(cosmwasm_std::coin(amount?, denom?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::coin;

    #[test]
    fn denom_shape() {
        assert_eq!(share_token_validator("cosmosvaloper1abc/12"), Some("cosmosvaloper1abc"));
        for bad in ["uatom", "cosmosvaloper1abc/", "/1", "a/b/1", "a/1x", "factory/cosmos1x/liatom", "a/123456789012345678901"] {
            assert_eq!(share_token_validator(bad), None, "{bad}");
        }
    }

    #[test]
    fn redeem_message_is_protobuf() {
        let m = redeem_msg("cosmos1x", &coin(5, "v/1"));
        assert_eq!(m.type_url, REDEEM_TYPE_URL);
        // 0a 08 "cosmos1x" | 12 08 (0a 03 "v/1" 12 01 "5")
        let mut want = vec![0x0a, 8];
        want.extend_from_slice(b"cosmos1x");
        want.extend_from_slice(&[0x12, 8, 0x0a, 3]);
        want.extend_from_slice(b"v/1");
        want.extend_from_slice(&[0x12, 1]);
        want.extend_from_slice(b"5");
        assert_eq!(m.value.to_vec(), want);
    }

    #[test]
    fn tokenize_message_and_response() {
        let m = tokenize_msg("cosmos1c", "cosmosvaloper1v", &coin(7, "uatom"), "cosmos1u");
        assert_eq!(m.type_url, TOKENIZE_TYPE_URL);
        let mut want = vec![0x0a, 8];
        want.extend_from_slice(b"cosmos1c");
        want.extend_from_slice(&[0x12, 15]);
        want.extend_from_slice(b"cosmosvaloper1v");
        want.extend_from_slice(&[0x1a, 10, 0x0a, 5]);
        want.extend_from_slice(b"uatom");
        want.extend_from_slice(&[0x12, 1]);
        want.extend_from_slice(b"7");
        want.extend_from_slice(&[0x22, 8]);
        want.extend_from_slice(b"cosmos1u");
        assert_eq!(m.value.to_vec(), want);

        // response: amount = 1 { denom = 1, amount = 2 }
        let mut resp = vec![0x0a, 15, 0x0a, 6];
        resp.extend_from_slice(b"val/12");
        resp.extend_from_slice(&[0x12, 5]);
        resp.extend_from_slice(b"12345");
        assert_eq!(parse_tokenize_response(&resp), Some(coin(12345, "val/12")));
        assert_eq!(parse_tokenize_response(&[]), None);
        assert_eq!(parse_tokenize_response(&[0x0a, 40, 1]), None);
        assert_eq!(parse_tokenize_response(&[0x08, 1]), None);
    }

    #[test]
    fn long_fields_use_multibyte_lengths() {
        let long = "x".repeat(200);
        let m = redeem_msg(&long, &coin(1, "v/1"));
        assert_eq!(&m.value[..3], &[0x0a, 0xc8, 0x01]);
    }
}
