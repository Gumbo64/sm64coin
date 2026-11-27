// Only these commands exist for the webview: capabilities/default.json grants each by name.
const COMMANDS: &[&str] = &["host_info", "rom_set", "rom_pick", "rom_bytes", "node_start", "node_stop", "node_status", "node_rpc", "remote_rpc", "remote_node_get", "remote_node_set", "miner_start", "miner_stop", "miner_status", "miner_viz_on", "miner_viz_mail", "wallet_state", "wallet_rpc", "wallet_settings", "wallet_activity", "link_start", "link_status", "link_cancel", "link_finish"];

fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS))).expect("tauri build");
}
