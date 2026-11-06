//! Renders the dashboard.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
    Frame,
};

use crate::{
    app::App,
    summary::{format_zec, group_thousands, BlockSummary},
    upgrades::UpgradeSchedule,
};

/// Draws the whole dashboard into `frame`.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let [top, logs, status] = Layout::vertical([
        Constraint::Length(14),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [block_area, pools_area, mempool_area] = Layout::horizontal([
        Constraint::Length(72),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .areas(top);

    draw_block(frame, block_area, app.block.as_ref(), &app.upgrades);
    draw_pools(frame, pools_area, app.block.as_ref());
    draw_mempool(frame, mempool_area, app);
    draw_logs(frame, logs, app);
    draw_status(frame, status, app);
    if app.show_peers {
        draw_peers(frame, app);
    }
}

fn titled(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title.into().bold())
}

fn draw_block(
    frame: &mut Frame,
    area: Rect,
    block: Option<&BlockSummary>,
    upgrades: &UpgradeSchedule,
) {
    let Some(block) = block else {
        frame.render_widget(
            Paragraph::new("waiting for the node…").block(titled(" Latest block ")),
            area,
        );
        return;
    };

    let header = vec![
        Line::from(vec!["hash ".dim(), Span::raw(block.hash.clone())]),
        Line::from(vec![
            "time ".dim(),
            Span::raw(format_time(block.time)),
            format!("  ({})", format_age(block.time)).dim(),
        ]),
        Line::from(vec![
            "size ".dim(),
            Span::raw(format!("{} bytes", group_thousands(block.size_bytes))),
            "   txs ".dim(),
            Span::raw(format!(
                "{} ({} coinbase)",
                block.transaction_count, block.coinbase_count
            )),
        ]),
        upgrade_line(upgrades, block.height),
    ];

    let count =
        |value: usize| Cell::from(group_thousands(u64::try_from(value).unwrap_or(u64::MAX)));
    let rows = vec![
        Row::new(vec![
            Cell::from("transparent"),
            count(block.transparent.inputs),
            count(block.transparent.outputs),
            Cell::from("coins"),
        ]),
        Row::new(vec![
            Cell::from("sprout"),
            count(block.sprout.inputs),
            count(block.sprout.outputs),
            Cell::from(format!("notes, {} joinsplits", block.sprout_joinsplits)),
        ]),
        Row::new(vec![
            Cell::from("sapling"),
            count(block.sapling.inputs),
            count(block.sapling.outputs),
            Cell::from("notes"),
        ]),
        Row::new(vec![
            Cell::from("orchard"),
            count(block.orchard_actions),
            count(block.orchard_actions),
            Cell::from("actions"),
        ]),
        Row::new(vec![
            Cell::from("ironwood"),
            count(block.ironwood_actions),
            count(block.ironwood_actions),
            Cell::from("actions"),
        ]),
    ];

    let outer = titled(format!(
        " Block {} ",
        group_thousands(u64::from(block.height))
    ));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    let [header_area, table_area] =
        Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).areas(inner);
    frame.render_widget(Paragraph::new(header), header_area);
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(12),
                Constraint::Length(8),
                Constraint::Length(8),
                Constraint::Min(0),
            ],
        )
        .header(
            Row::new(vec!["pool", "inputs", "outputs", "unit"])
                .style(Style::new().add_modifier(Modifier::UNDERLINED)),
        ),
        table_area,
    );
}

/// Describes the upgrade active at `height` and the next scheduled upgrade.
fn upgrade_line(upgrades: &UpgradeSchedule, height: u32) -> Line<'static> {
    let current = upgrades.current.clone().unwrap_or_else(|| "?".to_owned());
    let next = match &upgrades.next {
        Some(next) => vec![
            "   next ".dim(),
            Span::styled(
                format!(
                    "{} at {}",
                    next.name,
                    group_thousands(u64::from(next.activation_height))
                ),
                Style::new().fg(Color::Yellow),
            ),
            format!(
                " ({} blocks, ~{})",
                group_thousands(u64::from(next.blocks_remaining(height))),
                format_duration(
                    i64::try_from(next.estimated_seconds_remaining(height)).unwrap_or(i64::MAX)
                )
            )
            .dim(),
        ],
        None => vec!["   no upgrade scheduled".dim()],
    };
    Line::from([vec!["upgrade ".dim(), Span::raw(current)], next].concat())
}

