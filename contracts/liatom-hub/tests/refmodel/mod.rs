//! Independent reference model of the liATOM protocol accounting.
//!
//! Written from docs/architecture.md (sections 4, 5, 8) and docs/ai-audit.md only. It does not use any
//! type or function of the contract crate: plain integers and strings. Arithmetic is deliberately naive
//! (u128 with checked multiplication; every quantity here fits).
//!
//! The model owns its own protocol state (pending liATOM, owed, batches, claims, config mirror). It reads
//! only what the *chain* provides to the contract: delegations, pending rewards, liATOM supply, balance
//! (struct `Env`). For each contract transaction it predicts accept/refuse and the observable effects
//! and returns the list of disagreements with the contract's observed behaviour.

use std::collections::BTreeMap;
use std::sync::Mutex;

pub const BPS: u128 = 10_000;
const DAY: u64 = 86_400;

pub static HIST: Mutex<BTreeMap<String, BTreeMap<i128, u64>>> = Mutex::new(BTreeMap::new());
pub static INFO: Mutex<BTreeMap<String, (u64, String)>> = Mutex::new(BTreeMap::new());

pub fn hist(name: &str, v: i128) {
    *HIST.lock().unwrap().entry(name.to_string()).or_default().entry(v).or_default() += 1;
}
pub fn info(kind: &str, detail: String) {
    let mut m = INFO.lock().unwrap();
    let e = m.entry(kind.to_string()).or_insert((0, detail));
    e.0 += 1;
}

fn mulfloor(a: u128, b: u128, c: u128) -> u128 {
    a.checked_mul(b).expect("model overflow") / c
}

#[derive(Clone, Debug, Default)]
pub struct Env {
    pub now: u64,
    pub height: u64,
    pub stake: Vec<(String, u128)>,
    pub rewards: Vec<(String, u128)>,
    /// total liATOM in existence (bank supply)
    pub supply: u128,
    /// ATOM balance of the contract
    pub balance: u128,
    /// ATOM held by the (all) treasury address(es)
    pub treasury: u128,
    pub treasury_blocked: bool,
    pub skip_treasury: bool,
    /// the chain has slashed at some point (share/token rounding exists from then on)
    pub slashed: bool,
}

impl Env {
    fn stake_of(&self, v: &str) -> u128 {
        self.stake.iter().filter(|(n, _)| n == v).map(|(_, a)| *a).sum()
    }
    fn reward_of(&self, v: &str) -> u128 {
        self.rewards.iter().filter(|(n, _)| n == v).map(|(_, a)| *a).sum()
    }
    fn delegated(&self) -> u128 {
        self.stake.iter().map(|(_, a)| *a).sum()
    }
    fn unclaimed(&self) -> u128 {
        self.rewards.iter().map(|(_, a)| *a).sum()
    }
}

#[derive(Clone, Debug)]
pub enum Call {
    Deposit { who: String, amount: u128 },
    Redeem { who: String, amount: u128 },
    RedeemToShares { who: String, amount: u128 },
    DepositShares { who: String, validator: String },
    Claim { who: String, id: u64 },
    Harvest,
    Submit,
    Finalize { id: u64 },
    SetFee(u128),
    SetEpoch(u64),
    SetUnbonding(u64),
    SetMinDeposit(u128),
    SetMaxTvl(u128),
    Pause(u8),
    UpdateValidators(Vec<(String, u128)>),
    Other,
}

