// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod single;

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Duration;

use mosaic_core::controller::{EngineVersion, ImportSummary, Snapshot};
use mosaic_core::model::{EngineChoice, EngineKind, Rule, Settings};
use mosaic_core::updates::{AppUpdate, EngineUpdate};
use mosaic_core::stats::ConnView;
use mosaic_core::Controller;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent, Wry};

type Ctl<'a> = State<'a, Arc<Controller>>;

// ------------------------------------------------------------------ commands

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
async fn check_engine_updates(ctl: Ctl<'_>) -> Result<Vec<EngineUpdate>, String> {
    Ok(ctl.check_engine_updates().await)
}

#[tauri::command]
async fn update_engine(ctl: Ctl<'_>, kind: EngineKind) -> Result<String, String> {
    ctl.update_engine(kind).await
}

#[tauri::command]
async fn reset_engines(ctl: Ctl<'_>) -> Result<(), String> {
    ctl.reset_engines().await
}

#[tauri::command]
async fn check_app_update(ctl: Ctl<'_>) -> Result<Option<AppUpdate>, String> {
    Ok(ctl.check_app_update().await)
}

/// Download the new installer, start it and quit so it can replace the files.
#[tauri::command]
async fn install_app_update(app: AppHandle, ctl: Ctl<'_>) -> Result<(), String> {
    let installer = ctl.download_app_update().await?;
    ctl.disconnect().await;
    std::process::Command::new(&installer).spawn().map_err(|e| format!("cannot start the installer: {e}"))?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
async fn set_smart(ctl: Ctl<'_>, on: bool) -> Result<(), String> {
    ctl.set_smart(on).await;
    Ok(())
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

// ------------------------------------------------------------------ tray

struct Labels {
    auto: &'static str,
    open: &'static str,
    connect: &'static str,
    disconnect: &'static str,
    nodes: &'static str,
    quit: &'static str,
    on: &'static str,
    off: &'static str,
}

fn labels(ui_language: &str) -> Labels {
    let ru = match ui_language {
        "ru" => true,
        "en" => false,
        _ => mosaic_core::sys::ui_lang_is_ru(),
    };
    if ru {
        Labels { auto: "Авто (самый быстрый)", open: "Открыть Mosaic", connect: "Подключить", disconnect: "Отключить", nodes: "Узлы", quit: "Выход", on: "подключено", off: "отключено" }
    } else {
        Labels { auto: "Auto (fastest)", open: "Open Mosaic", connect: "Connect", disconnect: "Disconnect", nodes: "Nodes", quit: "Quit", on: "connected", off: "disconnected" }
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Windows menus treat '&' as a mnemonic marker.
fn menu_text(s: &str) -> String {
    s.replace('&', "&&")
}

fn build_menu(app: &AppHandle, snap: &Snapshot) -> tauri::Result<Menu<Wry>> {
    let l = labels(&snap.settings.ui_language);
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(app, "show", l.open, true, None::<&str>)?)?;
    let toggle = if snap.status.connected { l.disconnect } else { l.connect };
    menu.append(&MenuItem::with_id(app, "toggle", toggle, !snap.nodes.is_empty(), None::<&str>)?)?;
    if !snap.nodes.is_empty() {
        let sub = Submenu::with_id(app, "nodes", l.nodes, true)?;
        let smart = snap.settings.smart_connect;
        sub.append(&CheckMenuItem::with_id(app, "auto", l.auto, true, smart, None::<&str>)?)?;
        sub.append(&PredefinedMenuItem::separator(app)?)?;
        for n in snap.nodes.iter().take(40) {
            let checked = !smart && snap.selected.as_deref() == Some(n.node.id.as_str());
            let text = menu_text(&format!("{}  ·  {}", n.node.name, n.summary));
            sub.append(&CheckMenuItem::with_id(app, format!("node:{}", n.node.id), text, true, checked, None::<&str>)?)?;
        }
        menu.append(&sub)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", l.quit, true, None::<&str>)?)?;
    Ok(menu)
}

/// Update tooltip; rebuild the menu only when something it shows changed.
async fn refresh_tray(app: &AppHandle, ctl: &Controller, last: &str) -> String {
    let snap = ctl.snapshot().await;
    let selected = snap.selected.clone().unwrap_or_default();
    let sel_name = snap.nodes.iter().find(|n| n.node.id == selected).map(|n| n.node.name.clone()).unwrap_or_default();
    let sig = format!(
        "{}|{}|{}|{}|{}",
        snap.settings.smart_connect,
        snap.status.connected,
        selected,
        snap.settings.ui_language,
        snap.nodes.iter().map(|n| format!("{}:{}", n.node.id, n.node.name)).collect::<Vec<_>>().join(",")
    );
    if let Some(tray) = app.tray_by_id("main") {
        let l = labels(&snap.settings.ui_language);
        let state = if snap.status.connected { l.on } else { l.off };
        let mut tip = format!("Mosaic — {state}");
        if !sel_name.is_empty() {
            tip.push_str(" · ");
            tip.push_str(&sel_name);
        }
        let tip: String = tip.chars().take(120).collect();
        let _ = tray.set_tooltip(Some(tip));
        if sig != last {
            if let Ok(menu) = build_menu(app, &snap) {
                let _ = tray.set_menu(Some(menu));
            }
        }
    }
    sig
}

fn on_menu(app: &AppHandle, id: &str) {
    let ctl = app.state::<Arc<Controller>>().inner().clone();
    match id {
        "show" => show_main(app),
        "quit" => app.exit(0),
        "auto" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let on = !ctl.settings().await.smart_connect;
                ctl.set_smart(on).await;
                refresh_tray(&app, &ctl, "").await;
            });
        }
        "toggle" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if ctl.is_connected().await {
                    ctl.disconnect().await;
                } else if let Err(e) = ctl.connect().await {
                    ctl.logs.push(format!("[tray] connect failed: {e}"));
                    show_main(&app);
                }
                refresh_tray(&app, &ctl, "").await;
            });
        }
        other => {
            if let Some(node) = other.strip_prefix("node:") {
                let (node, app) = (node.to_string(), app.clone());
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = ctl.select_node(&node).await {
                        ctl.logs.push(format!("[tray] {e}"));
                    }
                    refresh_tray(&app, &ctl, "").await;
                });
            }
        }
    }
}