fn draw_pools(frame: &mut Frame, area: Rect, block: Option<&BlockSummary>) {
    let outer = titled(" Value pools (ZEC) ");
    let Some(block) = block else {
        frame.render_widget(outer, area);
        return;
    };

    let delta_cell = |delta: Option<i64>| match delta {
        Some(zat) => Cell::from(format_zec(zat, true)).style(delta_style(zat)),
        None => Cell::from("?").dim(),
    };
    let mut rows: Vec<Row> = block
        .pools
        .iter()
        .map(|pool| {
            Row::new(vec![
                Cell::from(pool.id.clone()),
                delta_cell(pool.delta_zat),
                Cell::from(format_zec(pool.balance_zat, false)),
            ])
        })
        .collect();
    let total_balance = block
        .pools
        .iter()
        .try_fold(0i64, |total, pool| total.checked_add(pool.balance_zat));
    rows.push(
        Row::new(vec![
            Cell::from("supply"),
            delta_cell(block.net_issuance_zat()),
            Cell::from(total_balance.map_or_else(|| "?".to_owned(), |zat| format_zec(zat, false))),
        ])
        .add_modifier(Modifier::BOLD),
    );

    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(12),
                Constraint::Length(18),
                Constraint::Min(0),
            ],
        )
        .header(
            Row::new(vec!["pool", "Δ this block", "balance"])
                .style(Style::new().add_modifier(Modifier::UNDERLINED)),
        )
        .block(outer),
        area,
    );
}