#[derive(Clone, Debug, Default)]
pub struct Obs {
    pub ok: bool,
    pub err: String,
    /// Undelegate entries created by the call: (validator, tokens)
    pub new_unbonding: Vec<(String, u128)>,
    /// tokenizations made by the call: (validator, tokens asked)
    pub tokenized: Vec<(String, u128)>,
    /// redemption of a share token into the contract: (validator, delegation before, delegation right after)
    pub dep: Option<(String, u128, u128)>,
    /// ATOM change of the sender (excluding attached funds)
    pub sender_delta: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum St {
    Open,
    Unbonding,
    Matured,
}

#[derive(Clone, Debug)]
pub struct RB {
    pub burned: u128,
    pub expected: Option<u128>,
    pub received: Option<u128>,
    pub status: St,
    pub mature_at: u64,
    pub undeleg: u32,
    pub noted: Option<u64>,
}
fn new_rb() -> RB {
    RB { burned: 0, expected: None, received: None, status: St::Open, mature_at: 0, undeleg: 0, noted: None }
}

/// What the contract shows through its queries.
#[derive(Clone, Debug, Default)]
pub struct BView {
    pub burned: u128,
    pub status: u8,
    pub expected: Option<u128>,
    pub received: Option<u128>,
    pub mature_at: Option<u64>,
    pub undeleg: u32,
}
#[derive(Clone, Debug, Default)]
pub struct View {
    pub pending: u128,
    pub owed: u128,
    pub delegated: u128,
    pub unclaimed: u128,
    pub accrued_fee: u128,
    pub pool_value: u128,
    pub supply: u128,
    pub rate18: u128,
    pub cur: u64,
    pub batches: BTreeMap<u64, BView>,
    pub claims: BTreeMap<(u64, String), u128>,
}

#[derive(Clone, Debug)]
pub struct Ref {
    fee_bps: u128,
    epoch: u64,
    unbonding: u64,
    unbonding_floor: u64,
    min_dep: u128,
    max_tvl: u128,
    pause: u8,
    weights: Vec<(String, u128)>,
    pending: u128,
    owed: u128,
    cur: u64,
    last_submit: u64,
    batches: BTreeMap<u64, RB>,
    claims: BTreeMap<(u64, String), u128>,
}

const CHAIN_ERRS: [&str; 8] = [
    "liquid staking cap",
    "redelegation in progress",
    "zero shares",
    "zero share token",
    "tiny redemption",
    "too many unbonding entries",
    "invalid shares amount",
    "empty validator",
];

impl Ref {
    pub fn new(weights: Vec<(String, u128)>, fee_bps: u128, epoch: u64, unbonding: u64, min_dep: u128, max_tvl: u128) -> Self {
        let mut batches = BTreeMap::new();
        batches.insert(1, new_rb());
        Ref {
            fee_bps,
            epoch,
            unbonding,
            unbonding_floor: unbonding / 8 * 7,
            min_dep,
            max_tvl,
            pause: 0,
            weights,
            pending: 0,
            owed: 0,
            cur: 1,
            last_submit: 0,
            batches,
            claims: BTreeMap::new(),
        }
    }

    // ---- pricing (architecture.md section 4) ------------------------------------------------------
    /// (pool value, gross unclaimed rewards)
    fn value(&self, e: &Env) -> (u128, u128) {
        let r = e.unclaimed();
        let fee = mulfloor(r, self.fee_bps, BPS);
        (e.delegated() + r - fee, r)
    }
    fn seff(&self, e: &Env) -> u128 {
        e.supply + self.pending
    }
    fn fee_of(&self, gross: u128) -> u128 {
        mulfloor(gross, self.fee_bps, BPS)
    }

    /// Validators whose deficit against the target is the largest after adding `add` to the total.
    fn deposit_targets(&self, e: &Env, add: u128) -> Vec<String> {
        let total = e.delegated() + add;
        let mut best: Option<i128> = None;
        let mut out = vec![];
        for (v, w) in &self.weights {
            let target = (total * w / BPS) as i128;
            let def = target - e.stake_of(v) as i128;
            match best {
                Some(b) if def < b => {}
                Some(b) if def == b => out.push(v.clone()),
                _ => {
                    best = Some(def);
                    out = vec![v.clone()];
                }
            }
        }
        out
    }

    fn grown_validator(pre: &Env, post: &Env) -> Option<String> {
        pre.stake
            .iter()
            .map(|(v, a)| (v.clone(), post.stake_of(v) as i128 - *a as i128))
            .max_by_key(|(_, g)| *g)
            .filter(|(_, g)| *g > 0)
            .map(|(v, _)| v)
    }