// ------------------------------------------------------------------ main

fn main() {
    // Another Mosaic is already running: it shows its window, we quit.
    let Some(instance) = single::acquire() else { return };
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
            set_smart,
            check_engine_updates,
            update_engine,
            reset_engines,
            check_app_update,
            install_app_update,
            test_latency,
            connections,
            logs,
            engine_versions
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let for_second_launch = handle.clone();
            single::listen(instance, move || show_main(&for_second_launch));
            // Autostart passes --minimized: stay in the tray.
            if !std::env::args().any(|a| a == "--minimized") {
                show_main(&handle);
            }
            let icon = app.default_window_icon().cloned().ok_or("the app has no icon")?;
            TrayIconBuilder::with_id("main")
                .icon(icon)
                .tooltip("Mosaic")
                .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::DoubleClick { .. } = event {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            let ctl = app.state::<Arc<Controller>>().inner().clone();
            // Smart Connect + scheduled subscription refresh.
            tauri::async_runtime::spawn(ctl.clone().run_background());
            tauri::async_runtime::spawn(async move {
                if ctl.settings().await.auto_connect {
                    ctl.prepare_startup().await;
                    if let Err(e) = ctl.connect().await {
                        ctl.logs.push(format!("[app] auto-connect failed: {e}"));
                        show_main(&handle);
                    }
                }
                let mut last = String::new();
                loop {
                    last = refresh_tray(&handle, &ctl, &last).await;
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let ctl = window.state::<Arc<Controller>>();
                if window.label() == "main" && ctl.close_to_tray.load(Relaxed) {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to start Mosaic")
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                // Restore the system proxy, remove WFP filters and stop engines.
                tauri::async_runtime::block_on(on_exit.disconnect());
            }
        });
}
