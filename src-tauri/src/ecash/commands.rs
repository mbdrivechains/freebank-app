//! The eCash tab's commands. The wallets in the eCash node are watch-only and FreeBank signs (v0.2.6 security review
//! H1, H2): the main account's key comes from the words, which the wallet passphrase opens for each payment; the
//! bidding account's from its owner-only key file. Neither the passphrase nor a private key goes to the node.

use super::conn::{self, Conn};
use super::keys::{Account, AccountKey, Root};
use super::to_coins;
use super::wallet::{self, Balance, Record, Tx};
use crate::node::NodeManager;
use crate::phone::commands::PhoneState;
use crate::phone::Approve;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::State;
use zeroize::Zeroizing;

/// A prepared payment is good for this long (the fee may change after).
const QUOTE_LIFE: Duration = Duration::from_secs(300);
const EXPIRED: &str = "That payment was prepared too long ago. Prepare it again.";
const NOT_READY: &str = "Set up your eCash wallet first.";

struct Pending {
    quote: wallet::Quote,
    to_bids: bool,
    /// From the bidding wallet back to the main one (signed with the key file; confirmed with no passphrase).
    from_bids: bool,
    at: Instant,
}

/// Keep a prepared payment for its confirm, and give its id.
fn book(q: wallet::Quote, to_bids: bool, from_bids: bool) -> String {
    let id = format!("{:016x}", rand::random::<u64>());
    let mut book = BOOK.lock().unwrap();
    book.retain(|_, p| p.at.elapsed() < QUOTE_LIFE);
    book.insert(id.clone(), Pending { quote: q, to_bids, from_bids, at: Instant::now() });
    id
}

/// Take a prepared payment of the kind asked for (from the bidding wallet or the main one), if still good.
fn take(id: &str, from_bids: bool) -> Result<Pending, String> {
    let mut book = BOOK.lock().unwrap();
    match book.get(id) {
        Some(p) if p.from_bids == from_bids && p.at.elapsed() < QUOTE_LIFE => Ok(book.remove(id).unwrap()),
        _ => Err(EXPIRED.into()),
    }
}

static BOOK: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Default::default);

/// A payment that may have gone out (the node didn't say it took it), by wallet name: its txid, its coins, when. The
/// next payment from that wallet must spend one of its coins until it shows, or for half an hour (re-review M-B).
type Uncertain = (String, Vec<bitcoin::OutPoint>, Instant);
static UNCERTAIN: LazyLock<Mutex<HashMap<String, Uncertain>>> = LazyLock::new(Default::default);
const UNCERTAIN_LIFE: Duration = Duration::from_secs(1800);

/// The coins the next payment from `wallet_name` must spend one of, if an earlier one may have gone out and doesn't
/// show yet.
async fn must_spend(c: &Conn, wallet_name: &str) -> Option<Vec<bitcoin::OutPoint>> {
    let (txid, inputs, at) = UNCERTAIN.lock().unwrap().get(wallet_name).cloned()?;
    if at.elapsed() >= UNCERTAIN_LIFE {
        UNCERTAIN.lock().unwrap().remove(wallet_name);
        return None;
    }
    // It shows: in the wallet, confirmed or in the node's mempool. Then its coins are spent, and nothing can double.
    let shows = match c.wallet(wallet_name).call_typed("gettransaction", vec![serde_json::json!(txid)]).await {
        Ok(t) => t["confirmations"].as_i64().unwrap_or(0) > 0
            || c.node().call_typed("getmempoolentry", vec![serde_json::json!(txid)]).await.is_ok(),
        Err(_) => false,
    };
    if shows {
        UNCERTAIN.lock().unwrap().remove(wallet_name);
        return None;
    }
    Some(inputs)
}

/// After a payment: forget the uncertain one if this one spent one of its coins; remember this one if it may have gone.
fn after_send(wallet_name: &str, r: &Result<String, wallet::SendFail>, must: &Option<Vec<bitcoin::OutPoint>>) {
    let mut u = UNCERTAIN.lock().unwrap();
    match r {
        Ok(_) if must.is_some() => {
            u.remove(wallet_name);
        }
        Err(f) if f.sent => {
            u.insert(wallet_name.to_string(), (f.txid.clone().unwrap_or_default(), f.inputs.clone(), Instant::now()));
        }
        _ => {}
    }
}