    // ---- predictions ------------------------------------------------------------------------------
    /// Ok(()) if the model says the call is accepted, Err(reason) if refused.
    fn predict(&self, c: &Call, e: &Env, o: &Obs) -> Result<(), String> {
        let (v, r) = self.value(e);
        let s = self.seff(e);
        match c {
            Call::Deposit { amount, .. } => {
                if self.pause != 0 {
                    return Err("paused".into());
                }
                if *amount < self.min_dep {
                    return Err("below min".into());
                }
                if v + amount > self.max_tvl {
                    return Err("tvl cap".into());
                }
                if s > 0 {
                    if v == 0 {
                        return Err("empty pool".into());
                    }
                    if mulfloor(*amount, s, v) == 0 {
                        return Err("zero mint".into());
                    }
                }
                // the fee on the touched validator's rewards is a strict send
                let tv = self.deposit_targets(e, *amount);
                if e.treasury_blocked {
                    let refusing: Vec<bool> = tv.iter().map(|t| self.fee_of(e.reward_of(t)) > 0).collect();
                    if refusing.iter().all(|x| *x) {
                        return Err("treasury refuses".into());
                    }
                    if refusing.iter().any(|x| *x) {
                        // tied validators, only some of which have a fee to send: which one the docs pick is open (A1)
                        info("A1 tie decides whether a blocked treasury refuses the deposit", format!("{tv:?}"));
                        if !o.ok {
                            return Err("ambiguous: treasury refuses".into());
                        }
                    }
                }
                Ok(())
            }
            Call::Redeem { .. } => {
                if self.pause == 2 {
                    Err("paused full".into())
                } else {
                    Ok(())
                }
            }
            Call::RedeemToShares { amount, .. } => {
                if self.pause == 2 {
                    return Err("paused full".into());
                }
                let worth = mulfloor(*amount, v, s);
                if worth < self.min_dep {
                    return Err("exit too small".into());
                }
                let d = e.delegated();
                if worth > d {
                    return Err("insufficient delegations".into());
                }
                let floors: Vec<u128> = e.stake.iter().map(|(_, a)| mulfloor(worth, *a, d)).collect();
                let rem = worth - floors.iter().sum::<u128>();
                let maxf = floors.iter().copied().max().unwrap_or(0);
                if maxf < 10 {
                    if maxf + rem < 10 {
                        return Err("all parts dust".into());
                    }
                    // ambiguity A2: the docs do not say who gets the rounding remainder, so whether the
                    // exit is refused as too small is not determined: accept whatever the contract does.
                    info(
                        "A2 exit remainder rule: all floored parts < 10 but the remainder could lift one",
                        format!("worth {worth} stakes {:?}", e.stake),
                    );
                    if !o.ok {
                        return Err("ambiguous: exit too small".into());
                    }
                }
                Ok(())
            }
            Call::DepositShares { validator, .. } => {
                if self.pause != 0 {
                    return Err("paused".into());
                }
                let w = match self.weights.iter().find(|(n, _)| n == validator) {
                    Some((_, w)) => *w,
                    None => return Err("validator not in set".into()),
                };
                let (_, before, after_redeem) = match &o.dep {
                    Some(x) => x.clone(),
                    None => return Err("redemption failed (chain)".into()),
                };
                let dep = after_redeem.saturating_sub(before);
                if dep < self.min_dep {
                    return Err("below min".into());
                }
                if v + dep > self.max_tvl {
                    return Err("tvl cap".into());
                }
                // at most 20% (relative) above its target weight
                let total_after = e.delegated() + dep;
                if after_redeem * BPS * BPS > total_after * w * (BPS + 2000) {
                    return Err("overweight".into());
                }
                if s > 0 {
                    if v == 0 {
                        return Err("empty pool".into());
                    }
                    if mulfloor(dep, s, v) == 0 {
                        return Err("zero mint".into());
                    }
                }
                let rv = e.reward_of(validator);
                if e.treasury_blocked && self.fee_of(rv) > 0 {
                    return Err("treasury refuses".into());
                }
                Ok(())
            }
            Call::Claim { who, id } => match self.batches.get(id) {
                Some(b) if b.status == St::Matured => {
                    if self.claims.get(&(*id, who.clone())).copied().unwrap_or(0) == 0 {
                        Err("no claim".into())
                    } else {
                        Ok(())
                    }
                }
                _ => Err("batch not matured".into()),
            },
            Call::Harvest => {
                if r == 0 {
                    return Err("nothing to harvest".into());
                }
                if e.treasury_blocked && self.fee_of(r) > 0 {
                    return Err("treasury refuses".into());
                }
                Ok(())
            }
            Call::Submit => {
                let b = &self.batches[&self.cur];
                if b.burned == 0 {
                    return Err("nothing to unbond".into());
                }
                if e.now < self.last_submit + self.epoch {
                    return Err("epoch".into());
                }
                let exp = mulfloor(b.burned, v, s);
                if exp > e.delegated() {
                    return Err("insufficient delegations".into());
                }
                Ok(())
            }
            Call::Finalize { id } => self.finalize_plan(*id, e).map(|_| ()),
            Call::SetFee(f) => {
                if *f > 2000 {
                    return Err("fee too high".into());
                }
                if *f != self.fee_bps && r > 0 && e.treasury_blocked && self.fee_of(r) > 0 {
                    return Err("treasury refuses".into());
                }
                Ok(())
            }
            Call::SetEpoch(ep) => self.timing_ok(*ep, self.unbonding),
            Call::SetUnbonding(u) => {
                if *u < self.unbonding_floor {
                    return Err("below floor".into());
                }
                self.timing_ok(self.epoch, *u)
            }
            Call::SetMinDeposit(m) => {
                if *m == 0 {
                    return Err("zero".into());
                }
                if self.max_tvl < *m {
                    return Err("max_tvl < min".into());
                }
                Ok(())
            }
            Call::SetMaxTvl(m) => {
                if *m < self.min_dep {
                    Err("max_tvl < min".into())
                } else {
                    Ok(())
                }
            }
            Call::Pause(_) | Call::UpdateValidators(_) | Call::Other => Ok(()),
        }
    }

