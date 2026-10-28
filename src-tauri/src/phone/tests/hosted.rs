//! Hosted wallets (v0.2.8, `phone/hosted.rs`; PROTOCOL.md, "Hosted wallets"): an owner's phone invites, the invited
//! phone pairs and is allowed by the inviting phone's Face ID, the desktop makes its wallet with fresh words, the
//! phone shows them, the wallet joins the members-only house and pays; then it moves home and the copy here goes.
//! Against the mocked node (its `call_in` keeps the hosted wallets) and a mock maker.

use super::*;
use crate::phone::hosted::{self as real, Maker, Step};

/// House 5's partner key: the mock node says its address is this wallet's.
pub(super) fn tests_partner_pubkey() -> String {
    use bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
    let sk = SecretKey::from_slice(&[9u8; 32]).unwrap();
    hex::encode(PublicKey::from_secret_key(&Secp256k1::new(), &sk).serialize())
}

/// Makes the wallet in the mock node: encrypted, no seed yet.
struct MockMaker(Arc<MockRpc>);

impl Maker for MockMaker {
    fn create_encrypted<'a>(&'a self, name: &'a str, pass: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let mut all = self.0.hosted.lock().unwrap();
            let w = all.entry(name.to_string()).or_default();
            if w.pass.is_none() {
                w.pass = Some(pass.to_string());
            }
            Ok(())
        })
    }
}

impl Sim {
    /// A pair request from an invite: the plaintext says `hosted`.
    fn pair_frame_hosted(&self, d_pub: &PublicKey, c: &[u8], name: &str, hosted: bool) -> (Value, String) {
        let e = crypto::random_secret();
        let k = crypto::pair_key(&e, d_pub, c);
        let n = [4u8; 12];
        let pt = json!({"p": self.p_pub(), "name": name, "hosted": hosted}).to_string();
        let frame = json!({"t": "pair", "e": crypto::pub_b64u(&e.public_key()), "n": crypto::b64u(&n),
               "ct": crypto::b64u(&crypto::seal(&k, &n, pt.as_bytes()))});
        (frame, crypto::pair_code(d_pub, &self.p.public_key(), &e.public_key(), c))
    }
}

/// Send a request on `ch` and collect frames until its reply: (the reply, the other frames, opened when they are
/// `ch`'s sealed messages).
async fn ask_all(h: &mut H, sim: &mut Sim, ch: u64, id: u64, m: &str, a: Value) -> (Value, Vec<Value>) {
    let f = sim.req(id, m, a);
    h.feed(ch, f);
    let mut others = Vec::new();
    loop {
        let fr = h.next().await;
        if fr["ch"] == ch && fr["d"]["t"] == "m" {
            let v = sim.open(&fr["d"]);
            if v["id"] == id {
                return (v, others);
            }
            others.push(v);
        } else {
            others.push(fr);
        }
    }
}

/// Face ID over a fresh challenge for `purpose`.
async fn auth(h: &mut H, sim: &mut Sim, ch: u64, id: u64, fid: &Authenticator, purpose: &str) -> Value {
    let c = ask(h, sim, ch, id, "auth-start", json!({"for": purpose})).await;
    fid.assert(&c["ok"]["challenge"])
}