#[derive(Debug, Serialize)]
pub struct Status {
    /// Why the eCash wallet can't be used now, in plain words (None: it can).
    pub problem: Option<String>,
    /// The eCash node took BitWindow's default login, which any program on this computer can use.
    pub default_login: bool,
    /// "none" (not set up for the words this app has), "ready", "missing" (set up, but the eCash node lacks the
    /// wallets: another node, or they were removed).
    pub state: &'static str,
    /// FreeBank has the recovery words saved (needed to set up).
    pub has_words: bool,
    /// Wallets of other words stay in the eCash node (watch-only, nothing secret): said on screen.
    pub earlier: bool,
    pub main: Option<Balance>,
    pub bids: Option<Balance>,
}

async fn settings(mgr: &NodeManager) -> crate::node::Settings {
    mgr.settings.lock().await.clone()
}

pub(crate) async fn connect(mgr: &NodeManager) -> Result<Conn, String> {
    conn::connect(&mgr.http, &settings(mgr).await, Some(&mgr.app_dir)).await
}

/// The record, if it is for the words this app has and the network the node is on.
fn record_for(mgr: &NodeManager, c: &Conn) -> Option<Record> {
    let r = wallet::read_record(&mgr.app_dir)?;
    (Some(&r.key_id) == crate::seed::sealed_key_id_hex(&mgr.app_dir).as_ref() && r.chain == wallet::chain_name(c.chain))
        .then_some(r)
}

/// The wallets, if set up for the words this app has and present in the node.
pub(crate) async fn ready(mgr: &NodeManager) -> Result<(Conn, Record), String> {
    let c = connect(mgr).await?;
    let r = record_for(mgr, &c).ok_or(NOT_READY)?;
    for a in [Account::Main, Account::Bids] {
        if !wallet::open(&c, r.name(a)).await? {
            return Err(NOT_READY.into());
        }
    }
    Ok((c, r))
}

pub(crate) async fn status_of(mgr: &NodeManager) -> Status {
    let has_words = crate::seed::seed_path(&mgr.app_dir).is_file();
    let mut st = Status { problem: None, default_login: false, state: "none", has_words, earlier: false, main: None, bids: None };
    let c = match connect(mgr).await {
        Ok(c) => c,
        Err(e) => {
            st.problem = Some(e);
            return st;
        }
    };
    st.default_login = c.default_login;
    let Some(r) = record_for(mgr, &c) else {
        st.earlier = wallet::read_record(&mgr.app_dir).is_some();
        return st;
    };
    let present = async {
        let mut ok = true;
        for a in [Account::Main, Account::Bids] {
            ok = ok && wallet::open(&c, r.name(a)).await? && wallet::holds(&c, r.name(a), &r.public(a)?).await?;
        }
        Ok::<_, String>(ok)
    };
    match present.await {
        Ok(true) => {}
        Ok(false) => {
            st.state = "missing";
            return st;
        }
        Err(e) => {
            st.problem = Some(e);
            return st;
        }
    }
    st.state = "ready";
    match (wallet::balance(&c, &r.main_name).await, wallet::balance(&c, &r.bids_name).await) {
        (Ok(m), Ok(b)) => {
            st.main = Some(m);
            st.bids = Some(b);
        }
        (Err(e), _) | (_, Err(e)) => st.problem = Some(e),
    }
    st
}

#[tauri::command]
pub async fn ecash_status(mgr: State<'_, Arc<NodeManager>>) -> Result<Status, String> {
    Ok(status_of(&mgr).await)
}

/// The eCash root from the saved words (the passphrase opens them).
async fn root_from_words(mgr: &NodeManager, pass: &Zeroizing<String>, chain: crate::seed::Chain) -> Result<(Root, String), String> {
    let (entropy, key_id) = crate::recovery::ops::open_saved_blocking(&mgr.app_dir, pass).await?;
    Ok((Root::from_words(&entropy, chain)?, crate::seed::key_id_hex(&key_id)))
}

