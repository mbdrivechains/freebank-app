mod commands;
mod node;
mod rpc;

use node::NodeManager;
use rpc::FreeBankClient;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(Arc::new(Mutex::new(FreeBankClient::default())))
        .setup(|app| {
            // FREEBANK_APP_DIR lets a developer run the app against a scratch folder.
            let dir = match std::env::var_os("FREEBANK_APP_DIR") {
                Some(d) => d.into(),
                None => app.path().app_data_dir()?,
            };
            app.manage(Arc::new(NodeManager::new(dir)));
            Ok(())
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
            node::commands::setup_info,
            node::commands::setup_save,
            node::commands::stack_check,
            node::commands::node_probe,
            node::commands::datadir_check,
            node::commands::validate_tag,
            node::commands::install_start,
            node::commands::install_progress,
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
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        // The node this app started goes down with it; a node someone else started is left alone.
        if let tauri::RunEvent::Exit = event {
            let mgr = handle.state::<Arc<NodeManager>>().inner().clone();
            let _ = tauri::async_runtime::block_on(node::process::stop(&mgr));
        }
    });
}
