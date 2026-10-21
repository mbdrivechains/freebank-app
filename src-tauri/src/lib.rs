mod activity;
mod ask;
mod admin;
mod app_update;
mod changer;
mod clipboard;
mod commands;
mod ecash;
mod faucet;
mod feedback;
mod house;
mod house_watch;
mod network;
mod node;
mod phone;
mod recovery;
mod rpc;
mod security;
mod seed;
mod send;
mod stack;
mod wallet;
mod wallets;
mod withdraw;

use node::NodeManager;
use rpc::FreeBankClient;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

/// The Mac's menu: the default one, with "Report a Problem or Suggest Something…" in Help. It opens the
/// report dialog (the page listens for `report-open`). Linux windows have no menu bar, so it is macOS only.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn menu_with_report<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{Menu, MenuItem, MenuItemKind, HELP_SUBMENU_ID};
    let menu = Menu::default(handle)?;
    if let Some(MenuItemKind::Submenu(help)) = menu.get(HELP_SUBMENU_ID) {
        help.append(&MenuItem::with_id(handle, "report", "Report a Problem or Suggest Something…", true, None::<&str>)?)?;
    }
    // ⌘Q asks what to stop, as closing the window does (v0.2.5): our own Quit item in place of the standard one, which quits at once. A Mac shutdown
    // or logout doesn't go through the menu, so it still quits without asking.
    if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
        let items = app_menu.items()?;
        let quit = items.iter().position(|i| matches!(i, MenuItemKind::Predefined(p) if p.text().is_ok_and(|t| t.starts_with("Quit"))));
        if let Some(at) = quit {
            app_menu.remove_at(at)?;
            let name = handle.package_info().name.clone();
            app_menu.insert(&MenuItem::with_id(handle, "quit", format!("Quit {name}"), true, Some("CmdOrCtrl+Q"))?, at)?;
        }
    }
    Ok(menu)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn on_menu<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event: tauri::menu::MenuEvent) {
    use tauri::Emitter;
    if event.id() == "report" {
        let _ = app.emit("report-open", ());
    } else if event.id() == "quit" {
        node::background::quit_asked(app);
    }
}

/// Files the app was started with, other than stdin, stdout and stderr, stay out of the programs it starts. An
/// AppImage's runtime hands the app its mount (fd 1023) and a keep-alive pipe: a node or background part that
/// inherited them kept the closed app's AppImage mounted for as long as it ran. Called first in main; Linux only.
pub fn keep_inherited_files_from_children() {
    #[cfg(target_os = "linux")]
    {
        let Ok(dir) = std::fs::read_dir("/proc/self/fd") else { return };
        let fds: Vec<i32> =
            dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).filter(|fd| *fd > 2).collect();
        for fd in fds {
            // SAFETY: fcntl on a number that is no longer open fails with EBADF, and is then skipped.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                }
            }
        }
    }
}

/// Started as the phone link's background part (phone/background.rs): its app folder, and whether it is light
/// (started at login, daemon mode).
pub fn phone_background_requested(args: &[String]) -> Option<(std::path::PathBuf, bool)> {
    phone::background::requested(args)
}

/// The background part's whole life; its exit code.
pub fn phone_background_main(app_dir: std::path::PathBuf, light: bool) -> i32 {
    // It runs for the network whose app folder it was given (v0.5.0).
    network::set(network::of_app_dir(&app_dir));
    phone::background::main(app_dir, light)
}

#[cfg(unix)]
async fn any_signal(sigs: &mut [tokio::signal::unix::Signal]) {
    futures_util::future::select_all(sigs.iter_mut().map(|s| Box::pin(s.recv()))).await;
}

/// Whether `sig` was set to be ignored when the app started (`nohup` ignores SIGHUP).
#[cfg(unix)]
fn ignored(sig: libc::c_int) -> bool {
    // SAFETY: reads the current action only (a null new action changes nothing).
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(sig, std::ptr::null(), &mut old) == 0 && old.sa_sigaction == libc::SIG_IGN
    }
}