/// Make the two watch-only eCash wallets from the saved recovery words, and the bidding key file. The passphrase opens
/// the words here; the node gets only the accounts' public keys.
#[tauri::command]
pub async fn ecash_setup(mgr: State<'_, Arc<NodeManager>>, passphrase: String) -> Result<Status, String> {
    let pass = Zeroizing::new(passphrase);
    let c = connect(&mgr).await?;
    let (root, key_id) = root_from_words(&mgr, &pass, c.chain).await?;
    let fp = root.fingerprint().to_string();
    let (main_name, bids_name) = wallet::names(&fp);
    let main = root.account(c.chain, Account::Main)?;
    let bids = root.account(c.chain, Account::Bids)?;
    drop(root);
    wallet::create(&c, &main_name, &main.public).await?;
    wallet::create(&c, &bids_name, &bids.public).await?;
    wallet::write_bids_key(&mgr.app_dir, &bids)?;
    let r = Record {
        key_id,
        chain: wallet::chain_name(c.chain).into(),
        fingerprint: fp,
        main_name,
        bids_name,
        main_xpub: main.public.to_record().0,
        bids_xpub: bids.public.to_record().0,
    };
    wallet::write_record(&mgr.app_dir, &r)?;
    crate::activity::note("ecash: wallets set up");
    Ok(status_of(&mgr).await)
}

#[tauri::command]
pub async fn ecash_receive(mgr: State<'_, Arc<NodeManager>>) -> Result<String, String> {
    let (c, r) = ready(&mgr).await?;
    wallet::new_address(&c, &r.main_name, &r.public(Account::Main)?).await
}

#[tauri::command]
pub async fn ecash_history(mgr: State<'_, Arc<NodeManager>>) -> Result<Vec<Tx>, String> {
    let (c, r) = ready(&mgr).await?;
    wallet::history(&c, &r).await
}

#[derive(Debug, Serialize)]
pub struct QuoteView {
    pub id: String,
    pub address: String,
    pub to_bids: bool,
    /// What arrives, sats.
    pub sats: u64,
    pub fee: u64,
    /// What leaves the wallet: sats + fee.
    pub total: u64,
}

fn parse_amount(amount: &Option<String>) -> Result<Option<u64>, String> {
    match amount.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(a) => crate::phone::store::decimal_to_sats(a)
            .map(Some)
            .map_err(|_| "Enter the amount as a number of eCash, with at most 8 decimals.".into()),
    }
}

/// Prepare a payment from the main eCash wallet: to `address`, or into the bidding wallet when `to_bids` (an address
/// of the bidding wallet's, checked against its key). No amount: everything it can spend, less the fee.
#[tauri::command]
pub async fn ecash_send_prepare(
    mgr: State<'_, Arc<NodeManager>>,
    address: String,
    amount: Option<String>,
    to_bids: bool,
) -> Result<QuoteView, String> {
    let sats = parse_amount(&amount)?;
    let (c, r) = ready(&mgr).await?;
    let address = if to_bids {
        wallet::new_address(&c, &r.bids_name, &r.public(Account::Bids)?).await?
    } else {
        address.trim().to_string()
    };
    let q = wallet::quote(&c, &r.main_name, &address, sats).await?;
    // The PSBT is checked now too, so a bad one is said before the passphrase is asked.
    let to = c.script_of(&q.address)?;
    super::sign::check(&q.psbt, &r.public(Account::Main)?, &q.expect(to))?;
    let (sats, fee) = (q.sats, q.fee);
    let id = book(q, to_bids, false);
    Ok(QuoteView { id, address, to_bids, sats, fee, total: sats + fee })
}

/// What a phone is asked to approve for a payment from the main eCash wallet.
pub(crate) fn approve_text(to_bids: bool, sats: u64, address: &str) -> String {
    if to_bids {
        format!("Move {} eCash into the bidding wallet ({})", to_coins(sats), address)
    } else {
        format!("Send {} eCash to {}", to_coins(sats), address)
    }
}

