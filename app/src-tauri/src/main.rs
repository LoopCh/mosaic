// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use mosaic_core::controller::{EngineVersion, ImportSummary, Snapshot};
use mosaic_core::model::{EngineChoice, Rule, Settings};
use mosaic_core::stats::ConnView;
use mosaic_core::Controller;
use tauri::State;

type Ctl<'a> = State<'a, Arc<Controller>>;

#[tauri::command]
async fn snapshot(ctl: Ctl<'_>) -> Result<Snapshot, String> {
    Ok(ctl.snapshot().await)
}

#[tauri::command]
async fn connect(ctl: Ctl<'_>) -> Result<(), String> {
    ctl.connect().await
}

#[tauri::command]
async fn disconnect(ctl: Ctl<'_>) -> Result<(), String> {
    ctl.disconnect().await;
    Ok(())
}

#[tauri::command]
async fn import_text(ctl: Ctl<'_>, text: String) -> Result<ImportSummary, String> {
    Ok(ctl.import_text(&text).await)
}

#[tauri::command]
async fn add_subscription(ctl: Ctl<'_>, url: String) -> Result<ImportSummary, String> {
    ctl.add_subscription(&url).await
}

#[tauri::command]
async fn update_subscriptions(ctl: Ctl<'_>) -> Result<ImportSummary, String> {
    Ok(ctl.update_all_subscriptions().await)
}

#[tauri::command]
async fn remove_subscription(ctl: Ctl<'_>, url: String) -> Result<(), String> {
    ctl.remove_subscription(&url).await;
    Ok(())
}

#[tauri::command]
async fn remove_node(ctl: Ctl<'_>, id: String) -> Result<(), String> {
    ctl.remove_node(&id).await;
    Ok(())
}

#[tauri::command]
async fn select_node(ctl: Ctl<'_>, id: String) -> Result<(), String> {
    ctl.select_node(&id).await
}

#[tauri::command]
async fn set_node_engine(ctl: Ctl<'_>, id: String, engine: EngineChoice) -> Result<(), String> {
    ctl.set_node_engine(&id, engine).await
}

#[tauri::command]
async fn set_rules(ctl: Ctl<'_>, rules: Vec<Rule>, final_target: String) -> Result<(), String> {
    ctl.set_rules(rules, final_target).await;
    Ok(())
}

#[tauri::command]
async fn set_settings(ctl: Ctl<'_>, settings: Settings) -> Result<(), String> {
    ctl.set_settings(settings).await
}

#[tauri::command]
async fn test_latency(ctl: Ctl<'_>) -> Result<(), String> {
    ctl.test_latency().await
}

#[tauri::command]
async fn connections(ctl: Ctl<'_>) -> Result<Vec<ConnView>, String> {
    Ok(ctl.connections())
}

#[tauri::command]
async fn logs(ctl: Ctl<'_>) -> Result<Vec<String>, String> {
    Ok(ctl.logs())
}

#[tauri::command]
async fn engine_versions(ctl: Ctl<'_>) -> Result<Vec<EngineVersion>, String> {
    Ok(ctl.engine_versions().await)
}

fn main() {
    let ctl = Arc::new(Controller::load());
    let on_exit = ctl.clone();
    tauri::Builder::default()
        .manage(ctl)
        .invoke_handler(tauri::generate_handler![
            snapshot,
            connect,
            disconnect,
            import_text,
            add_subscription,
            update_subscriptions,
            remove_subscription,
            remove_node,
            select_node,
            set_node_engine,
            set_rules,
            set_settings,
            test_latency,
            connections,
            logs,
            engine_versions
        ])
        .build(tauri::generate_context!())
        .expect("не удалось запустить Mosaic")
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                // Restore the system proxy and kill engines before the process exits.
                tauri::async_runtime::block_on(on_exit.disconnect());
            }
        });
}