async fn until(h: &H, what: &str, f: impl Fn(&H) -> bool) {
    for _ in 0..200 {
        if f(h) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

fn step(h: &H) -> Step {
    h.phone.hosted_list()[0].step
}

#[tokio::test]
async fn invite_join_pay_move_and_delete() {
    let mut h = harness(None);
    h.rpc.hosting.store(true, Ordering::SeqCst);
    h.phone.set_maker(Arc::new(MockMaker(h.rpc.clone())));
    // The owner's phone, with Face ID.
    let mut owner = Sim::new();
    paired(&mut h, &mut owner, 1).await;
    let fid = Authenticator::new(7);
    assert!(add_passkey(&mut h, &mut owner, 1, 1, &fid, false).await["ok"].is_object());

    // Only house 5 is this wallet's members-only house.
    let r = ask(&mut h, &mut owner, 1, 3, "houses-mine", json!({})).await;
    assert_eq!(r["ok"], json!([{"house": 5, "name": "Bank of the Stall", "type": "members"}]), "{r}");

    // An invite takes Face ID, then "Let my phone send" (joining is signed here).
    let r = ask(&mut h, &mut owner, 1, 4, "invite", json!({"house": 5})).await;
    assert_eq!(r["err"], ERR_AUTH_FAILED);
    let a = auth(&mut h, &mut owner, 1, 5, &fid, "invite").await;
    let r = ask(&mut h, &mut owner, 1, 6, "invite", json!({"house": 5, "auth": a})).await;
    assert_eq!(r["err"], real::ERR_INVITE_NEEDS_SEND);
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(pass(PASS).unwrap()).await.unwrap();
    let a = auth(&mut h, &mut owner, 1, 7, &fid, "invite").await;
    let r = ask(&mut h, &mut owner, 1, 8, "invite", json!({"house": 6, "auth": a})).await;
    assert_eq!(r["err"], real::ERR_NOT_MINE, "a members-only house that isn't this wallet's");
    let a = auth(&mut h, &mut owner, 1, 9, &fid, "invite").await;
    let r = ask(&mut h, &mut owner, 1, 10, "invite", json!({"house": 5, "auth": a})).await;
    let url = r["ok"]["url"].as_str().unwrap().to_string();
    assert_eq!(r["ok"]["secs"], 600);
    let link = link_of(&url);
    assert_eq!(link["h"], json!({"house": 5, "name": "Bank of the Stall", "by": "Test phone"}));
    let c = code_of(&url);

    // The shopkeeper's phone. A request that doesn't say hosted, with the invite's code, is refused; so is a hosted one
    // with an ordinary pairing code.
    let mut shop = Sim::new();
    h.feed(2, shop.pair_frame(&h.d_pub(), &c, "Shop"));
    assert_eq!(h.next().await["d"]["t"], "pair-refused");
    let (pairing, _) = h.phone.pair_start().unwrap();
    h.feed(2, shop.pair_frame_hosted(&h.d_pub(), &code_of(&pairing), "Shop", true).0);
    assert_eq!(h.next().await["d"]["t"], "pair-refused");
    assert!(h.phone.pair_pending().is_empty() && h.phone.hosted_pending().is_empty());

    // The hosted request: the inviting phone hears an allow card with the code the shop's phone shows.
    let (frame, code) = shop.pair_frame_hosted(&h.d_pub(), &c, "Shop", true);
    h.feed(2, frame);
    let card = owner.open(&h.next().await["d"]);
    let allow = &card["allow"];
    assert_eq!(allow["code"], code);
    assert_eq!(allow["house"], 5);
    assert_eq!(allow["name"], "Shop");
    assert_eq!(h.ev.named(EV_PAIR).last().unwrap()["hosted"], true);
    // Another key's Face ID doesn't allow it; the inviting phone's does.
    let bad = Authenticator::new(9).assert(&allow["challenge"]);
    let r = ask(&mut h, &mut owner, 1, 11, "allow", json!({"id": allow["id"], "auth": bad})).await;
    assert_eq!(r["err"], ERR_AUTH_FAILED);
    let (r, others) = ask_all(&mut h, &mut owner, 1, 12, "allow", json!({"id": allow["id"], "auth": fid.assert(&allow["challenge"])})).await;
    assert_eq!(r["ok"], json!({}), "{r}");
    assert!(others.iter().any(|o| o["allow-done"] == allow["id"]), "{others:?}");
    assert!(others.iter().any(|o| o["ch"] == 2 && o["d"]["t"] == "paired"), "{others:?}");
    assert!(h.phone.devices().len() == 1, "not among the owner's phones");

    // The wallet is made (encrypted, the words' key, its first address) and waits for its words.
    until(&h, "the wallet made", |h| step(h) == Step::Words).await;
    let hp = h.phone.hosted_list()[0].clone();
    let wallet = format!("hosted-{}", hp.id);
    let member = hp.member.clone().unwrap();
    assert!(h.rpc.hosted.lock().unwrap()[&wallet].pass.is_some());
    assert_eq!(h.rpc.hosted.lock().unwrap()[&wallet].unlocked_until, 0, "locked again");

    // Its session: a passkey first, with no other phone's approval.
    session(&mut h, &mut shop, 3).await;
    let r = ask(&mut h, &mut shop, 3, 1, "status", json!({})).await;
    assert_eq!((r["ok"]["hosted"].clone(), r["ok"]["step"].clone(), r["ok"]["house"].clone()), (json!(true), json!("words"), json!(5)));
    assert_eq!(ask(&mut h, &mut shop, 3, 2, "balance", json!({})).await["err"], real::ERR_HOSTED_FACE_ID);
    let sfid = Authenticator::new(11);
    assert_eq!(add_passkey(&mut h, &mut shop, 3, 3, &sfid, true).await, json!({"id": 4, "ok": {}}));

    // Nothing of the owner's door.
    for (i, m) in ["houses-mine", "invite", "approve", "passkey-set"].iter().enumerate() {
        let r = ask(&mut h, &mut shop, 3, 10 + i as u64, m, json!({})).await;
        assert!(r["err"].is_string(), "{m}: {r}");
    }
    // And the owner's phone has no hosted methods.
    assert_eq!(ask(&mut h, &mut owner, 1, 20, "words", json!({})).await["err"], "unknown method");

    // The words: 24, the wallet's own (its first address is the member address).
    let r = ask(&mut h, &mut shop, 3, 20, "words", json!({})).await;
    let words: Vec<String> = serde_json::from_value(r["ok"]["words"].clone()).unwrap();
    assert_eq!(words.len(), 24);
    let e = crate::seed::parse_words(&words.join(" ")).unwrap();
    let hd = crate::seed::freebank_hd_seed(&e).unwrap();
    assert_eq!(crate::seed::address(&hd, false, 0).unwrap(), member);
    // Confirmed with Face ID; the desktop then forgets them.
    assert_eq!(ask(&mut h, &mut shop, 3, 21, "words-ok", json!({})).await["err"], ERR_AUTH_FAILED);
    let a = auth(&mut h, &mut shop, 3, 22, &sfid, "change").await;
    assert_eq!(ask(&mut h, &mut shop, 3, 23, "words-ok", json!({"auth": a})).await["ok"], json!({}));
    assert!(h.phone.store.load_hosted_keys_checked().unwrap().keys[0].entropy.is_none());
    assert!(ask(&mut h, &mut shop, 3, 24, "words", json!({})).await["err"].is_string(), "shown once");

    // Joining: one member change, signed with the phone-send passphrase; ready once the member list says active.
    until(&h, "the member change", |h| !h.rpc.params_of("addhousemembers").is_empty()).await;
    assert_eq!(h.rpc.params_of("addhousemembers")[0], vec![json!(5), json!([member]), json!(real::MEMBER_FEE)]);
    // And a little sECX for its payments' fees, from the desktop's wallet in the same unlock (found in the real run).
    assert_eq!(h.rpc.sends(), vec![vec![json!(member), json!(real::FEE_FLOAT)]]);
    until(&h, "the float recorded", |h| h.phone.store.load_hosted().phones[0].float_txid.is_some()).await;
    assert!(!h.rpc.unlocked(), "the desktop's wallet locked again");
    assert_eq!(step(&h), Step::Joining);
    h.rpc.members.lock().unwrap()[0].1 = true;
    until(&h, "ready", |h| step(h) == Step::Ready).await;
    assert_eq!(h.rpc.params_of("addhousemembers").len(), 1, "sent once");

    // Its own wallet: balance, notes, the member address to receive at.
    let r = ask(&mut h, &mut shop, 3, 30, "balance", json!({})).await;
    assert_eq!(r["ok"]["confirmed"], 0.25);
    let r = ask(&mut h, &mut shop, 3, 31, "notes", json!({})).await;
    assert_eq!(r["ok"][0]["house"], 5);
    assert_eq!(r["ok"][0]["amount"], 0.07);
    assert_eq!(ask(&mut h, &mut shop, 3, 32, "receive", json!({})).await["ok"]["address"], member);
    // A payment takes Face ID every time, and comes from the hosted wallet, locked again after.
    let r = ask(&mut h, &mut shop, 3, 33, "note-send", json!({"house": 5, "address": TO, "amount": 0.01})).await;
    assert_eq!(r["err"], ERR_AUTH_FAILED);
    let a = auth(&mut h, &mut shop, 3, 34, &sfid, "send").await;
    let r = ask(&mut h, &mut shop, 3, 35, "note-send", json!({"house": 5, "address": TO, "amount": 0.01, "auth": a})).await;
    assert!(r["ok"]["txid"].is_string(), "{r}");
    let sent = h.rpc.params_of(&format!("{wallet}/transfernote"));
    assert_eq!(sent, vec![vec![json!(5), json!(1_000_000), json!(NOTE_FEE), json!(TO)]]);
    assert!(h.rpc.params_of("transfernote").is_empty(), "never from the owner's wallet");
    assert_eq!(h.rpc.hosted.lock().unwrap()[&wallet].unlocked_until, 0);

    // Moving home to fresh words : the shopkeeper's own computer's address (here TO) joins the house, the notes
    // go, then the sECX; done when the wallet holds nothing. Its own address, or a non-address, is refused.
    let a = auth(&mut h, &mut shop, 3, 40, &sfid, "change").await;
    let r = ask(&mut h, &mut shop, 3, 41, "move-home", json!({"address": member, "auth": a})).await;
    assert!(r["err"].as_str().unwrap().contains("own"), "{r}");
    let a = auth(&mut h, &mut shop, 3, 42, &sfid, "change").await;
    assert!(ask(&mut h, &mut shop, 3, 43, "move-home", json!({"address": "nope", "auth": a})).await["err"].is_string());
    // Nor one of the house's own keys (the re-review of v0.2.8, N4).
    let partner = crate::seed::p2pkh_address(&hex::decode(tests_partner_pubkey()).unwrap());
    let a = auth(&mut h, &mut shop, 3, 50, &sfid, "change").await;
    let r = ask(&mut h, &mut shop, 3, 51, "move-home", json!({"address": partner, "auth": a})).await;
    assert!(r["err"].as_str().unwrap().contains("house's own"), "{r}");
    // The new computer is told first, and says whether the address is its own (N3): here the owner's desktop plays
    // it, and TO isn't its wallet's.
    let r = ask(&mut h, &mut owner, 1, 60, "move-notice", json!({"address": TO, "house_name": "Bank of the Stall"})).await;
    assert_eq!(r["ok"], json!({"mine": false}));
    assert_eq!(h.ev.named(EV_MOVE_NOTICE).last().unwrap()["address"], TO);
    let a = auth(&mut h, &mut shop, 3, 44, &sfid, "change").await;
    assert_eq!(ask(&mut h, &mut shop, 3, 45, "move-home", json!({"address": TO, "auth": a})).await["ok"], json!({}));
    assert_eq!(step(&h), Step::Moving);
    assert_eq!(h.ev.named(EV_HOSTED_MOVING).last().unwrap()["address"], TO, "the owner sees the new member (N4)");
    // A move can be stopped (N1), and started again.
    let a = auth(&mut h, &mut shop, 3, 52, &sfid, "change").await;
    assert_eq!(ask(&mut h, &mut shop, 3, 53, "move-stop", json!({"auth": a})).await["ok"], json!({}));
    assert_eq!(step(&h), Step::Ready);
    // A move started names its destination for good: another is refused (the final re-review, N4).
    let a = auth(&mut h, &mut shop, 3, 56, &sfid, "change").await;
    let r = ask(&mut h, &mut shop, 3, 57, "move-home", json!({"address": TO2, "auth": a})).await;
    assert!(r["err"].as_str().unwrap().contains("already"), "{r}");
    let a = auth(&mut h, &mut shop, 3, 54, &sfid, "change").await;
    assert_eq!(ask(&mut h, &mut shop, 3, 55, "move-home", json!({"address": TO, "auth": a})).await["ok"], json!({}));
    until(&h, "his address's member change", |h| h.rpc.params_of("addhousemembers").len() == 2).await;
    assert_eq!(h.rpc.params_of("addhousemembers")[1], vec![json!(5), json!([TO]), json!(real::MEMBER_FEE)]);
    assert_eq!(h.rpc.sends().len(), 1, "no second fee float");
    assert_eq!(step(&h), Step::Moving, "nothing goes before his address is a member");
    assert!(h.rpc.params_of(&format!("{wallet}/transfernote")).len() == 1);
    h.rpc.members.lock().unwrap().iter_mut().for_each(|m| m.1 = true);
    until(&h, "moved", |h| step(h) == Step::Moved).await;
    let moved_notes = h.rpc.params_of(&format!("{wallet}/transfernote"));
    assert_eq!(moved_notes[1], vec![json!(5), json!(6_000_000), json!(NOTE_FEE), json!(TO)], "all of house 5's notes");
    assert_eq!(h.rpc.params_of(&format!("{wallet}/sendtoaddress")).last().unwrap(), &vec![json!(TO), json!(0.25), json!(""), json!(""), json!(true)]);
    assert!(h.phone.store.load_hosted().phones[0].empty);
    assert_eq!(h.ev.named(EV_HOSTED_MOVED).len(), 1);
    // The old address comes off the house (N2).
    until(&h, "the old member removed", |h| h.phone.store.load_hosted().phones[0].old_removed.is_some()).await;
    assert!(!h.rpc.members.lock().unwrap().iter().any(|(a, on)| *a == member && *on));
    // Money that reaches the old copy later is sent on before the copy can go (N2).
    h.rpc.hosted_ecx.lock().unwrap().insert(wallet.clone(), 0.05);
    let e = h.phone.hosted_empty_now(&hp.id).await.unwrap_err();
    assert!(e.contains("after the move"), "{e}");
    until(&h, "sent on", |h| step(h) == Step::Moved && h.phone.store.load_hosted().phones[0].empty).await;
    assert_eq!(h.phone.hosted_empty_now(&hp.id).await, Ok(true));
    assert_eq!(ask(&mut h, &mut shop, 3, 46, "balance", json!({})).await["err"], real::ERR_MOVED);
    h.phone.hosted_remove(&hp.id).unwrap();
    assert_eq!(h.next().await["d"]["t"], "denied");
    assert_eq!(h.next().await, json!({"t": "close", "ch": 3}), "and the relay drops it");
    h.feed(4, shop.hello());
    assert_eq!(h.next().await["d"]["t"], "denied");

    // At the node's next start the emptied copy is deleted outright, with its passphrase, wherever the node keeps
    // wallets, and leaves the lists.
    let datadir = h.dir.join("node");
    std::fs::create_dir_all(datadir.join("wallets")).unwrap();
    std::fs::write(datadir.join("wallets").join(&wallet), b"wallet").unwrap();
    real::sweep_removed(&h.dir, &datadir);
    assert!(!datadir.join("wallets").join(&wallet).exists());
    assert!(!h.dir.join("hosted-removed").exists(), "nothing kept aside: it held nothing");
    assert!(h.phone.store.load_hosted().phones.is_empty());
    assert!(h.phone.store.load_hosted_keys_checked().unwrap().keys.is_empty());
}

#[tokio::test]
async fn a_copy_that_still_holds_money_moves_aside_and_a_missing_file_waits() {
    let mut h = harness(None);
    let datadir = h.dir.join("node");
    std::fs::create_dir_all(&datadir).unwrap();
    for (id, empty) in [("a", false), ("b", false)] {
        h.phone.host_for_test(real::HostedPhone { id: id.into(), wallet: format!("hosted-{id}"), remove: true, empty, ..Default::default() });
    }
    h.phone.store.save_hosted_keys(&real::HostedKeys { keys: vec![real::HostedKey { id: "a".into(), pass: "pa".into(), entropy: None }] }).unwrap();
    std::fs::write(datadir.join("hosted-a"), b"wallet").unwrap();
    real::sweep_removed(&h.dir, &datadir);
    let aside: Vec<String> = std::fs::read_dir(h.dir.join("hosted-removed")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(aside.len(), 2, "{aside:?}");
    assert!(aside.iter().any(|n| n.ends_with(".passphrase")));
    let left = h.phone.store.load_hosted().phones;
    assert_eq!(left.len(), 1, "b's file wasn't found: it waits");
    assert!(left[0].why.as_deref().unwrap().contains("wasn't found"));
    // A damaged keys file is never written over (security review of v0.2.8, L1).
    std::fs::write(h.dir.join("phone/hosted-keys.json"), b"{not json").unwrap();
    assert!(h.phone.store.load_hosted_keys_checked().is_err());
    real::sweep_removed(&h.dir, &datadir);
    assert_eq!(std::fs::read(h.dir.join("phone/hosted-keys.json")).unwrap(), b"{not json");
    let _ = &mut h;
}

#[tokio::test]
async fn an_invite_runs_out_and_its_asks_are_refused() {
    let mut h = harness(None);
    h.rpc.hosting.store(true, Ordering::SeqCst);
    h.phone.set_maker(Arc::new(MockMaker(h.rpc.clone())));
    let mut owner = Sim::new();
    paired(&mut h, &mut owner, 1).await;
    let fid = Authenticator::new(7);
    add_passkey(&mut h, &mut owner, 1, 1, &fid, false).await;
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(pass(PASS).unwrap()).await.unwrap();
    let a = auth(&mut h, &mut owner, 1, 3, &fid, "invite").await;
    let url = ask(&mut h, &mut owner, 1, 4, "invite", json!({"house": 5, "auth": a})).await["ok"]["url"].as_str().unwrap().to_string();
    let shop = Sim::new();
    h.feed(2, shop.pair_frame_hosted(&h.d_pub(), &code_of(&url), "Shop", true).0);
    let _card = h.next().await;
    assert_eq!(h.phone.hosted_pending().len(), 1);
    h.later(real::INVITE_TTL_SECS);
    h.phone.expire();
    let mut seen = vec![h.next().await, h.next().await];
    seen.sort_by_key(|f| f["ch"].as_u64());
    assert_eq!(seen[1]["d"]["t"], "pair-refused", "{seen:?}");
    assert!(h.phone.hosted_pending().is_empty());
    // The desktop can't allow it any more either, and nothing was made.
    assert!(h.phone.hosted_list().is_empty());
    h.feed(2, shop.pair_frame_hosted(&h.d_pub(), &code_of(&url), "Shop", true).0);
    assert_eq!(h.next().await["d"]["t"], "pair-refused");
}

#[tokio::test]
async fn a_phone_cant_be_both_an_owners_and_hosted() {
    let mut h = harness(None);
    h.rpc.hosting.store(true, Ordering::SeqCst);
    let mut owner = Sim::new();
    paired(&mut h, &mut owner, 1).await;
    let fid = Authenticator::new(7);
    add_passkey(&mut h, &mut owner, 1, 1, &fid, false).await;
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(pass(PASS).unwrap()).await.unwrap();
    let a = auth(&mut h, &mut owner, 1, 3, &fid, "invite").await;
    let url = ask(&mut h, &mut owner, 1, 4, "invite", json!({"house": 5, "auth": a})).await["ok"]["url"].as_str().unwrap().to_string();
    // The owner's own phone asking to be hosted is refused.
    h.feed(2, owner.twin().pair_frame_hosted(&h.d_pub(), &code_of(&url), "Again", true).0);
    assert_eq!(h.next().await["d"]["t"], "pair-refused");
    assert!(h.phone.hosted_pending().is_empty());
}

#[tokio::test]
async fn two_phones_at_most_wait_on_one_invite_and_each_card_says_so() {
    // Security review of v0.2.8, L4: someone who saw the invite's QR can't fill the queue, and the inviting phone is
    // told another phone opened it.
    let mut h = harness(None);
    h.rpc.hosting.store(true, Ordering::SeqCst);
    let mut owner = Sim::new();
    paired(&mut h, &mut owner, 1).await;
    let fid = Authenticator::new(7);
    add_passkey(&mut h, &mut owner, 1, 1, &fid, false).await;
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(pass(PASS).unwrap()).await.unwrap();
    let a = auth(&mut h, &mut owner, 1, 3, &fid, "invite").await;
    let url = ask(&mut h, &mut owner, 1, 4, "invite", json!({"house": 5, "auth": a})).await["ok"]["url"].as_str().unwrap().to_string();
    let c = code_of(&url);
    h.feed(2, Sim::new().pair_frame_hosted(&h.d_pub(), &c, "Shop", true).0);
    assert_eq!(owner.open(&h.next().await["d"])["allow"]["others"], 0);
    h.feed(3, Sim::new().pair_frame_hosted(&h.d_pub(), &c, "Watcher", true).0);
    let cards = [owner.open(&h.next().await["d"]), owner.open(&h.next().await["d"])];
    assert!(cards.iter().all(|m| m["allow"]["others"] == 1), "{cards:?}");
    h.feed(4, Sim::new().pair_frame_hosted(&h.d_pub(), &c, "Third", true).0);
    assert_eq!(h.next().await["d"]["t"], "pair-refused");
    assert_eq!(h.phone.hosted_pending().len(), 2);
}

#[tokio::test]
async fn a_move_without_fee_money_stops_after_a_few_top_ups() {
    // The re-review of v0.2.8, N1: a move can't loop for ever. With no sECX for the notes' fee, the house tops up three
    // times, then the move fails with why, and the desktop can try again.
    let h = harness(None);
    let hid = "stuck000000000000".to_string();
    let wallet = format!("hosted-{hid}");
    let entropy = crate::seed::new_entropy();
    let hd = crate::seed::freebank_hd_seed(&entropy).unwrap();
    let member = crate::seed::address(&hd, false, 0).unwrap();
    h.rpc.hosted.lock().unwrap().insert(wallet.clone(), MockWallet { pass: Some("pw".into()), hd: Some(hex::encode(*hd)), ..Default::default() });
    h.rpc.hosted_ecx.lock().unwrap().insert(wallet.clone(), 0.0);
    h.rpc.members.lock().unwrap().push((TO.into(), true));
    h.phone.store.save_hosted_keys(&real::HostedKeys { keys: vec![real::HostedKey { id: hid.clone(), pass: "pw".into(), entropy: None }] }).unwrap();
    h.phone.host_for_test(real::HostedPhone {
        id: hid.clone(),
        name: "Stuck".into(),
        house: 5,
        wallet: wallet.clone(),
        step: Step::Moving,
        member: Some(member.clone()),
        move_to: Some(TO.into()),
        ..Default::default()
    });
    tokio::spawn(h.phone.clone().move_home(hid.clone()));
    until(&h, "failed", |h| step(h) == Step::Failed).await;
    let l = h.phone.store.load_hosted().phones;
    assert_eq!(l[0].move_topups, real::MOVE_TOPUPS);
    assert!(l[0].why.as_deref().unwrap().contains("no sECX"), "{:?}", l[0].why);
    assert_eq!(h.rpc.sends().len(), real::MOVE_TOPUPS as usize, "three top-ups to the member address");
    assert!(h.rpc.params_of(&format!("{wallet}/transfernote")).is_empty());
}
