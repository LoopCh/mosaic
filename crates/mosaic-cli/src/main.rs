//! mosaic-cli — headless front-end for debugging the core without the UI.

use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use mosaic_core::model::{EngineChoice, Mode, TunStack};
use mosaic_core::Controller;

const HELP: &str = "mosaic-cli — Mosaic without the UI

  import <link | file>       add nodes (share links, base64 subscription, WireGuard .conf)
  sub <https://…>            add a subscription
  update                     refresh all subscriptions
  list                       list nodes
  select <n>                 select the active node
  engine <n> <auto|xray|singbox>   choose the engine for a node
  test                       latency test for all nodes
  run [--tun [--native]]     connect and run until Ctrl+C
  config                     where settings and generated configs live
";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ctl = Controller::load();
    let code = match run(&ctl, &args).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    };
    std::process::exit(code);
}

async fn node_id_by_index(ctl: &Controller, idx: &str) -> Result<String, String> {
    let i: usize = idx.parse().map_err(|_| "expected a node number from `list`")?;
    let snap = ctl.snapshot().await;
    snap.nodes.get(i.wrapping_sub(1)).map(|n| n.node.id.clone()).ok_or_else(|| format!("no node #{i}"))
}

async fn run(ctl: &Controller, args: &[String]) -> Result<(), String> {
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "import" => {
            let arg = args.get(1).ok_or("import <link | file>")?;
            let text = match std::fs::read_to_string(arg) {
                Ok(t) => t,
                Err(_) => arg.clone(),
            };
            let s = ctl.import_text(&text).await;
            println!("added: {}, updated: {}", s.added, s.updated);
            for e in s.errors {
                println!("  skipped: {e}");
            }
        }
        "sub" => {
            let url = args.get(1).ok_or("sub <url>")?;
            let s = ctl.add_subscription(url).await?;
            println!("subscription: added {}, updated {}, skipped {}", s.added, s.updated, s.errors.len());
        }
        "update" => {
            let s = ctl.update_all_subscriptions().await;
            println!("added {}, updated {}", s.added, s.updated);
            for e in s.errors {
                println!("  {e}");
            }
        }
        "list" => {
            let snap = ctl.snapshot().await;
            if snap.nodes.is_empty() {
                println!("no nodes — mosaic-cli import <link>");
            }
            for (i, n) in snap.nodes.iter().enumerate() {
                let mark = if snap.selected.as_deref() == Some(n.node.id.as_str()) { "*" } else { " " };
                let engine = match (&n.effective, &n.effective_error) {
                    (Some(k), _) => k.label().to_string(),
                    (None, Some(e)) => format!("— {e}"),
                    _ => "—".into(),
                };
                let lat = match &n.latency {
                    Some(l) => match (l.ms, &l.error) {
                        (Some(ms), _) => format!("{ms} ms"),
                        (None, Some(e)) => format!("error: {e}"),
                        _ => String::new(),
                    },
                    None => String::new(),
                };
                println!("{mark}{:>3}. {:<28} {:<32} [{engine}] {lat}", i + 1, n.node.name, n.summary);
            }
            for e in &snap.engines {
                println!("engine {}: {}", e.name, e.path.clone().unwrap_or_else(|| "NOT FOUND".into()));
            }
        }
        "select" => {
            let id = node_id_by_index(ctl, args.get(1).ok_or("select <n>")?).await?;
            ctl.select_node(&id).await?;
            println!("ok");
        }
        "engine" => {
            let id = node_id_by_index(ctl, args.get(1).ok_or("engine <n> <auto|xray|singbox>")?).await?;
            let choice = match args.get(2).map(String::as_str) {
                Some("auto") => EngineChoice::Auto,
                Some("xray") => EngineChoice::Xray,
                Some("singbox") | Some("sing-box") => EngineChoice::Singbox,
                _ => return Err("engine: auto | xray | singbox".into()),
            };
            ctl.set_node_engine(&id, choice).await?;
            println!("ok");
        }
        "test" => {
            ctl.logs.set_echo(true);
            ctl.test_latency().await?;
            ctl.logs.set_echo(false);
            Box::pin(run(ctl, &["list".to_string()])).await?;
        }
        "run" => {
            ctl.logs.set_echo(true);
            let mut settings = ctl.snapshot().await.settings;
            settings.mode = if args.iter().any(|a| a == "--tun") { Mode::Tun } else { Mode::SystemProxy };
            settings.tun_stack = if args.iter().any(|a| a == "--native") { TunStack::Native } else { TunStack::Singbox };
            ctl.set_settings(settings).await?;
            ctl.connect().await?;
            println!("connected, Ctrl+C to quit");
            let mut last = (0u64, 0u64);
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {
                        let up = ctl.stats.up.load(Relaxed);
                        let down = ctl.stats.down.load(Relaxed);
                        println!("↑ {:.1} KB/s  ↓ {:.1} KB/s  connections: {}",
                            (up - last.0) as f64 / 5120.0, (down - last.1) as f64 / 5120.0, ctl.stats.active_count());
                        last = (up, down);
                    }
                }
            }
            ctl.disconnect().await;
            println!("disconnected");
        }
        "config" => {
            let snap = ctl.snapshot().await;
            println!("settings: {}", std::path::Path::new(&snap.data_dir).join("state.json").display());
            println!("engine configs: {}", std::path::Path::new(&snap.data_dir).join("run").display());
        }
        _ => print!("{HELP}"),
    }
    Ok(())
}
