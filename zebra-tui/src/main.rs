//! A terminal dashboard for a running zebrad node.
//!
//! It shows the node's log, statistics for the newest block and the mempool, and the
//! value pool balances. Press `p` to show the connected peers.

mod app;
mod logs;
mod mempool;
mod rpc;
mod summary;
mod ui;
mod upgrades;
mod workers;

use std::{
    path::PathBuf,
    sync::{atomic::AtomicBool, atomic::Ordering, mpsc, Arc},
    time::Duration,
};

use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::{
    app::{App, Update},
    logs::LogTail,
    rpc::NodeRpc,
};

/// How long the interface waits for a key press before it redraws.
const FRAME_INTERVAL: Duration = Duration::from_millis(200);

/// The number of log lines that PgUp and PgDn move.
const PAGE_LINES: isize = 20;

/// Terminal dashboard for a running zebrad node.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// The node's JSON-RPC endpoint.
    #[arg(long, default_value = "http://127.0.0.1:8232")]
    rpc_url: String,

    /// The node's RPC cookie file, if the node requires cookie authentication.
    #[arg(long)]
    cookie_file: Option<PathBuf>,

    /// The log file that zebrad writes, as set by `tracing.log_file`.
    #[arg(long)]
    log_file: Option<PathBuf>,

    /// Seconds between polls of the node.
    #[arg(long, default_value_t = 2.0)]
    poll_seconds: f64,
}

/// An error that stops the dashboard.
#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Rpc(#[from] rpc::RpcError),

    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),

    #[error("--poll-seconds must be a positive number of seconds")]
    InvalidPollInterval,
}

fn main() -> Result<(), Error> {
    let args = Args::parse();
    let interval = Duration::try_from_secs_f64(args.poll_seconds)
        .ok()
        .filter(|interval| !interval.is_zero())
        .ok_or(Error::InvalidPollInterval)?;
    let rpc = NodeRpc::new(
        args.rpc_url.clone(),
        args.cookie_file.as_deref(),
        Duration::from_secs(30),
    )?;

    let (sender, receiver) = mpsc::channel();
    let peers_wanted = Arc::new(AtomicBool::new(false));
    workers::spawn_node_poller(rpc, interval, peers_wanted.clone(), sender.clone());
    if let Some(path) = &args.log_file {
        workers::spawn_log_follower(
            LogTail::new(path.clone()),
            Duration::from_millis(250),
            sender,
        );
    }

    let mut app = App::new(
        args.rpc_url,
        args.log_file.map(|path| path.display().to_string()),
    );
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &receiver, &peers_wanted);
    ratatui::restore();
    result
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    updates: &mpsc::Receiver<Update>,
    peers_wanted: &AtomicBool,
) -> Result<(), Error> {
    while !app.should_quit {
        for update in updates.try_iter() {
            app.apply(update);
        }
        terminal.draw(|frame| ui::draw(frame, app))?;
        if event::poll(FRAME_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(app, key.code, key.modifiers);
                    peers_wanted.store(app.show_peers, Ordering::Relaxed);
                }
            }
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => app.should_quit = true,
        KeyCode::Esc if app.show_peers => app.show_peers = false,
        KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('p') => app.show_peers = !app.show_peers,
        KeyCode::Up | KeyCode::Char('k') => app.scroll_log(1),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_log(-1),
        KeyCode::PageUp => app.scroll_log(PAGE_LINES),
        KeyCode::PageDown => app.scroll_log(-PAGE_LINES),
        KeyCode::Home => app.scroll_log(isize::MAX),
        KeyCode::End => app.log_scroll_back = 0,
        _ => {}
    }
}