/// Sign and send a prepared payment. The passphrase opens the words here (a wrong one leaves the payment prepared),
/// then "Approve sends on my phone" counts it or asks a phone, then FreeBank checks the node's payment and signs it.
#[tauri::command]
pub async fn ecash_send_confirm(
    mgr: State<'_, Arc<NodeManager>>,
    phone: State<'_, PhoneState>,
    id: String,
    passphrase: String,
) -> Result<String, String> {
    let pass = Zeroizing::new(passphrase);
    {
        let book = BOOK.lock().unwrap();
        let p = book.get(&id).ok_or(EXPIRED)?;
        if p.from_bids || p.at.elapsed() >= QUOTE_LIFE {
            return Err(EXPIRED.into());
        }
    }
    let (c, r) = ready(&mgr).await?;
    let (root, _) = root_from_words(&mgr, &pass, c.chain).await?;
    let key = root.account(c.chain, Account::Main)?;
    // A move into the bidding wallet went to an address checked against ecash.json's bidding xpub: that must be the
    // words' too (re-review L-F), or a swapped file could send it elsewhere.
    let bids_ok = root.account(c.chain, Account::Bids)?.public == r.public(Account::Bids)?;
    drop(root);
    if key.public != r.public(Account::Main)? {
        return Err("These words don't give this eCash wallet's key.".into());
    }
    // Only now, with the passphrase right: the prepared payment is used once.
    let p = take(&id, false)?;
    if p.to_bids && !bids_ok {
        return Err("The bidding wallet on record isn't the one these words give. Nothing was sent; set up eCash again.".into());
    }
    let total = p.quote.sats + p.quote.fee;
    let guard = phone.guard(&mgr.app_dir)?.filter(|ph| ph.approve_over().is_some());
    let cleared = match guard {
        Some(ph) => Some(
            ph.clear_desktop(total, Approve::Action { text: approve_text(p.to_bids, p.quote.sats, &p.quote.address), sats: Some(total) })
                .await?,
        ),
        None => None,
    };
    let must = must_spend(&c, &r.main_name).await;
    let res = wallet::sign_and_send(&c, &p.quote, &key, must.as_deref()).await;
    after_send(&r.main_name, &res, &must);
    match res {
        Ok(txid) => Ok(txid),
        Err(f) => {
            // Given back only if nothing left FreeBank: a payment handed to the node stays counted (re-review M-B).
            if !f.sent {
                if let (Some(cl), Some(ph)) = (cleared, guard) {
                    ph.uncount(cl);
                }
            }
            crate::activity::note(&format!("ecash: not sent: {}", crate::activity::mask_numbers(&f.message)));
            Err(f.message)
        }
    }
}

/// Prepare moving eCash from the bidding wallet back into the main one: `amount`, or all of it. The money goes to an
/// address of the main wallet's, checked; the fee is shown before anything goes (re-review L-C).
#[tauri::command]
pub async fn ecash_bids_withdraw_prepare(mgr: State<'_, Arc<NodeManager>>, amount: Option<String>) -> Result<QuoteView, String> {
    let sats = parse_amount(&amount)?;
    let (c, r) = ready(&mgr).await?;
    let to = wallet::new_address(&c, &r.main_name, &r.public(Account::Main)?).await?;
    let q = wallet::quote(&c, &r.bids_name, &to, sats).await?;
    super::sign::check(&q.psbt, &r.public(Account::Bids)?, &q.expect(c.script_of(&q.address)?))?;
    let (sats, fee) = (q.sats, q.fee);
    let id = book(q, false, true);
    Ok(QuoteView { id, address: to, to_bids: false, sats, fee, total: sats + fee })
}

/// Move it: signed with the bidding key file, so nothing is asked.
#[tauri::command]
pub async fn ecash_bids_withdraw_confirm(mgr: State<'_, Arc<NodeManager>>, id: String) -> Result<String, String> {
    let (c, r) = ready(&mgr).await?;
    let key: AccountKey = wallet::read_bids_key(&mgr.app_dir, &r)?;
    let q = take(&id, true)?.quote;
    let must = must_spend(&c, &r.bids_name).await;
    let res = wallet::sign_and_send(&c, &q, &key, must.as_deref()).await;
    after_send(&r.bids_name, &res, &must);
    res.map_err(|f| f.message)
}

// ---- Bidding for FreeBank blocks ----------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RoundView {
    pub height: u64,
    pub outcome: String,
    pub fee: u64,
    pub at: u64,
    pub txid: String,
}

