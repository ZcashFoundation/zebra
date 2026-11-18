//! Checks the Zebra book's hand-written reference tables against the code.
//!
//! The configuration reference and the network upgrade table are copied from the code by hand.
//! These tests fail when the code accepts a configuration key or changes an activation height
//! without the matching book update.

use std::collections::BTreeMap;

use zebra_chain::parameters::{Network, NetworkUpgrade};
use zebrad::config::ZebradConfig;

const CONFIG_PAGE: &str = include_str!("../../../book/src/user/config.md");
const INTRO_PAGE: &str = include_str!("../../../book/src/README.md");

/// The top-level `zebrad.toml` sections, in the order the configuration reference documents them.
const SECTIONS: [&str; 12] = [
    "consensus",
    "network",
    "state",
    "sync",
    "mempool",
    "rpc",
    "mining",
    "tracing",
    "metrics",
    "health",
    "notify",
    "zcashd_compat",
];

/// Nested network tables, documented in the tables of the `[network]` section.
const NETWORK_TABLES: [&str; 6] = [
    "[network.testnet_parameters]",
    "[network.testnet_parameters.activation_heights]",
    "[[network.testnet_parameters.funding_streams]]",
    "[[network.testnet_parameters.funding_streams]]\n[network.testnet_parameters.funding_streams.height_range]",
    "[[network.testnet_parameters.funding_streams]]\n[[network.testnet_parameters.funding_streams.recipients]]",
    "[[network.testnet_parameters.lockbox_disbursements]]",
];

const PROBE_KEY: &str = "zebra_book_probe";

/// Returns the keys that `zebrad.toml` accepts in the table opened by `header`.
///
/// Every configuration table rejects unknown keys, and the rejection lists the accepted ones.
fn accepted_keys(header: &str) -> Vec<String> {
    let probe = format!("{header}\n{PROBE_KEY} = 0\n");
    let error = toml::from_str::<ZebradConfig>(&probe)
        .expect_err("configuration tables reject unknown keys")
        .to_string();
    let (rejected, expected) = error
        .split_once("expected")
        .unwrap_or_else(|| panic!("unexpected error for {header:?}: {error}"));
    assert!(
        rejected.contains(&format!("`{PROBE_KEY}`")),
        "the error for {header:?} must reject the probe key: {error}"
    );

    let keys: Vec<String> = expected
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect();
    assert!(
        !keys.is_empty(),
        "no accepted keys listed for {header:?}: {error}"
    );

    keys
}

/// Returns the text from `heading` up to the next heading of the same or a higher level,
/// ignoring `#` lines inside fenced code blocks.
fn section<'a>(page: &'a str, heading: &str) -> &'a str {
    let level = heading.len() - heading.trim_start_matches('#').len();
    let mut start = None;
    let mut in_fence = false;
    let mut offset = 0;

    for line in page.split_inclusive('\n') {
        let line_text = line.trim_end();
        if line_text.starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence {
            let hashes = line_text.len() - line_text.trim_start_matches('#').len();
            let is_heading = hashes > 0 && line_text[hashes..].starts_with(' ');
            match start {
                None if line_text == heading => start = Some(offset),
                Some(start) if is_heading && hashes <= level => return &page[start..offset],
                _ => {}
            }
        }
        offset += line.len();
    }

    &page[start.unwrap_or_else(|| panic!("the book page has no {heading:?} heading"))..]
}

/// Returns the cells of the Markdown table rows in `text`, excluding header separator rows.
fn table_rows(text: &str) -> impl Iterator<Item = Vec<&str>> {
    text.lines()
        .filter(|line| line.starts_with('|') && !line.starts_with("| ---"))
        .map(|line| line.trim_matches('|').split('|').map(str::trim).collect())
}

/// Accepted legacy key names and the fields they alias. The book documents each alias in its
/// field's table row.
const ALIASES: [(&str, &str); 3] = [
    ("new_peer_interval", "crawl_new_peer_interval"),
    (
        "max_concurrent_block_requests",
        "download_concurrency_limit",
    ),
    ("lookahead_limit", "checkpoint_verify_concurrency_limit"),
];