    fn timing_ok(&self, epoch: u64, unb: u64) -> Result<(), String> {
        if epoch == 0 || unb == 0 || unb > epoch * 6 {
            return Err("ratio".into());
        }
        if epoch > 30 * DAY || unb > 42 * DAY {
            return Err("too long".into());
        }
        Ok(())
    }

    /// architecture.md section 5, FinalizeBatch and section 8.
    /// Returns (batches settled with what they receive, batches whose shortfall is noted in this call).
    fn finalize_plan(&self, id: u64, e: &Env) -> Result<(Vec<(u64, u128)>, Vec<u64>), String> {
        let req = self.batches.get(&id).ok_or("no such batch")?;
        if req.status != St::Unbonding {
            return Err("wrong status".into());
        }
        if e.now < req.mature_at + 60 {
            return Err("not matured".into());
        }
        let mut set: Vec<(u64, &RB)> = self
            .batches
            .iter()
            .filter(|(_, b)| b.status == St::Unbonding && e.now >= b.mature_at + 60)
            .map(|(i, b)| (*i, b))
            .collect();
        let total: u128 = set.iter().map(|(_, b)| b.expected.unwrap()).sum();
        let avail = e.balance.saturating_sub(self.owed);
        let tol = 10 * set.iter().map(|(_, b)| b.undeleg as u128).sum::<u128>();
        let mut noted = vec![];
        if total.saturating_sub(avail) > tol {
            let past = |b: &RB| e.now >= b.mature_at + 7 * DAY;
            if !past(req) {
                return Err("not received yet".into());
            }
            set.retain(|(_, b)| past(b));
            for (i, b) in &set {
                if b.noted.is_none() {
                    noted.push(*i);
                }
            }
            set.retain(|(_, b)| b.noted.is_some_and(|h| h < e.height));
            if !set.iter().any(|(i, _)| *i == id) {
                return Ok((vec![], noted));
            }
        }
        let tot: u128 = set.iter().map(|(_, b)| b.expected.unwrap()).sum();
        let out = set
            .iter()
            .map(|(i, b)| {
                let x = b.expected.unwrap();
                (*i, if avail >= tot { x } else { mulfloor(x, avail, tot) })
            })
            .collect();
        Ok((out, noted))
    }

    // ---- one transaction ----------------------------------------------------------------------------
    pub fn step(&mut self, c: &Call, pre: &Env, post: &Env, o: &Obs, view: &View) -> Vec<String> {
        let mut d: Vec<String> = vec![];
        let chain_err = !o.ok && CHAIN_ERRS.iter().any(|k| o.err.contains(k));
        let pred = self.predict(c, pre, o);
        let pred_ok = pred.is_ok();
        let unmodelled = matches!(c, Call::Other);
        if !unmodelled && o.ok != pred_ok && !(chain_err && pred_ok) {
            d.push(format!(
                "ACCEPT/REFUSE: {c:?}: contract ok={} err={:?}; model ok={} reason={:?}",
                o.ok,
                o.err,
                pred_ok,
                pred.as_ref().err()
            ));
        }
        if o.ok {
            self.apply_ok(c, pre, post, o, &mut d);
        }
        d.extend(self.compare_view(post, view));
        d
    }

