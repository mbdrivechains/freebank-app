mod commands;
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

pub fn run() {
    let app = tauri::Builder::default()
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
            // The phone-send passphrase leaves memory first.
            if let Some(p) = handle.try_state::<phone::commands::PhoneState>() {
                p.forget_passphrase();
            }
            let mgr = handle.state::<Arc<NodeManager>>().inner().clone();
            tauri::async_runtime::block_on(node::background::at_exit(&mgr));
            node::obliterate::wipe_at_exit(&mgr);
        }
    });
}