/// Returns each field named in the first column of the reference tables in `text`, with the rest
/// of its row.
///
/// Field names containing dots are documented in their quoted TOML form, such as `"NU6.1"`.
fn documented_fields(text: &str) -> BTreeMap<String, String> {
    table_rows(text)
        .filter_map(|cells| {
            let name = cells.first()?.strip_prefix('`')?.strip_suffix('`')?;
            Some((name.trim_matches('"').to_owned(), cells[1..].join("|")))
        })
        .collect()
}

/// Returns whether `key` has its own table row, or is a known alias mentioned in its field's row.
fn is_documented(documented: &BTreeMap<String, String>, key: &str) -> bool {
    documented.contains_key(key)
        || ALIASES.iter().any(|&(alias, field)| {
            alias == key
                && documented
                    .get(field)
                    .is_some_and(|row| row.contains(&format!("`{alias}`")))
        })
}

#[test]
fn book_documents_every_config_key() {
    let mut sections = accepted_keys("");
    sections.sort();
    let mut documented_sections = SECTIONS.map(str::to_owned).to_vec();
    documented_sections.sort();
    assert_eq!(
        sections, documented_sections,
        "add a section to book/src/user/config.md and to SECTIONS for each new top-level table"
    );

    let tables = SECTIONS
        .iter()
        .map(|name| (format!("[{name}]"), format!("## `[{name}]`")))
        .chain(
            NETWORK_TABLES
                .iter()
                .map(|header| (header.to_string(), "## `[network]`".to_string())),
        );

    let mut undocumented = Vec::new();
    for (header, heading) in tables {
        let documented = documented_fields(section(CONFIG_PAGE, &heading));
        for key in accepted_keys(&header) {
            if !is_documented(&documented, &key) {
                undocumented.push(format!("{header} {key}"));
            }
        }
    }

    assert!(
        undocumented.is_empty(),
        "book/src/user/config.md has no table row for these keys: {undocumented:#?}"
    );
}

#[test]
fn book_lists_compiled_activation_heights() {
    let table = section(INTRO_PAGE, "## Networks and network upgrades");
    let height = |cell: &str| cell.replace(',', "").parse::<u32>().ok();
    let documented: BTreeMap<String, (Option<u32>, Option<u32>)> = table_rows(table)
        .filter(|cells| cells.len() >= 3 && cells[0] != "Network upgrade")
        .map(|cells| {
            let name = cells[0].split(" (").next().unwrap_or(cells[0]).to_owned();
            (name, (height(cells[1]), height(cells[2])))
        })
        .collect();

    let heights = |network: &Network| -> BTreeMap<String, u32> {
        network
            .activation_list()
            .into_iter()
            .filter(|(_, upgrade)| {
                !matches!(
                    upgrade,
                    NetworkUpgrade::Genesis | NetworkUpgrade::BeforeOverwinter
                )
            })
            .map(|(height, upgrade)| {
                let name = serde_json::to_value(upgrade).expect("upgrades serialize to strings");
                (
                    name.as_str().expect("upgrade names are strings").to_owned(),
                    height.0,
                )
            })
            .collect()
    };
    // Each network is compared on its own, so an upgrade scheduled on Testnet before Mainnet
    // also needs a row.
    let mainnet = heights(&Network::Mainnet);
    let testnet = heights(&Network::new_default_testnet());
    let compiled: BTreeMap<String, (Option<u32>, Option<u32>)> = mainnet
        .keys()
        .chain(testnet.keys())
        .map(|name| {
            let heights = (mainnet.get(name).copied(), testnet.get(name).copied());
            (name.clone(), heights)
        })
        .collect();

    assert_eq!(
        documented, compiled,
        "the network upgrade table in book/src/README.md must match the compiled heights"
    );
}