    #[allow(clippy::too_many_arguments)]
    fn rate_check(&self, name: &str, pre: &Env, post: &Env, s_pre: u128, s_post: u128, d: &mut Vec<String>) {
        // pool value before / after under the model's own (already updated) pending numbers
        let mut before = self.clone();
        before.pending = s_pre - pre.supply;
        let (v0, _) = before.value(pre);
        let (v1, _) = self.value(post);
        // need = ceil(v0 * s_post / s_pre): value the pool must still have so the rate does not fall
        let need = (v0 * s_post + s_pre - 1) / s_pre;
        let deficit = need as i128 - v1 as i128;
        hist(&format!("rate deficit (uatom) after {name}"), deficit.clamp(-5, 5));
        // without any slash the chain converts shares and tokens exactly, so no rate fall at all is allowed
        let limit = if pre.slashed || post.slashed { 3 } else { 0 };
        if deficit > limit {
            d.push(format!(
                "RATE FELL in {name}: pool {v0} -> {v1}, supply_eff {s_pre} -> {s_post}, needed {need} (deficit {deficit})"
            ));
        }
    }

    fn apply_ok(&mut self, c: &Call, pre: &Env, post: &Env, o: &Obs, d: &mut Vec<String>) {
        let (v, r) = self.value(pre);
        let s = self.seff(pre);
        let tdelta = post.treasury as i128 - pre.treasury as i128;
        match c {
            Call::Deposit { amount, .. } => {
                let mint = if s == 0 { *amount } else { mulfloor(*amount, s, v) };
                let got = post.supply as i128 - pre.supply as i128;
                if got != mint as i128 {
                    d.push(format!("MINT: deposit {amount} at pool {v} supply_eff {s}: model mints {mint}, contract {got}"));
                }
                let tv = Self::grown_validator(pre, post);
                let targets = self.deposit_targets(pre, *amount);
                match &tv {
                    Some(t) => {
                        if !targets.contains(t) {
                            d.push(format!(
                                "TARGET: deposit went to {t}, model largest-deficit set {targets:?}, stakes {:?}",
                                pre.stake
                            ));
                        } else if targets.len() > 1 {
                            info(
                                "A1 tie between validators with equal largest deficit",
                                format!("tie {targets:?} chosen {t}"),
                            );
                        }
                        let fee = self.fee_of(pre.reward_of(t));
                        if !pre.skip_treasury && tdelta != fee as i128 {
                            d.push(format!("FEE: deposit touched {t}: model fee {fee}, treasury got {tdelta}"));
                        }
                    }
                    None => d.push("no validator grew after a deposit".into()),
                }
                self.rate_check("Deposit", pre, post, s, s + mint, d);
                let (v1, _) = self.value(post);
                hist("pool value change minus deposit amount, Deposit", (v1 as i128 - v as i128 - *amount as i128).clamp(-5, 5));
                if !pre.slashed && !post.slashed && (v1 as i128) < v as i128 + *amount as i128 {
                    d.push(format!("VALUE LOST in deposit without slash: pool {v} + {amount} -> {v1}"));
                }
            }
            Call::Redeem { who, amount } => {
                self.pending += amount;
                self.batches.get_mut(&self.cur).unwrap().burned += amount;
                *self.claims.entry((self.cur, who.clone())).or_default() += amount;
                if pre.supply - post.supply != *amount {
                    d.push(format!("Redeem burned {} not {}", pre.supply - post.supply, amount));
                }
            }
            Call::RedeemToShares { amount, .. } => {
                let worth = mulfloor(*amount, v, s);
                if pre.supply - post.supply != *amount {
                    d.push(format!("RedeemToShares burned {} not {}", pre.supply - post.supply, amount));
                }
                let dd = pre.delegated();
                let sum: u128 = o.tokenized.iter().map(|(_, a)| *a).sum();
                if sum != worth {
                    d.push(format!(
                        "EXIT SUM: worth {worth} (floor({amount}*{v}/{s})) but tokenized {sum}: {:?}",
                        o.tokenized
                    ));
                }
                let n = pre.stake.iter().filter(|(_, a)| *a > 0).count() as u128;
                let floors: Vec<(String, u128)> =
                    pre.stake.iter().map(|(n, a)| (n.clone(), mulfloor(worth, *a, dd))).collect();
                let rem = worth - floors.iter().map(|(_, f)| *f).sum::<u128>();
                for (name, f) in &floors {
                    let got: u128 = o.tokenized.iter().filter(|(vn, _)| vn == name).map(|(_, a)| *a).sum();
                    if got == 0 {
                        if *f >= 10 {
                            d.push(format!("EXIT PRORATA: {name} gave 0, pro rata floor {f}"));
                        }
                    } else {
                        if got < 10 {
                            d.push(format!("EXIT DUST: part {got} < 10 for {name} was not folded"));
                        }
                        let hi = f + rem + 9 * n;
                        if got < *f || got > hi {
                            d.push(format!(
                                "EXIT PRORATA: {name} gave {got}, floor {f}, remainder {rem}, folded allowance {}",
                                9 * n
                            ));
                        }
                    }
                }
                let touched: Vec<&String> = o.tokenized.iter().map(|(v, _)| v).collect();
                let paid: u128 = pre.rewards.iter().filter(|(n, _)| touched.contains(&n)).map(|(_, a)| *a).sum();
                let fee = self.fee_of(paid);
                let want = if pre.treasury_blocked { 0 } else { fee as i128 };
                if !pre.skip_treasury && tdelta != want {
                    d.push(format!("FEE: exit touched {touched:?}: model fee {want}, treasury got {tdelta}"));
                }
                self.rate_check("RedeemToShares", pre, post, s, s - amount, d);
            }
            Call::DepositShares { validator, .. } => {
                let (_, before, after) = o.dep.clone().unwrap();
                let dep = after - before;
                let mint = if s == 0 { dep } else { mulfloor(dep, s, v) };
                let got = post.supply as i128 - pre.supply as i128;
                if got != mint as i128 {
                    d.push(format!(
                        "MINT: share deposit of {dep} (validator {validator}) at pool {v} supply_eff {s}: model mints {mint}, contract {got}"
                    ));
                }
                let fee = self.fee_of(pre.reward_of(validator));
                if !pre.skip_treasury && tdelta != fee as i128 {
                    d.push(format!("FEE: share deposit at {validator}: model fee {fee}, treasury got {tdelta}"));
                }
                self.rate_check("DepositShares", pre, post, s, s + mint, d);
                let (v1, _) = self.value(post);
                hist("pool value change minus measured growth, DepositShares", (v1 as i128 - v as i128 - dep as i128).clamp(-5, 5));
            }
            Call::Claim { who, id } => {
                let b = &self.batches[id];
                let liatom = self.claims.remove(&(*id, who.clone())).unwrap();
                let payout = mulfloor(b.received.unwrap(), liatom, b.burned);
                self.owed -= payout;
                if o.sender_delta != payout as i128 {
                    d.push(format!(
                        "CLAIM: batch {id} received {:?} burned {} share {liatom}: model pays {payout}, contract {}",
                        b.received, b.burned, o.sender_delta
                    ));
                }
            }
            Call::Harvest => {
                let fee = self.fee_of(r);
                if !pre.skip_treasury && tdelta != fee as i128 {
                    d.push(format!("FEE: harvest of {r}: model fee {fee}, treasury got {tdelta}"));
                }
                let net = r - fee;
                let grew = post.delegated() as i128 - pre.delegated() as i128;
                hist("delegation growth minus model net, Harvest", (grew - net as i128).clamp(-5, 5));
                let tol = if pre.slashed || post.slashed { 3 } else { 0 };
                if (grew - net as i128).abs() > tol {
                    d.push(format!("HARVEST reinvested {grew}, model net {net}"));
                }
                self.rate_check("Harvest", pre, post, s, s, d);
            }
            Call::Submit => {
                let burned = self.batches[&self.cur].burned;
                let exp = mulfloor(burned, v, s);
                let b = self.batches.get_mut(&self.cur).unwrap();
                b.expected = Some(exp);
                b.status = St::Unbonding;
                b.mature_at = pre.now + self.unbonding;
                b.undeleg = o.new_unbonding.len() as u32;
                let got: u128 = o.new_unbonding.iter().map(|(_, a)| *a).sum();
                let n = o.new_unbonding.len() as u128;
                hist("unbonded tokens minus expected, Submit", (got as i128 - exp as i128).clamp(-5, 5));
                if (got as i128 - exp as i128).unsigned_abs() > 2 * n.max(1) {
                    d.push(format!("SUBMIT: undelegated {got} but batch expects {exp} ({:?})", o.new_unbonding));
                }
                let dd = pre.delegated();
                for (name, a) in &o.new_unbonding {
                    let fair = mulfloor(exp, pre.stake_of(name), dd);
                    if (*a as i128 - fair as i128).unsigned_abs() > n + 2 {
                        d.push(format!("SUBMIT PRORATA: {name} undelegated {a}, pro rata {fair}"));
                    }
                }
                let touched: Vec<&String> = o.new_unbonding.iter().map(|(v, _)| v).collect();
                let paid: u128 = pre.rewards.iter().filter(|(n, _)| touched.contains(&n)).map(|(_, a)| *a).sum();
                let want = if pre.treasury_blocked { 0 } else { self.fee_of(paid) as i128 };
                if !pre.skip_treasury && tdelta != want {
                    d.push(format!("FEE: submit touched {touched:?}: model fee {want}, treasury got {tdelta}"));
                }
                self.pending -= burned;
                self.last_submit = pre.now;
                self.cur += 1;
                self.batches.insert(self.cur, new_rb());
                self.rate_check("Submit", pre, post, s, s - burned, d);
            }
            Call::Finalize { id } => {
                let (settled, noted) = self.finalize_plan(*id, pre).unwrap_or_default();
                for n in noted {
                    self.batches.get_mut(&n).unwrap().noted = Some(pre.height);
                }
                for (i, rec) in settled {
                    let b = self.batches.get_mut(&i).unwrap();
                    b.status = St::Matured;
                    b.received = Some(rec);
                    self.owed += rec;
                }
            }
            Call::SetFee(f) => {
                if *f != self.fee_bps {
                    let fee = self.fee_of(r);
                    if !pre.skip_treasury && tdelta != fee as i128 {
                        d.push(format!("FEE: fee change harvests at old fee: model fee {fee}, treasury got {tdelta}"));
                    }
                }
                self.fee_bps = *f;
            }
            Call::SetEpoch(e) => self.epoch = *e,
            Call::SetUnbonding(u) => self.unbonding = *u,
            Call::SetMinDeposit(m) => self.min_dep = *m,
            Call::SetMaxTvl(m) => self.max_tvl = *m,
            Call::Pause(m) => self.pause = *m,
            Call::UpdateValidators(w) => self.weights = w.clone(),
            Call::Other => {}
        }
    }

