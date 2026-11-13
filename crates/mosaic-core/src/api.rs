//! The UI commands behind one JSON entry point: `dispatch("select_node", {"id": "…"})`.
//!
//! The desktop app calls the controller through Tauri commands; the Android app
//! (WebView + JNI) and future front-ends use this. Command names and argument
//! names match the Tauri ones, so the same `app.js` works everywhere.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::model::{EngineChoice, EngineKind, Rule, Settings};
use crate::Controller;

fn arg<T: DeserializeOwned>(args: &Value, names: &[&str]) -> Result<T, String> {
    for n in names {
        if let Some(v) = args.get(*n) {
            return serde_json::from_value(v.clone()).map_err(|e| format!("argument '{n}': {e}"));
        }
    }
    Err(format!("missing argument '{}'", names[0]))
}

fn json<T: Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

pub async fn dispatch(ctl: &Arc<Controller>, cmd: &str, args: &Value) -> Result<Value, String> {
    match cmd {
        "snapshot" => json(ctl.snapshot().await),
        "progress" => json(ctl.progress()),
        "connect" => json(ctl.connect().await?),
        "disconnect" => json(ctl.disconnect().await),
        "import_text" => json(ctl.import_text(&arg::<String>(args, &["text"])?).await),
        "add_subscription" => json(ctl.add_subscription(&arg::<String>(args, &["url"])?).await?),
        "update_subscriptions" => json(ctl.update_all_subscriptions().await),
        "update_subscription" => json(ctl.update_subscription(&arg::<String>(args, &["url"])?).await?),
        "remove_subscription" => json(ctl.remove_subscription(&arg::<String>(args, &["url"])?).await),
        "remove_node" => json(ctl.remove_node(&arg::<String>(args, &["id"])?).await),
        "select_node" => json(ctl.select_node(&arg::<String>(args, &["id"])?).await?),
        "update_node" => json(ctl.update_node(arg::<crate::model::Node>(args, &["node"])?).await?),
        "set_node_engine" => {
            let id: String = arg(args, &["id"])?;
            let engine: EngineChoice = arg(args, &["engine"])?;
            json(ctl.set_node_engine(&id, engine).await?)
        }
        "set_rules" => {
            let rules: Vec<Rule> = arg(args, &["rules"])?;
            let final_target: String = arg(args, &["finalTarget", "final_target"])?;
            json(ctl.set_rules(rules, final_target).await?)
        }
        "set_settings" => json(ctl.set_settings(arg::<Settings>(args, &["settings"])?).await?),
        "release_kill_switch" => json(ctl.release_kill_switch().await?),
        "set_smart" => json(ctl.set_smart(arg::<bool>(args, &["on"])?).await),
        "test_latency" => {
            let ids: Option<Vec<String>> = arg(args, &["ids"]).ok().flatten();
            json(ctl.test_nodes(ids.as_deref()).await?)
        }
        "connections" => json(ctl.connections()),
        "logs" => json(ctl.logs()),
        "log_entries" => json(ctl.log_entries(arg::<u64>(args, &["since"]).unwrap_or(0))),
        "diagnostics" => json(serde_json::json!({ "text": ctl.diagnostics().await, "path": null })),
        "engine_versions" => json(ctl.engine_versions().await),
        "check_engine_updates" => json(ctl.check_engine_updates().await),
        "update_engine" => json(ctl.update_engine(arg::<EngineKind>(args, &["kind"])?).await?),
        "reset_engines" => json(ctl.reset_engines().await?),
        "check_app_update" => json(ctl.check_app_update().await),
        other => Err(format!("unknown command '{other}'")),
    }
}
