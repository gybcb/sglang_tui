use std::sync::{Arc, RwLock};
use std::time::Duration;

use clap::Parser;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use sgtop::app::App;
use sgtop::config::{Cli, Config};
use sgtop::model::snapshot::Snapshot;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::build(&cli);
    if cli.print_config {
        print!("{}", toml::to_string_pretty(&config)?);
        return Ok(());
    }

    let snapshot = Arc::new(RwLock::new(Snapshot::default()));
    let app = App::new(config.clone(), snapshot.clone());
    run(app, snapshot).await
}

async fn run(mut app: App, snapshot: Arc<RwLock<Snapshot>>) -> anyhow::Result<()> {
    let mut term: DefaultTerminal = ratatui::init();

    // Bounded so a slow UI backpressures the poller rather than queueing stale samples.
    let (tx, mut rx) = mpsc::channel::<sgtop::poll::Tick>(8);
    let cfg = app.config.clone();
    let control = app.control.clone();
    let handle = tokio::spawn(sgtop::poll::run(cfg, control, snapshot.clone(), tx));

    let mut events = EventStream::new();
    let mut draw = tokio::time::interval(Duration::from_millis(250));
    draw.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut fatal = None;
    loop {
        tokio::select! {
            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(Event::Key(k))) if k.kind == KeyEventKind::Press => {
                        if app.on_key(k) { break; }
                    }
                    Some(Ok(Event::Resize(..))) => {}
                    Some(Ok(Event::Mouse(_))) => {}
                    _ => {}
                }
            }
            _ = draw.tick() => {}
            Some(_tick) = rx.recv() => {
                // The poller parks MetricsDisabled on a fatal 404; surface it.
                if let Some(err) = App::fatal_error(&snapshot) {
                    fatal = Some(err);
                    break;
                }
            }
        }
        term.draw(|f| app.draw(f))?;
    }

    handle.abort();
    ratatui::restore();
    if let Some(err) = fatal {
        eprintln!("{err}");
        std::process::exit(1);
    }
    Ok(())
}