    /// The contract's queries against the model's own state.
    pub fn compare_view(&self, e: &Env, v: &View) -> Vec<String> {
        let mut d = vec![];
        let (val, r) = self.value(e);
        let s = self.seff(e);
        let mut chk = |name: &str, model: u128, contract: u128| {
            if model != contract {
                d.push(format!("VIEW {name}: model {model}, contract {contract}"));
            }
        };
        chk("pending_liatom", self.pending, v.pending);
        chk("owed_to_redeemers", self.owed, v.owed);
        chk("delegated", e.delegated(), v.delegated);
        chk("unclaimed_rewards", r, v.unclaimed);
        chk("accrued_fee", self.fee_of(r), v.accrued_fee);
        chk("pool_value", val, v.pool_value);
        chk("liatom_supply", e.supply, v.supply);
        let rate18 = if s == 0 { 1_000_000_000_000_000_000 } else { mulfloor(val, 1_000_000_000_000_000_000, s) };
        chk("rate (18 decimals)", rate18, v.rate18);
        chk("current batch", self.cur as u128, v.cur as u128);
        for (id, b) in &self.batches {
            let Some(cb) = v.batches.get(id) else {
                d.push(format!("VIEW batch {id} missing"));
                continue;
            };
            let st = match b.status {
                St::Open => 0,
                St::Unbonding => 1,
                St::Matured => 2,
            };
            let same = cb.burned == b.burned
                && cb.status == st
                && cb.expected == b.expected
                && cb.received == b.received
                && (b.status == St::Open || cb.mature_at == Some(b.mature_at))
                && cb.undeleg == b.undeleg;
            if !same {
                d.push(format!("VIEW batch {id}: model {b:?}, contract {cb:?}"));
            }
        }
        let users: std::collections::BTreeSet<&String> =
            self.claims.keys().map(|(_, u)| u).chain(v.claims.keys().map(|(_, u)| u)).collect();
        for id in self.batches.keys() {
            for u in &users {
                let m = self.claims.get(&(*id, (*u).clone())).copied().unwrap_or(0);
                let k = v.claims.get(&(*id, (*u).clone())).copied().unwrap_or(0);
                if m != k {
                    d.push(format!("VIEW claim of {u} in batch {id}: model {m}, contract {k}"));
                }
            }
        }
        d
    }
}