pub fn run() {
    // No core dump of the app's process (v0.4.10, the Truthcoin app's copy-words review, check C5): recovery words on
    // screen are in its memory, and a crash handler such as Ubuntu's apport would keep a dump. The node, started later,
    // is unaffected. A non-dumpable process can't call xdg-desktop-portal, which GTK uses for its file chooser (Restore
    // a backup file) only when GTK_USE_PORTAL=1: then the app stays dumpable rather than lose the file chooser.
    #[cfg(target_os = "linux")]
    if std::env::var("GTK_USE_PORTAL").map_or(true, |v| v != "1") {
        // SAFETY: sets a flag of this process only.
        unsafe {
            libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        }
    }
    // Copied recovery words leave the clipboard if the app panics (the release build aborts right after the hook), as
    // they do at a quit (review L3).
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report(info);
        clipboard::clear_after_panic();
    }));
    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.menu(menu_with_report).on_menu_event(on_menu);
    let app = builder
        .plugin(tauri_plugin_shell::init())
        .manage(Arc::new(Mutex::new(FreeBankClient::default())))
        // One guard for every walletpassphrase the app makes (wallet::RelockGuard).
        .manage(wallet::RelockState::default())
        // Automatic house attests (v0.4.6): off until turned on; the passphrase only in memory.
        .manage(house_watch::AutoAttestState::default())
        .setup(|app| {
            // FREEBANK_APP_DIR lets a developer run the app against a scratch folder.
            let top: std::path::PathBuf = match std::env::var_os("FREEBANK_APP_DIR") {
                Some(d) => d.into(),
                None => app.path().app_data_dir()?,
            };
            // The network this run is on (v0.5.0), and its own app folder: beta's is the top folder, as before.
            let net = network::chosen(&top);
            network::set(net);
            let dir = network::app_dir(&top, net);
            let client = app.state::<Arc<Mutex<FreeBankClient>>>().inner().clone();
            let relock = app.state::<wallet::RelockState>().inner().clone();
            // "Include recent activity" in a report reads this log (activity.rs).
            activity::init(&dir);
            // Off the main thread, as both may ask the system's user directory: "Start when I log in" names this
            // program as it is now (a moved AppImage), and what an interrupted app update left beside it goes.
            let login_dir = dir.clone();
            std::thread::spawn(move || {
                phone::login_item::refresh(&login_dir);
                app_update::sweep_leftovers();
            });
            activity::note(&format!("FreeBank app {} started on {}", env!("CARGO_PKG_VERSION"), net.id));
            // A background part kept the phone connected while the app was closed: it stops first,
            // so the app's own link never takes turns with it at the relay.
            app.manage(phone::commands::PhoneBackground(phone::background::take_back(&dir)));
            house_watch::spawn(
                app.handle().clone(),
                client.clone(),
                relock.clone(),
                app.state::<house_watch::AutoAttestState>().inner().clone(),
            );
            let phone = phone::commands::start(app.handle(), &dir, client, relock);
            let hosting = phone.0.as_ref().ok().cloned();
            app.manage(phone);
            let mgr = Arc::new(NodeManager::new(dir));
            // Demo mode (v0.4.4): a node left running finds its relay to the gateway again.
            {
                let m = mgr.clone();
                tauri::async_runtime::spawn(async move {
                    if m.settings.lock().await.demo {
                        if let Err(e) = node::relay::ensure(&m).await {
                            activity::note(&format!("demo relay: {}", e));
                        }
                    }
                });
            }
            // Hosted wallets (v0.2.8): the phone link makes them in this node, and finishes what a restart cut short.
            if let Some(p) = hosting {
                p.set_maker(Arc::new(wallets::HostedMaker { mgr: mgr.clone() }));
                tauri::async_runtime::spawn(p.resume_hosted());
            }
            // Several wallets (v0.2.6): the clients name the main wallet once others are listed, and the screens'
            // client the one chosen last time.
            {
                let (m, c) = (mgr.clone(), app.state::<Arc<Mutex<FreeBankClient>>>().inner().clone());
                tauri::async_runtime::spawn(async move {
                    let s = m.settings.lock().await.clone();
                    wallets::apply(&s, &mut *c.lock().await);
                });
            }
            // Bidding for FreeBank blocks (v0.2.6): does nothing until it is turned on in the eCash tab.
            ecash::bmm::spawn(mgr.clone());
            // The app's own eCash node stops itself before its disk fills (v0.5.1).
            stack::spawn_watch(mgr.clone());
            let updater = Arc::new(app_update::AppUpdater::new(mgr.http.clone()));
            // Automatic updates (v0.4.2): nothing happens unless Settings > App updates turns them on.
            app_update::spawn_auto(updater.clone(), mgr.clone());
            app.manage(updater);
            app.manage(mgr);
            // SIGTERM, SIGINT or SIGHUP end the app as a quit does (v0.4.10, review L3): copied words leave the clipboard,
            // and the node stops or keeps running as set. A signal ignored when the app started (`nohup`) stays ignored
            // (R4). Another one a second or more later, while the quit runs, ends the app at once, after the
            // clipboard's clear (R3).
            #[cfg(unix)]
            {
                let h = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    use tokio::signal::unix::{signal, SignalKind};
                    let mut sigs: Vec<_> = [
                        (libc::SIGTERM, SignalKind::terminate()),
                        (libc::SIGINT, SignalKind::interrupt()),
                        (libc::SIGHUP, SignalKind::hangup()),
                    ]
                    .into_iter()
                    .filter(|(n, _)| !ignored(*n))
                    .filter_map(|(_, k)| signal(k).ok())
                    .collect();
                    if sigs.is_empty() {
                        return;
                    }
                    any_signal(&mut sigs).await;
                    h.exit(0);
                    let first = std::time::Instant::now();
                    loop {
                        any_signal(&mut sigs).await;
                        if first.elapsed() >= std::time::Duration::from_secs(1) {
                            break;
                        }
                    }
                    clipboard::clear_at_exit();
                    // SAFETY: ends the process without running exit handlers while the main thread is inside GTK.
                    unsafe { libc::_exit(1) };
                });
            }
            Ok(())
        })
        // With "Keep running" on, the first close says what happens to the node (node/background.rs).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                node::background::on_close(window, api);
            }
        })
        .invoke_handler(tauri::generate_handler![
            network::network_info,
            faucet::faucet_ask,
            network::network_switch,
            stack::stack_status,
            stack::stack_install,
            stack::stack_install_progress,
            stack::stack_start,
            stack::stack_stop,
            stack::stack_remove,
            stack::stack_free,
            stack::stack_find_remote,
            stack::stack_use_remote,
            commands::get_balance,
            commands::get_new_address,
            commands::get_transactions,
            commands::connect_node,
            commands::get_connection_status,
            commands::get_blockchain_info,
            commands::rpc_call,
            // v0.2.0 send
            send::fee_choices,
            send::send_prepare,
            send::send_confirm,
            house::note_lock_check,
            house::note_lock_send,
            house::netting_sign,
            send::send_log,
            send::speed_up_quote,
            send::send_speed_up,
            send::history,
            send::history_csv,
            wallet::wallet_status,
            wallet::wallet_unlock,
            wallet::wallet_lock,
            // v0.2.0 wallet
            recovery::commands::wallet_protection,
            recovery::commands::wallet_info,
            recovery::commands::wallet_setup_start,
            recovery::commands::wallet_words_elsewhere,
            recovery::commands::wallet_setup_from_network,
            recovery::commands::wallet_setup_progress,
            recovery::commands::wallet_setup_words,
            recovery::commands::wallet_words_confirmed,
            recovery::commands::seed_check_words,
            recovery::commands::wallet_reveal,
            clipboard::copy_words,
            clipboard::words_own_copy,
            clipboard::clipboard_clear_words,
            clipboard::clipboard_forget_words,
            feedback::feedback_details,
            phone::commands::phone_keep_info,
            phone::commands::phone_remove_passkey,
            phone::commands::phone_keep_set,
            phone::commands::phone_login_set,
            phone::commands::phone_keep_connected_quit,
            feedback::feedback_send,
            feedback::feedback_activity,
            feedback::activity_note,
            recovery::commands::wallet_change_passphrase,
            recovery::commands::wallet_backup_now,
            recovery::commands::wallet_restore_file_check,
            recovery::commands::wallet_restore_file_start,
            recovery::commands::wallet_move_plan,
            recovery::commands::wallet_move_coins,
            // end v0.2.0 wallet
            node::commands::setup_info,
            node::commands::setup_save,
            node::commands::stack_check,
            node::commands::node_probe,
            node::commands::datadir_check,
            node::commands::validate_tag,
            node::commands::install_start,
            node::commands::install_progress,
            node::commands::install_cancel,
            node::commands::node_start,
            node::commands::node_stop,
            node::commands::node_progress,
            node::commands::node_status,
            node::commands::connect_local,
            node::commands::test_connection,
            node::commands::node_set_tag,
            node::commands::update_check,
            node::commands::update_start,
            node::commands::update_progress,
            node::commands::remove_programs,
            node::commands::delete_chain_data,
            node::commands::obliterate_plan,
            node::commands::wallet_backup,
            node::commands::obliterate,
            node::commands::remove_app_itself,
            node::commands::app_quit,
            node::commands::app_quit_asked,
            // v0.2.0 node
            node::commands::node_set_keep_running,
            node::commands::node_set_demo,
            node::commands::node_restart,
            node::commands::refetch_start,
            phone::commands::phone_pair_start,
            phone::commands::phone_pair_answer,
            phone::commands::phone_hosted,
            phone::commands::phone_hosted_answer,
            phone::commands::phone_hosted_remove,
            phone::commands::phone_hosted_retry,
            phone::commands::phone_hosted_remove_passkey,
            commands::member_addresses,
            phone::commands::phone_approve_info,
            phone::commands::phone_approve_set,
            phone::commands::phone_approval_cancel,
            phone::commands::phone_approve_cancel_scheduled,
            // v0.2.6: the app's eCash wallet
            ecash::commands::ecash_status,
            ecash::commands::ecash_setup,
            ecash::commands::ecash_receive,
            ecash::commands::ecash_history,
            ecash::commands::ecash_send_prepare,
            ecash::commands::ecash_send_confirm,
            ecash::commands::deposit_prepare,
            ecash::commands::deposit_confirm,
            ecash::commands::deposit_list,
            withdraw::withdraw_prepare,
            withdraw::withdraw_confirm,
            withdraw::withdraw_list,
            withdraw::withdraw_cancel,
            ask::ask_helper,
            house_watch::houses_due,
            house_watch::auto_attest_on,
            house_watch::auto_attest_off,
            changer::changer_info,
            changer::changer_quote,
            changer::changer_pay,
            changer::changer_orders,
            changer::changer_set,
            changer::changer_get,
            ecash::commands::ecash_bids_withdraw_prepare,
            ecash::commands::ecash_bids_withdraw_confirm,
            ecash::commands::ecash_login_get,
            ecash::commands::ecash_login_set,
            ecash::commands::bmm_status,
            // v0.2.6: several wallets
            wallets::wallets_list,
            wallets::wallet_select,
            wallets::wallet_forget,
            wallets::wallet_add_words,
            wallets::wallet_add_file,
            ecash::commands::bmm_set,
            phone::commands::phone_devices,
            phone::commands::phone_revoke,
            phone::commands::phone_set_limit,
            phone::commands::phone_confirm_send,
            phone::commands::phone_relay_status,
            phone::commands::phone_set_relay,
            phone::commands::phone_set_nostr_relays,
            phone::commands::phone_recent_sends,
            phone::commands::phone_wallet,
            phone::commands::phone_send_on,
            phone::commands::phone_send_off,
            // v0.2.0 panels
            security::security_check,
            security::security_reveal,
            // v0.2.4: the app updates itself
            app_update::app_update_check,
            app_update::app_update_start,
            app_update::app_update_progress,
            app_update::apt_updates_offer,
            app_update::apt_updates_enable,
            // v0.4.2: automatic updates, opt in
            app_update::app_auto_update_get,
            app_update::app_auto_update_set,
            app_update::app_update_restart,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        // The node this app started goes down with it, unless "Keep running" is on; a node someone
        // else started is left alone. Then what "Obliterate" left for exit goes: the screen used it
        // until now.
        if let tauri::RunEvent::Exit = event {
            let mgr = handle.state::<Arc<NodeManager>>().inner().clone();
            // Quit without the close notice (⌘Q on a Mac): the phone stays connected if so set; then
            // the phone-send passphrase leaves memory, and copied recovery words the clipboard.
            if let Some(p) = handle.try_state::<phone::commands::PhoneState>() {
                phone::commands::keep_at_exit(&mgr, &p);
                p.forget_passphrase();
            }
            clipboard::clear_at_exit();
            tauri::async_runtime::block_on(node::background::at_exit(&mgr));
            // The app's own eCash node and enforcer after the FreeBank node that uses them (v0.5.0).
            tauri::async_runtime::block_on(stack::at_exit(&mgr));
            node::obliterate::wipe_at_exit(&mgr);
        }
    });
}

#[cfg(all(test, target_os = "linux"))]
mod fd_tests {
    /// A file the app was started with, like an AppImage's keep-alive pipe, doesn't reach a program it starts.
    #[test]
    fn inherited_files_stay_out_of_children() {
        let mut p = [0; 2];
        // No O_CLOEXEC, as an AppImage's runtime leaves its pipe.
        assert_eq!(unsafe { libc::pipe(p.as_mut_ptr()) }, 0);
        let sees = |fd: i32| {
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("[ -e /proc/self/fd/{fd} ] && echo yes || echo no"))
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        assert_eq!(sees(p[0]), "yes", "the test's pipe reaches a child before");
        super::keep_inherited_files_from_children();
        assert_eq!(sees(p[0]), "no");
        assert_eq!(sees(p[1]), "no");
        for fd in p {
            unsafe { libc::close(fd) };
        }
    }
}

#[cfg(test)]
mod menu_tests {
    /// The Mac-only menu wiring in `run` type-checks on every system.
    #[test]
    fn the_report_menu_wiring_builds() {
        let _ = tauri::Builder::<tauri::Wry>::default().menu(super::menu_with_report).on_menu_event(super::on_menu);
    }
}