fn draw_mempool(frame: &mut Frame, area: Rect, app: &App) {
    let outer = titled(" Mempool ");
    let Some(mempool) = &app.mempool else {
        frame.render_widget(Paragraph::new("waiting for the node…").block(outer), area);
        return;
    };

    let label = |text: &'static str| Span::from(format!("{text:<14}")).dim();
    let bytes = |value: u64| format!("{} bytes", group_thousands(value));
    let mut lines = vec![
        Line::from(vec![
            label("transactions"),
            Span::raw(group_thousands(
                u64::try_from(mempool.transaction_count).unwrap_or(u64::MAX),
            )),
            format!(
                "  ({} spend mempool outputs)",
                group_thousands(u64::try_from(mempool.dependent_count).unwrap_or(u64::MAX))
            )
            .dim(),
        ]),
        Line::from(vec![
            label("size"),
            Span::raw(bytes(mempool.bytes)),
            match mempool.blocks_to_clear() {
                1 => "  (~1 full block)".to_owned(),
                blocks => format!("  (~{blocks} full blocks)"),
            }
            .dim(),
        ]),
        Line::from(vec![
            label("memory"),
            Span::raw(bytes(mempool.memory_usage_bytes)),
        ]),
        Line::from(vec![
            label("total fees"),
            Span::raw(format!("{} ZEC", format_zec(mempool.total_fee_zat, false))),
        ]),
        Line::from(vec![
            label("fee rate"),
            Span::raw(mempool.fee_rate.map_or_else(
                || "-".to_owned(),
                |rate| {
                    format!(
                        "{} / {} / {}",
                        group_thousands(rate.min),
                        group_thousands(rate.median),
                        group_thousands(rate.max)
                    )
                },
            )),
            "  zat/kB min/median/max".dim(),
        ]),
        Line::from(vec![
            label("oldest"),
            Span::raw(
                mempool
                    .oldest_entry_time
                    .map_or_else(|| "-".to_owned(), format_age),
            ),
        ]),
    ];
    let syncing = matches!(
        (app.tip_height, app.estimated_height),
        (Some(tip), Some(estimate)) if estimate > tip
    );
    if mempool.transaction_count == 0 && syncing {
        lines.push(Line::raw(""));
        lines.push(Line::from(
            "zebrad turns on its mempool near the chain tip.".dim(),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(outer),
        area,
    );
}

fn delta_style(zat: i64) -> Style {
    match zat.signum() {
        1 => Style::new().fg(Color::Green),
        -1 => Style::new().fg(Color::Red),
        _ => Style::new().add_modifier(Modifier::DIM),
    }
}

fn draw_logs(frame: &mut Frame, area: Rect, app: &App) {
    let title = match (&app.log_source, app.log_scroll_back) {
        (None, _) => " Log ".to_owned(),
        (Some(path), 0) => format!(" Log: {path} "),
        (Some(path), back) => format!(" Log: {path} (paused, {back} newer lines) "),
    };
    let outer = titled(title);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let message = match (&app.log_source, &app.log_error) {
        (None, _) => Some(
            "No log file. Set `tracing.log_file` in zebrad.toml, restart zebrad, \
             and pass the same path with --log-file."
                .to_owned(),
        ),
        (Some(_), Some(error)) => Some(format!("cannot read log file: {error}")),
        _ => None,
    };
    if let Some(message) = message {
        frame.render_widget(
            Paragraph::new(message).wrap(Wrap { trim: true }).dim(),
            inner,
        );
        return;
    }

    let height = usize::from(inner.height);
    let end = app.log_lines.len().saturating_sub(app.log_scroll_back);
    let start = end.saturating_sub(height);
    let lines: Vec<Line> = app
        .log_lines
        .range(start..end)
        .map(|line| styled_log_line(line))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Colors a tracing log line by its level.
fn styled_log_line(line: &str) -> Line<'_> {
    let color = [
        (" ERROR ", Color::Red),
        (" WARN ", Color::Yellow),
        (" INFO ", Color::Green),
        (" DEBUG ", Color::Blue),
        (" TRACE ", Color::Magenta),
    ]
    .into_iter()
    .find_map(|(level, color)| line.find(level).map(|index| (index, level.len(), color)));

    match color {
        Some((index, length, color)) => Line::from(vec![
            Span::raw(&line[..index]).dim(),
            Span::styled(&line[index..index + length], Style::new().fg(color)),
            Span::raw(&line[index + length..]),
        ]),
        None => Line::raw(line),
    }
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let sync = match (app.tip_height, app.estimated_height) {
        (Some(tip), Some(estimate)) if estimate > tip => format!(
            "tip {} / ~{} (syncing, {} behind)",
            group_thousands(u64::from(tip)),
            group_thousands(u64::from(estimate)),
            group_thousands(u64::from(estimate - tip)),
        ),
        (Some(tip), _) => format!("tip {}", group_thousands(u64::from(tip))),
        (None, _) => "tip unknown".to_owned(),
    };
    let connection = match &app.rpc_error {
        Some(error) => Span::styled(format!(" {} ", error), Style::new().fg(Color::Red)),
        None => Span::styled(format!(" {} ", app.rpc_url), Style::new().fg(Color::Green)),
    };
    let line = Line::from(vec![
        connection,
        Span::raw(format!(" {sync} ")),
        " [p] peers  [↑↓ PgUp PgDn] scroll  [End] follow  [q] quit".dim(),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_peers(frame: &mut Frame, app: &mut App) {
    let area = centered(frame.area(), 90, 70);
    frame.render_widget(Clear, area);

    let title = match app.peers_updated {
        Some(_) => format!(
            " Peers ({}) — [↑↓ PgUp PgDn Home End] move  [p] close ",
            app.peers.len()
        ),
        None => " Peers — loading… [p] close ".to_owned(),
    };
    let now = unix_now();
    let rows = app.peers.iter().map(|peer| {
        Row::new(vec![
            Cell::from(peer.addr.clone()),
            Cell::from(if peer.inbound { "in" } else { "out" }),
            Cell::from(group_thousands(peer.startingheight.max(0).unsigned_abs())),
            Cell::from(
                peer.pingtime
                    .map_or_else(|| "-".to_owned(), |s| format!("{:.0}", s * 1000.0)),
            ),
            Cell::from(format_duration(now.saturating_sub(peer.lastrecv))),
            Cell::from(peer.version.to_string()),
            Cell::from(peer.subver.clone()),
            Cell::from(peer.connection_state.clone()),
        ])
    });
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                // "[" + 39-character IPv6 address + "]:" + 5-digit port
                Constraint::Length(47),
                Constraint::Length(4),
                Constraint::Length(10),
                Constraint::Length(8),
                Constraint::Length(9),
                Constraint::Length(7),
                Constraint::Min(14),
                Constraint::Length(10),
            ],
        )
        .header(
            Row::new(vec![
                "address",
                "dir",
                "start ht",
                "ping ms",
                "last recv",
                "proto",
                "agent",
                "state",
            ])
            .style(Style::new().add_modifier(Modifier::UNDERLINED)),
        )
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .block(titled(title).border_style(Style::new().fg(Color::Cyan))),
        area,
        &mut app.peer_table,
    );
}

/// Returns a rectangle of `percent_x` by `percent_y` of `area`, centered in it.
fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let [area] = Layout::vertical([Constraint::Percentage(percent_y)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

fn format_time(unix_seconds: i64) -> String {
    chrono::DateTime::from_timestamp(unix_seconds, 0).map_or_else(
        || unix_seconds.to_string(),
        |time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
    )
}

fn format_age(unix_seconds: i64) -> String {
    let age = unix_now().saturating_sub(unix_seconds);
    if age < 0 {
        "in the future".to_owned()
    } else {
        format!("{} ago", format_duration(age))
    }
}

/// Formats a non-negative number of seconds in its largest whole unit.
fn format_duration(seconds: i64) -> String {
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations_in_largest_unit() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(59), "59s");
        assert_eq!(format_duration(60), "1m");
        assert_eq!(format_duration(7_200), "2h");
        assert_eq!(format_duration(172_800), "2d");
    }

    #[test]
    fn colors_only_the_level_token() {
        let line = styled_log_line("2026-11-05T01:02:03Z  WARN zebra_network: slow peer");
        let texts: Vec<&str> = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(
            texts,
            vec![
                "2026-11-05T01:02:03Z ",
                " WARN ",
                "zebra_network: slow peer"
            ]
        );
    }
}
