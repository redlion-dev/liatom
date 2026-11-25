//! Wire-format checks (review of commit e896e13, parsing focus).
//!
//! Every message and query that the web UI (web/src/main.ts, chain.ts), the keeper (scripts/keeper/keeper.py),
//! the dev scripts (scripts/dev/*.sh) and the instantiate example send must deserialise into the contract's real
//! types and serialise back to the same JSON. Every response a client reads must carry the field names and the
//! JSON types that client expects.

use cosmwasm_std::{Addr, Decimal, Timestamp, Uint128};
use liatom_hub::msg::{ExchangeRateResponse, ExecuteMsg, InstantiateMsg, PoolResponse, QueryMsg};
use liatom_hub::state::{Batch, BatchStatus, Config, PauseMode, ValidatorWeight};
use serde_json::{json, Value};

const VALOPER: &str = "cosmosvaloper1red54x4vu6ncz4hq8jw72z2u6he7fkzlyjsle0";

/// Drops `null` members, so an `Option` field a client leaves out compares equal to the re-serialised `null`.
fn strip_nulls(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .filter(|(_, x)| !x.is_null())
                .map(|(k, x)| (k, strip_nulls(x)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

/// ExecuteMsg JSON exactly as the clients build it. The comment names the sender.
fn execute_messages() -> Vec<(&'static str, Value)> {
    vec![
        // web/src/main.ts
        ("web deposit", json!({"deposit": {}})),
        ("web redeem", json!({"redeem": {}})),
        ("web deposit_tokenized_shares", json!({"deposit_tokenized_shares": {}})),
        ("web redeem_to_shares", json!({"redeem_to_shares": {}})),
        ("web claim", json!({"claim": {"batch_id": 1}})),
        ("web finalize_batch", json!({"finalize_batch": {"batch_id": 1}})),
        // scripts/keeper/keeper.py
        ("keeper harvest", json!({"harvest": {}})),
        ("keeper submit_unbonding", json!({"submit_unbonding": {}})),
        ("keeper finalize_batch", json!({"finalize_batch": {"batch_id": 7}})),
        // scripts/dev/smoke-*.sh and keeper-e2e.sh
        ("dev update_config fee+min", json!({"update_config": {"fee_bps": 500, "min_deposit": "2000000"}})),
        ("dev update_config max_tvl", json!({"update_config": {"max_tvl": "1000000"}})),
        ("dev update_config max_tvl MAX", json!({"update_config": {"max_tvl": "340282366920938463463374607431768211455"}})),
        ("dev update_config epoch", json!({"update_config": {"epoch_secs": 10}})),
        ("dev update_validators", json!({"update_validators": {"validators": [
            {"validator": VALOPER, "weight_bps": 5000},
            {"validator": "cosmosvaloper1zzz", "weight_bps": 5000}
        ]}})),
        ("dev pause deposits_only", json!({"pause": {"mode": "deposits_only"}})),
        ("dev pause full", json!({"pause": {"mode": "full"}})),
        ("dev unpause", json!({"unpause": {}})),
        ("dev rebalance", json!({"rebalance": {}})),
    ]
}

fn query_messages() -> Vec<(&'static str, Value)> {
    vec![
        ("web/keeper config", json!({"config": {}})),
        ("web/keeper pool", json!({"pool": {}})),
        ("web exchange_rate", json!({"exchange_rate": {}})),
        ("web validators", json!({"validators": {}})),
        ("web/keeper batch", json!({"batch": {"batch_id": 3}})),
        ("web claim", json!({"claim": {"batch_id": 3, "address": "cosmos1abc"}})),
    ]
}

#[test]
fn every_client_execute_message_parses_and_round_trips() {
    for (who, msg) in execute_messages() {
        let parsed: ExecuteMsg = serde_json::from_value(msg.clone())
            .unwrap_or_else(|e| panic!("{who}: {msg} does not parse as ExecuteMsg: {e}"));
        let back = strip_nulls(serde_json::to_value(&parsed).unwrap());
        assert_eq!(back, msg, "{who}: contract reads it as something else");
    }
}

#[test]
fn every_client_query_parses_and_round_trips() {
    for (who, q) in query_messages() {
        let parsed: QueryMsg = serde_json::from_value(q.clone())
            .unwrap_or_else(|e| panic!("{who}: {q} does not parse as QueryMsg: {e}"));
        assert_eq!(serde_json::to_value(&parsed).unwrap(), q, "{who}");
    }
}

#[test]
fn example_and_smoke_test_instantiate_parse() {
    let template = include_str!("fixtures/instantiate.example.json");
    let parsed: InstantiateMsg = serde_json::from_str(template).expect("template is an InstantiateMsg");
    assert_eq!(parsed.denom_creation_fee, Uint128::new(10_000_000));
    assert_eq!(parsed.max_tvl, Uint128::new(100_000_000));

    // scripts/dev/smoke-test.sh (jq-built), with u128::MAX as max_tvl
    let smoke: InstantiateMsg = serde_json::from_value(json!({
        "admin": "cosmos1admin", "treasury": "cosmos1admin", "fee_bps": 1000, "epoch_secs": 60,
        "unbonding_secs": 300, "min_deposit": "1000000",
        "max_tvl": "340282366920938463463374607431768211455",
        "denom_creation_fee": "10000000",
        "validators": [{"validator": VALOPER, "weight_bps": 10000}]
    }))
    .expect("smoke-test instantiate parses");
    assert_eq!(smoke.max_tvl, Uint128::MAX);
}

/// Unit-variant names are snake_case on the wire, and a message with two variants is refused.
#[test]
fn variant_names_are_strict() {
    assert!(serde_json::from_value::<ExecuteMsg>(json!({"Deposit": {}})).is_err());
    assert!(serde_json::from_value::<ExecuteMsg>(json!({"deposit": {}, "redeem": {}})).is_err());
}

/// cosmwasm-schema-derive 2.2.11 `cw_serde` adds only Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema
/// and `rename_all = "snake_case"` for enums: it does NOT add `deny_unknown_fields`. So an extra key in the
/// instantiate message is ignored. The example's required fields still catch most typos (a misspelled
/// required key fails), but an extra or misspelled optional key is silently dropped. Severity Low.
#[test]
fn finding_unknown_field_in_instantiate_is_refused() {
    let mut tpl: Value = serde_json::from_str(include_str!("fixtures/instantiate.example.json")).unwrap();
    tpl["treasury_typo"] = json!("cosmos1x");
    assert!(serde_json::from_value::<InstantiateMsg>(tpl).is_err(), "unknown key was silently ignored");
}

/// A typo in a field of a struct variant (`{"claim": {...}}`, `{"update_config": {...}}`) is silently
/// dropped, because `cw_serde` does not deny unknown fields at all (see the instantiate finding below).
/// Concrete effect: an admin who writes `{"update_config":{"fee_bps":500,"min_depsoit":"2000000"}}` gets
/// the fee changed and the deposit minimum left alone, with no error. Severity Low (admin ops, no direct
/// fund movement; the check-then-send workflow is the mitigation).
#[test]
fn finding_typo_in_a_struct_variant_field_is_refused() {
    assert!(
        serde_json::from_value::<ExecuteMsg>(json!({"update_config": {"fee_bps": 500, "min_depsoit": "2000000"}}))
            .is_err(),
        "unknown field min_depsoit was silently ignored"
    );
    assert!(
        serde_json::from_value::<ExecuteMsg>(json!({"claim": {"batch_id": 1, "batch": 1}})).is_err(),
        "unknown field batch was silently ignored"
    );
    assert!(
        serde_json::from_value::<ExecuteMsg>(json!({"rebalance": {"min_mov": 5}})).is_err(),
        "unknown field min_mov was silently ignored (rebalance then uses the 1 ATOM default)"
    );
    assert!(serde_json::from_value::<QueryMsg>(json!({"batch": {"batch_id": 1, "id": 1}})).is_err());
}

#[test]
fn execute_variant_names_are_snake_case_on_the_wire() {
    for (who, msg) in [
        ("deposit", json!({"deposit": {}})),
        ("finalize", json!({"finalize_batch": {"batch_id": 1}})),
    ] {
        assert!(serde_json::from_value::<ExecuteMsg>(msg).is_ok(), "{who}");
    }
    // a client sending CamelCase would be refused
    assert!(serde_json::from_value::<ExecuteMsg>(json!({"FinalizeBatch": {"batch_id": 1}})).is_err());
}

#[test]
fn config_response_has_the_types_the_clients_read() {
    let cfg = Config {
        admin: Addr::unchecked("cosmos1admin"),
        pending_admin: None,
        treasury: Addr::unchecked("cosmos1treasury"),
        fee_bps: 1000,
        epoch_secs: 345_600,
        unbonding_secs: 1_814_400,
        min_unbonding_secs: 1_587_600,
        min_deposit: Uint128::new(1_000_000),
        max_tvl: Uint128::new(100_000_000),
        pause: Some(PauseMode::DepositsOnly),
        liatom_denom: "factory/cosmos1contract/liatom".into(),
        bond_denom: "uatom".into(),
    };
    let v = serde_json::to_value(&cfg).unwrap();
    // web/src/chain.ts ContractConfig
    for k in ["admin", "treasury", "min_deposit", "max_tvl", "liatom_denom", "bond_denom"] {
        assert!(v[k].is_string(), "{k} must be a string, got {}", v[k]);
    }
    // web: fee_bps: number; keeper: int(cfg["unbonding_secs"]) and cfg["epoch_secs"]
    for k in ["fee_bps", "epoch_secs", "unbonding_secs"] {
        assert!(v[k].is_u64(), "{k} must be a JSON number, got {}", v[k]);
    }
    // web: pause: null | "deposits_only" | "full"
    assert_eq!(v["pause"], json!("deposits_only"));
    assert_eq!(serde_json::to_value(PauseMode::Full).unwrap(), json!("full"));
    let no_pause = Config { pause: None, ..cfg };
    assert!(serde_json::to_value(&no_pause).unwrap()["pause"].is_null());
}

#[test]
fn pool_and_rate_responses_have_the_fields_the_web_reads() {
    let pool = PoolResponse {
        delegated: Uint128::new(5),
        unclaimed_rewards: Uint128::new(6),
        accrued_fee: Uint128::new(1),
        owed_to_redeemers: Uint128::new(2),
        pending_liatom: Uint128::new(3),
        pool_value: Uint128::new(10),
        liatom_supply: Uint128::new(4),
    };
    let v = serde_json::to_value(&pool).unwrap();
    for k in [
        "delegated", "unclaimed_rewards", "accrued_fee", "owed_to_redeemers",
        "pending_liatom", "pool_value", "liatom_supply",
    ] {
        assert!(v[k].is_string(), "pool.{k} must be a string");
    }
    let rate = serde_json::to_value(ExchangeRateResponse { rate: Decimal::percent(125) }).unwrap();
    assert_eq!(rate, json!({"rate": "1.25"}));
}

#[test]
fn batch_response_matches_the_web_and_keeper_readers() {
    let open = Batch {
        id: 2,
        status: BatchStatus::Open,
        liatom_burned: Uint128::new(7),
        atom_expected: None,
        atom_received: None,
        mature_at: None,
        undelegations: 0,
        shortfall_noted_at: None,
        submitted_at: None,
    };
    let v = serde_json::to_value(&open).unwrap();
    assert_eq!(v["id"], json!(2));
    assert_eq!(v["status"], json!("open"));
    assert!(v["atom_expected"].is_null() && v["mature_at"].is_null() && v["submitted_at"].is_null());

    // Timestamp is a string of nanoseconds; keeper.py to_secs() and web nanosToSecs() divide it.
    let t = Timestamp::from_seconds(1_700_000_000);
    let unbonding = Batch {
        status: BatchStatus::Unbonding,
        atom_expected: Some(Uint128::new(9)),
        mature_at: Some(t),
        submitted_at: Some(Timestamp::from_seconds(1_699_000_000)),
        ..open.clone()
    };
    let v = serde_json::to_value(&unbonding).unwrap();
    assert_eq!(v["status"], json!("unbonding"));
    assert_eq!(v["mature_at"], json!("1700000000000000000"));
    assert_eq!(v["submitted_at"], json!("1699000000000000000"));
    assert_eq!(v["atom_expected"], json!("9"));

    let matured = Batch { status: BatchStatus::Matured, ..unbonding };
    assert_eq!(serde_json::to_value(&matured).unwrap()["status"], json!("matured"));

    // Batches written before `undelegations`, `shortfall_noted_at` and `submitted_at` existed must still load.
    let old: Batch = serde_json::from_value(json!({
        "id": 1, "status": "unbonding", "liatom_burned": "5", "atom_expected": "5",
        "atom_received": null, "mature_at": "1700000000000000000"
    }))
    .expect("old batch layout loads");
    assert_eq!(old.undelegations, 0);
    assert!(old.submitted_at.is_none());
}

#[test]
fn validators_and_claim_responses_have_the_shapes_the_clients_read() {
    let v = serde_json::to_value(vec![ValidatorWeight { validator: VALOPER.into(), weight_bps: 10_000 }]).unwrap();
    // web: `{ validator: string }[]`
    assert_eq!(v[0]["validator"], json!(VALOPER));
    assert_eq!(v[0]["weight_bps"], json!(10_000));
    // web userClaim(): `BigInt(v ?? "0")`, so the claim is a JSON string
    assert_eq!(serde_json::to_value(Uint128::new(42)).unwrap(), json!("42"));
}
