//! Custom messages understood by the Cosmos Hub wasm bindings
//! (`github.com/cosmos/tokenfactory/x/tokenfactory/bindings`).
//! The JSON is passed as `CosmosMsg::Custom` and decoded by the chain.

use cosmwasm_schema::cw_serde;
use cosmwasm_std::{CosmosMsg, CustomMsg, Uint128};

#[cw_serde]
pub enum TokenFactoryMsg {
    CreateDenom {
        subdenom: String,
    },
    MintTokens {
        denom: String,
        amount: Uint128,
        mint_to_address: String,
    },
    /// An empty `burn_from_address` burns from the contract's own balance.
    BurnTokens {
        denom: String,
        amount: Uint128,
        burn_from_address: String,
    },
    /// Writes bank denom metadata. The contract must be the denom admin and `metadata.base`
    /// must equal `denom`.
    SetMetadata {
        denom: String,
        metadata: Metadata,
    },
}

#[cw_serde]
pub struct Metadata {
    pub description: String,
    pub denom_units: Vec<DenomUnit>,
    pub base: String,
    pub display: String,
    pub name: String,
    pub symbol: String,
}

#[cw_serde]
pub struct DenomUnit {
    pub denom: String,
    pub exponent: u32,
    pub aliases: Vec<String>,
}

impl CustomMsg for TokenFactoryMsg {}

pub fn create_denom(subdenom: &str) -> CosmosMsg<TokenFactoryMsg> {
    CosmosMsg::Custom(TokenFactoryMsg::CreateDenom {
        subdenom: subdenom.to_string(),
    })
}

pub fn mint(denom: &str, amount: Uint128, to: &str) -> CosmosMsg<TokenFactoryMsg> {
    CosmosMsg::Custom(TokenFactoryMsg::MintTokens {
        denom: denom.to_string(),
        amount,
        mint_to_address: to.to_string(),
    })
}

pub fn burn_own(denom: &str, amount: Uint128) -> CosmosMsg<TokenFactoryMsg> {
    CosmosMsg::Custom(TokenFactoryMsg::BurnTokens {
        denom: denom.to_string(),
        amount,
        burn_from_address: String::new(),
    })
}

/// Wallet-facing metadata for liATOM: 6 decimals like ATOM, shown as "liATOM".
pub fn set_metadata(denom: &str) -> CosmosMsg<TokenFactoryMsg> {
    CosmosMsg::Custom(TokenFactoryMsg::SetMetadata {
        denom: denom.to_string(),
        metadata: Metadata {
            description: "Liquid staked ATOM issued by the liATOM hub contract.".to_string(),
            denom_units: vec![
                DenomUnit {
                    denom: denom.to_string(),
                    exponent: 0,
                    aliases: vec![],
                },
                DenomUnit {
                    denom: DISPLAY_DENOM.to_string(),
                    exponent: 6,
                    aliases: vec![],
                },
            ],
            base: denom.to_string(),
            display: DISPLAY_DENOM.to_string(),
            name: "Liquid Staked ATOM".to_string(),
            symbol: "liATOM".to_string(),
        },
    })
}

pub const DISPLAY_DENOM: &str = "liatom";

pub fn full_denom(contract: &str, subdenom: &str) -> String {
    format!("factory/{contract}/{subdenom}")
}