#[derive(Debug, Serialize)]
pub struct BmmView {
    pub on: bool,
    pub bid: u64,
    pub daily_cap: u64,
    /// Bids won or live in the last 24 hours, sats.
    pub spent_today: u64,
    pub won_today: usize,
    /// What the loop said last (unix time, words).
    pub last: Option<(u64, String)>,
    /// The newest rounds first.
    pub rounds: Vec<RoundView>,
}

fn bmm_view(b: &super::bmm::Bmm) -> BmmView {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    BmmView {
        on: b.on,
        bid: b.bid,
        daily_cap: b.daily_cap,
        spent_today: b.spent_today(now),
        won_today: b.rounds.iter().filter(|r| r.outcome == "won" && r.at + 86_400 > now).count(),
        last: super::bmm::RUNNER.last(),
        rounds: b
            .rounds
            .iter()
            .rev()
            .take(20)
            .map(|r| RoundView { height: r.height, outcome: r.outcome.clone(), fee: r.fee, at: r.at, txid: r.txid.clone() })
            .collect(),
    }
}

#[tauri::command]
pub async fn bmm_status(mgr: State<'_, Arc<NodeManager>>) -> Result<BmmView, String> {
    Ok(bmm_view(&super::bmm::RUNNER.get(&mgr.app_dir).await))
}

/// Bidding on or off, the bid and the daily cap (eCash amounts as typed). Turning it on needs the eCash wallets.
#[tauri::command]
pub async fn bmm_set(mgr: State<'_, Arc<NodeManager>>, on: bool, bid: String, daily_cap: String) -> Result<BmmView, String> {
    let bid = crate::phone::store::decimal_to_sats(bid.trim()).map_err(|_| "Enter the bid in eCash, with at most 8 decimals.")?;
    let cap =
        crate::phone::store::decimal_to_sats(daily_cap.trim()).map_err(|_| "Enter the daily cap in eCash, with at most 8 decimals.")?;
    if bid == 0 {
        return Err("The bid must be above zero.".into());
    }
    if cap < bid {
        return Err("The daily cap must be at least one bid.".into());
    }
    if on {
        // Bids are signed with the bidding key file: it must be there.
        let (_, r) = ready(&mgr).await?;
        wallet::read_bids_key(&mgr.app_dir, &r)?;
    }
    let b = super::bmm::RUNNER.set(&mgr.app_dir, on, bid, cap).await?;
    crate::activity::note(if on { "bmm: bidding on" } else { "bmm: bidding off" });
    Ok(bmm_view(&b))
}

// ---- The eCash node's login (Settings › Node & connection) ------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct EcashLogin {
    /// The RPC address in use, host:port.
    pub rpc: String,
    /// What Settings holds (empty: the defaults).
    pub l1_rpc: String,
    pub l1_datadir: String,
    pub user: String,
    pub has_password: bool,
}

#[tauri::command]
pub async fn ecash_login_get(mgr: State<'_, Arc<NodeManager>>) -> Result<EcashLogin, String> {
    let s = settings(&mgr).await;
    Ok(EcashLogin {
        rpc: conn::rpc_endpoint(&s).unwrap_or_else(|e| e),
        l1_rpc: s.l1_rpc.clone().unwrap_or_default(),
        l1_datadir: s.l1_datadir.clone().unwrap_or_default(),
        user: s.l1_user.clone().unwrap_or_default(),
        has_password: conn::password_path(&mgr.app_dir).is_file(),
    })
}

/// Save the eCash node's address, data folder and login (each empty: the default), then say whether it answers.
/// `password` None keeps the one saved; Some("") forgets it.
#[tauri::command]
pub async fn ecash_login_set(
    mgr: State<'_, Arc<NodeManager>>,
    rpc: String,
    datadir: String,
    user: String,
    password: Option<String>,
) -> Result<Status, String> {
    let opt = |v: String| Some(v.trim().to_string()).filter(|x| !x.is_empty());
    let mut s = settings(&mgr).await;
    s.l1_rpc = opt(rpc);
    s.l1_datadir = opt(datadir);
    s.l1_user = opt(user);
    mgr.save_settings(s).await?;
    if let Some(p) = password {
        let p = Zeroizing::new(p);
        conn::save_password(&mgr.app_dir, Some(p.as_str()))?;
    }
    Ok(status_of(&mgr).await)
}
