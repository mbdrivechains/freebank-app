mod clipboard;
mod commands;
mod feedback;
mod gates;
mod node;
mod phone;
mod recovery;
mod rpc;
mod security;
mod seed;
mod send;
mod wallet;

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
    Ok(menu)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn on_menu<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event: tauri::menu::MenuEvent) {
    use tauri::Emitter;
    if event.id() == "report" {
        let _ = app.emit("report-open", ());
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

/// Started as the phone link's background part (phone/background.rs): its app folder.
pub fn phone_background_requested(args: &[String]) -> Option<std::path::PathBuf> {
    phone::background::requested(args)
}

/// The background part's whole life; its exit code.
pub fn phone_background_main(app_dir: std::path::PathBuf) -> i32 {
    phone::background::main(app_dir)
}

pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.menu(menu_with_report).on_menu_event(on_menu);
    let app = builder
        .plugin(tauri_plugin_shell::init())
        .manage(Arc::new(Mutex::new(FreeBankClient::default())))
        // One guard for every walletpassphrase the app makes (wallet::RelockGuard).
        .manage(wallet::RelockState::default())
        .setup(|app| {
            // FREEBANK_APP_DIR lets a developer run the app against a scratch folder.
            let dir = match std::env::var_os("FREEBANK_APP_DIR") {
                Some(d) => d.into(),
                None => app.path().app_data_dir()?,
            };
            let client = app.state::<Arc<Mutex<FreeBankClient>>>().inner().clone();
            let relock = app.state::<wallet::RelockState>().inner().clone();
            // A background part kept the phone connected while the app was closed: it stops first,
            // so the app's own link never takes turns with it at the relay.
            app.manage(phone::commands::PhoneBackground(phone::background::take_back(&dir)));
            let phone = phone::commands::start(app.handle(), &dir, client, relock);
            app.manage(phone);
            app.manage(Arc::new(NodeManager::new(dir)));
            Ok(())
        })
        // With "Keep running" on, the first close says what happens to the node (node/background.rs).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                node::background::on_close(window, api);
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_balance,
            commands::get_new_address,
            commands::send_transaction,
            commands::get_transactions,
            commands::connect_node,
            commands::get_connection_status,
            commands::get_blockchain_info,
            commands::rpc_call,
            // v0.2.0 send
            send::fee_choices,
            send::send_prepare,
            send::send_confirm,
            send::send_log,
            send::speed_up_quote,
            send::send_speed_up,
            send::history,
            send::history_csv,
            gates::gate_info,
            wallet::wallet_status,
            wallet::wallet_unlock,
            wallet::wallet_lock,
            // v0.2.0 wallet
            recovery::commands::wallet_protection,
            recovery::commands::wallet_info,
            recovery::commands::wallet_setup_start,
            recovery::commands::wallet_setup_progress,
            recovery::commands::wallet_setup_words,
            recovery::commands::wallet_words_confirmed,
            recovery::commands::seed_check_words,
            recovery::commands::wallet_reveal,
            clipboard::copy_words,
            feedback::feedback_details,
            phone::commands::phone_keep_info,
            phone::commands::phone_remove_passkey,
            phone::commands::phone_keep_set,
            phone::commands::phone_keep_connected_quit,
            feedback::feedback_send,
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
            node::commands::app_quit,
            // v0.2.0 node
            node::commands::node_set_keep_running,
            node::commands::node_restart,
            node::commands::refetch_start,
            phone::commands::phone_pair_start,
            phone::commands::phone_pair_answer,
            phone::commands::phone_devices,
            phone::commands::phone_revoke,
            phone::commands::phone_set_limit,
            phone::commands::phone_confirm_send,
            phone::commands::phone_relay_status,
            phone::commands::phone_set_relay,
            phone::commands::phone_recent_sends,
            phone::commands::phone_wallet,
            phone::commands::phone_send_on,
            phone::commands::phone_send_off,
            // v0.2.0 panels
            security::security_check,
            security::security_reveal,
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
