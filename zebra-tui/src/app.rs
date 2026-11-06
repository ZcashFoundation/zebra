//! Dashboard state and the updates that change it.

use std::{collections::VecDeque, time::SystemTime};

use ratatui::widgets::TableState;

use crate::{
    mempool::MempoolSummary, rpc::RpcPeer, summary::BlockSummary, upgrades::UpgradeSchedule,
};

/// The maximum number of log lines that the dashboard keeps.
const LOG_CAPACITY: usize = 20_000;

/// A change reported by a background worker.
pub enum Update {
    /// The node's chain tip height, the estimated network height, and the upgrade schedule.
    Tip {
        height: u32,
        estimated_height: Option<u32>,
        upgrades: UpgradeSchedule,
    },
    /// Statistics for the newest block.
    Block(Box<BlockSummary>),
    /// Statistics for the node's mempool.
    Mempool(MempoolSummary),
    /// The node's current peers.
    Peers(Vec<RpcPeer>),
    /// The latest node RPC call failed.
    RpcFailed(String),
    /// Lines appended to the log file.
    LogLines(Vec<String>),
    /// The log file could not be read.
    LogFailed(String),
}

/// Everything that the dashboard displays.
pub struct App {
    pub rpc_url: String,
    pub log_source: Option<String>,
    pub tip_height: Option<u32>,
    pub estimated_height: Option<u32>,
    pub upgrades: UpgradeSchedule,
    pub block: Option<BlockSummary>,
    pub mempool: Option<MempoolSummary>,
    pub peers: Vec<RpcPeer>,
    pub peers_updated: Option<SystemTime>,
    pub rpc_error: Option<String>,
    pub log_error: Option<String>,
    pub log_lines: VecDeque<String>,
    /// Lines between the bottom of the log pane and the newest line. Zero follows new lines.
    pub log_scroll_back: usize,
    pub show_peers: bool,
    /// The highlighted row and scroll position of the peer list.
    pub peer_table: TableState,
    pub should_quit: bool,
}

impl App {
    /// Creates an empty dashboard for the node at `rpc_url`.
    pub fn new(rpc_url: String, log_source: Option<String>) -> Self {
        Self {
            rpc_url,
            log_source,
            tip_height: None,
            estimated_height: None,
            upgrades: UpgradeSchedule::default(),
            block: None,
            mempool: None,
            peers: Vec::new(),
            peers_updated: None,
            rpc_error: None,
            log_error: None,
            log_lines: VecDeque::new(),
            log_scroll_back: 0,
            show_peers: false,
            peer_table: TableState::new().with_selected(Some(0)),
            should_quit: false,
        }
    }

    /// Applies a worker update.
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Tip {
                height,
                estimated_height,
                upgrades,
            } => {
                self.tip_height = Some(height);
                self.estimated_height = estimated_height;
                self.upgrades = upgrades;
                self.rpc_error = None;
            }
            Update::Block(summary) => {
                self.block = Some(*summary);
                self.rpc_error = None;
            }
            Update::Mempool(summary) => self.mempool = Some(summary),
            Update::Peers(mut peers) => {
                peers.sort_by_key(|peer| std::cmp::Reverse(peer.startingheight));
                let selected_addr = self
                    .peer_table
                    .selected()
                    .and_then(|index| self.peers.get(index))
                    .map(|peer| peer.addr.clone());
                if let Some(index) =
                    selected_addr.and_then(|addr| peers.iter().position(|peer| peer.addr == addr))
                {
                    self.peer_table.select(Some(index));
                }
                self.peers = peers;
                self.peers_updated = Some(SystemTime::now());
            }
            Update::RpcFailed(message) => self.rpc_error = Some(message),
            Update::LogLines(lines) => {
                let added = lines.len();
                self.log_lines.extend(lines);
                let excess = self.log_lines.len().saturating_sub(LOG_CAPACITY);
                self.log_lines.drain(..excess);
                if self.log_scroll_back > 0 {
                    self.log_scroll_back = (self.log_scroll_back + added).min(self.log_lines.len());
                }
                self.log_error = None;
            }
            Update::LogFailed(message) => self.log_error = Some(message),
        }
    }

    /// Moves the log view `lines` toward older lines. Negative values move toward newer lines.
    pub fn scroll_log(&mut self, lines: isize) {
        self.log_scroll_back = self
            .log_scroll_back
            .saturating_add_signed(lines)
            .min(self.log_lines.len());
    }

    /// Moves the peer list highlight `rows` down. Negative values move it up.
    ///
    /// The highlight stops at the first and last peer.
    pub fn move_peer_selection(&mut self, rows: isize) {
        let last = self.peers.len().saturating_sub(1);
        let selected = self
            .peer_table
            .selected()
            .unwrap_or(0)
            .min(last)
            .saturating_add_signed(rows)
            .min(last);
        self.peer_table.select(Some(selected));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolled_log_view_stays_on_the_same_lines_when_lines_arrive() {
        let mut app = App::new(String::new(), None);
        app.apply(Update::LogLines(vec!["a".into(), "b".into(), "c".into()]));
        app.scroll_log(1);
        app.apply(Update::LogLines(vec!["d".into()]));
        assert_eq!(app.log_scroll_back, 2);
    }

    #[test]
    fn following_log_view_keeps_following() {
        let mut app = App::new(String::new(), None);
        app.apply(Update::LogLines(vec!["a".into()]));
        app.apply(Update::LogLines(vec!["b".into()]));
        assert_eq!(app.log_scroll_back, 0);
    }

    fn peer(addr: &str, height: i64) -> RpcPeer {
        serde_json::from_value(serde_json::json!({"addr": addr, "startingheight": height}))
            .expect("test peer is valid getpeerinfo JSON")
    }

    #[test]
    fn peer_highlight_follows_the_same_peer_after_a_refresh() {
        let mut app = App::new(String::new(), None);
        app.apply(Update::Peers(vec![
            peer("a", 3),
            peer("b", 2),
            peer("c", 1),
        ]));
        app.move_peer_selection(1);
        assert_eq!(app.peers[app.peer_table.selected().unwrap_or(0)].addr, "b");

        app.apply(Update::Peers(vec![
            peer("a", 3),
            peer("c", 9),
            peer("b", 2),
        ]));
        assert_eq!(app.peers[app.peer_table.selected().unwrap_or(0)].addr, "b");
    }

    #[test]
    fn peer_highlight_stops_at_the_ends_of_the_list() {
        let mut app = App::new(String::new(), None);
        app.apply(Update::Peers(vec![peer("a", 2), peer("b", 1)]));
        app.move_peer_selection(10);
        assert_eq!(app.peer_table.selected(), Some(1));
        app.move_peer_selection(-1);
        assert_eq!(app.peer_table.selected(), Some(0));
        app.move_peer_selection(-10);
        assert_eq!(app.peer_table.selected(), Some(0));
    }

    #[test]
    fn scrolling_is_bounded_by_the_log_length() {
        let mut app = App::new(String::new(), None);
        app.apply(Update::LogLines(vec!["a".into(), "b".into()]));
        app.scroll_log(10);
        assert_eq!(app.log_scroll_back, 2);
        app.scroll_log(-10);
        assert_eq!(app.log_scroll_back, 0);
    }
}
