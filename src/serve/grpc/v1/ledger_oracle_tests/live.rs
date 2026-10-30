use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use ledger_oracle_judge::{
    compare::Tally,
    live::{
        block_bytes, chunk_blocks, era_name, node_amounts, settle, utxo_presence, Aligned, Amounts,
        NodeTip, UtxoPresence,
    },
    manifest::{self, Candidates, Known, LiveLocus},
    record::PParams,
};
use serde::de::DeserializeOwned;

use super::super::spec::{
    cardano,
    query::{self as q, any_chain_params, any_chain_tx, read_era_summary_response},
    sync::{self as s, any_chain_block},
};
use super::{
    output::Integer,
    params,
    serve::{add_block, add_tx, Served},
    show_input,
};

fn read<T: DeserializeOwned>(dir: &Path, name: &str) -> T {
    let path = dir.join(name);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("LIVE {} {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("LIVE {} {e}", path.display()))
}

/// The files in the reads directory whose names start with the prefix, in name order.
fn files(dir: &Path, prefix: &str) -> Vec<String> {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("LIVE {} {e}", dir.display()));
    let mut names: Vec<String> = entries
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(prefix))
        .collect();
    names.sort();
    names
}

fn served_params(response: q::ReadParamsResponse) -> (cardano::PParams, u64) {
    let slot = response
        .ledger_tip
        .expect("ReadParams names its ledger tip")
        .slot;
    match response.values.and_then(|v| v.params) {
        Some(any_chain_params::Params::Cardano(p)) => (p, slot),
        None => panic!("LIVE ReadParams serves no Cardano parameters"),
    }
}

fn era_names(response: q::ReadEraSummaryResponse) -> Vec<String> {
    match response.summary {
        Some(read_era_summary_response::Summary::Cardano(eras)) => {
            eras.summaries.into_iter().map(|e| e.name).collect()
        }
        None => Vec::new(),
    }
}

/// The hashes of the transactions in a served block that contain sub transactions.
fn batch_hashes(block: &s::AnyChainBlock) -> Vec<[u8; 32]> {
    let Some(any_chain_block::Chain::Cardano(block)) = &block.chain else {
        panic!("LIVE a served block is a Cardano block");
    };
    let txs = block.body.iter().flat_map(|b| &b.tx);
    txs.filter(|tx| !tx.sub_transactions.is_empty())
        .map(|tx| {
            let hash = &tx
                .common
                .as_ref()
                .expect("a served transaction has common fields")
                .hash;
            hash.as_ref().try_into().expect("a served hash is 32 bytes")
        })
        .collect()
}

fn presence(output: Option<&Option<cardano::TxOutput>>) -> &'static str {
    match output {
        None => "absent",
        Some(None) => "no_output",
        Some(Some(_)) => "present",
    }
}

/// Counts each input FetchBlock serves for a batch transaction against the output ReadTx serves for it.
fn compare_children(dir: &Path, tally: &mut Tally) {
    let mut read_tx = Served::new();
    for name in files(dir, "tx-") {
        let response: q::ReadTxResponse = read(dir, &name);
        match response.tx.and_then(|t| t.chain) {
            Some(any_chain_tx::Chain::Cardano(tx)) => add_tx(&mut read_tx, &tx),
            None => panic!("LIVE {name} serves no Cardano transaction"),
        }
    }
    for name in files(dir, "block-") {
        let response: s::FetchBlockResponse = read(dir, &name);
        let block = response
            .block
            .unwrap_or_else(|| panic!("LIVE {name} serves no block"));
        let mut fetched = Served::new();
        add_block(&mut fetched, &block);
        for hash in batch_hashes(&block) {
            let tx = hex::encode(hash);
            let fetched = &fetched[&hash];
            let Some(read) = read_tx.get(&hash) else {
                tally.add(false, || format!("{name} tx={tx} readtx=absent"));
                continue;
            };
            let read: BTreeMap<_, _> = read.iter().map(|(i, o)| (i, o)).collect();
            for (input, output) in fetched {
                let other = read.get(input).copied();
                let agrees = output.is_some() && other == Some(output);
                tally.add(agrees, || {
                    let differs =
                        output.is_some() && other.is_some_and(|o| o.is_some() && o != output);
                    format!(
                        "{name} tx={tx} input={} fetchblock={} readtx={}",
                        show_input(input),
                        presence(Some(output)),
                        if differs { "differs" } else { presence(other) },
                    )
                });
            }
            if read.len() != fetched.len() {
                tally.add(false, || {
                    format!(
                        "{name} tx={tx} inputs fetchblock={} readtx={}",
                        fetched.len(),
                        read.len()
                    )
                });
            }
        }
    }
}

/// Counts the bytes of each served block against the node's immutable block at its slot.
fn compare_blocks(dir: &Path, tally: &mut Tally) {
    let mut node = BTreeMap::new();
    for name in files(dir, "node-").iter().filter(|n| n.ends_with(".chunk")) {
        let secondary = name.replace(".chunk", ".secondary");
        let bytes = |name: &str| {
            let path = dir.join(name);
            fs::read(&path).unwrap_or_else(|e| panic!("LIVE {} {e}", path.display()))
        };
        let blocks = chunk_blocks(&bytes(&secondary), &bytes(name))
            .unwrap_or_else(|e| panic!("LIVE {name} {e}"));
        node.extend(blocks.into_iter().map(|b| (b.slot, b)));
    }
    for name in files(dir, "block-") {
        let response: s::FetchBlockResponse = read(dir, &name);
        let block = response
            .block
            .unwrap_or_else(|| panic!("LIVE {name} serves no block"));
        let Some(any_chain_block::Chain::Cardano(cardano)) = &block.chain else {
            panic!("LIVE {name} serves no Cardano block");
        };
        let header = cardano
            .header
            .as_ref()
            .unwrap_or_else(|| panic!("LIVE {name} serves no header"));
        let certifying = header.body_contains_leios_certificate;
        let verdict = block_bytes(node.get(&header.slot), &block.native_bytes, certifying);
        tally.add(verdict.agrees(), || {
            format!(
                "{name} slot={} certifying={certifying} bytes={}",
                header.slot,
                verdict.name()
            )
        });
    }
}

/// The value of a served output as named amounts, none when an amount is not a natural number.
fn served_amounts(output: &cardano::TxOutput) -> Option<Amounts> {
    let amount = |value: Option<&cardano::BigInt>| Integer::served(value?)?.unsigned();
    let mut amounts = Amounts::from([("lovelace".to_string(), amount(output.coin.as_ref())?)]);
    for policy in &output.assets {
        for asset in &policy.assets {
            let key = format!(
                "{}.{}",
                hex::encode(&policy.policy_id),
                hex::encode(&asset.name)
            );
            amounts.insert(key, amount(asset.quantity.as_ref())?);
        }
    }
    Some(amounts)
}

/// Counts the outputs of the served blocks by their values on Dolos and the node, and returns how many neither holds.
fn compare_utxos(dir: &Path, tally: &mut Tally) -> usize {
    let refs = fs::read_to_string(dir.join("refs.txt")).expect("LIVE refs.txt");
    let node = node_amounts(&read(dir, "node-utxo.json")).unwrap_or_else(|e| panic!("LIVE {e}"));
    let served: q::ReadUtxosResponse = read(dir, "utxos.json");
    let mut dolos = BTreeMap::new();
    for item in served.items {
        let txo = item
            .txo_ref
            .expect("LIVE a served UTxO names its reference");
        let output = match item.parsed_state {
            Some(q::any_utxo_data::ParsedState::Cardano(output)) => served_amounts(&output),
            None => None,
        };
        dolos.insert(format!("{}#{}", hex::encode(&txo.hash), txo.index), output);
    }
    let mut neither = 0;
    for line in refs.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let [_, hash, index] = fields[..] else {
            panic!("LIVE refs.txt line {line}");
        };
        let key = format!("{hash}#{index}");
        let served = match dolos.get(&key) {
            None => None,
            Some(Some(amounts)) => Some(amounts),
            Some(None) => {
                tally.add(false, || format!("utxo={key} dolos=unreadable"));
                continue;
            }
        };
        let show = |presence: &str| format!("utxo={key} {presence}");
        match utxo_presence(node.get(&key), served) {
            UtxoPresence::Neither => neither += 1,
            UtxoPresence::Both { same } => tally.add(same, || show("values=differ")),
            UtxoPresence::NodeOnly => tally.add(false, || show("dolos=absent")),
            UtxoPresence::DolosOnly => tally.add(false, || show("node=absent")),
        }
    }
    neither
}

#[test]
#[ignore = "judges a live Dolos endpoint against its node from reads written by oracle/box/live.sh"]
fn live() {
    let dir = PathBuf::from(std::env::var("LIVE_DIR").expect("LIVE_DIR names the reads directory"));
    let known = manifest::known().unwrap_or_else(|e| panic!("{e}")).live;
    let image = fs::read_to_string(dir.join("image.txt")).expect("LIVE image.txt");
    let before: NodeTip = read(&dir, "node-tip-before.json");
    let after: NodeTip = read(&dir, "node-tip-after.json");
    let tip: s::ReadTipResponse = read(&dir, "tip.json");
    let (served, params_slot) = served_params(read(&dir, "params.json"));
    let ledger = PParams(read(&dir, "node-params.json"));
    let aligned = Aligned::new(&before, &after, params_slot, (ledger, served));
    println!(
        "LIVE image={} node_tip={} dolos_tip={} params_slot={params_slot} aligned={}",
        image.trim(),
        before.slot,
        tip.tip.map_or("none".to_string(), |t| t.slot.to_string()),
        aligned
            .as_ref()
            .map_or("no".to_string(), |a| format!("epoch={}", a.epoch())),
    );

    let mut tallies: BTreeMap<LiveLocus, Tally> = BTreeMap::new();
    if let Some(aligned) = &aligned {
        let (ledger, served) = aligned.value();
        let label = format!("epoch={}", aligned.epoch());
        params::compare(
            label,
            ledger,
            served,
            tallies.entry(LiveLocus::Params).or_default(),
        );
    }
    let names = era_names(read(&dir, "era.json"));
    let verdict = era_name(&names, &before.era);
    let era = tallies.entry(LiveLocus::EraSummary).or_default();
    era.add(verdict.agrees(), || verdict.to_string());
    compare_children(&dir, tallies.entry(LiveLocus::ChildAsOutput).or_default());
    compare_blocks(&dir, tallies.entry(LiveLocus::Block).or_default());
    let spent = compare_utxos(&dir, tallies.entry(LiveLocus::Utxo).or_default());
    println!("LIVE utxo spent_on_both={spent}");

    let compared = LiveLocus::ALL.iter().map(|locus| {
        let n = tallies.get(locus).map_or(0, |t| t.compared);
        format!("{locus}={n}")
    });
    println!("LIVE compared {}", compared.collect::<Vec<_>>().join(" "));
    let settled = settle(&tallies, &known);
    for line in &settled.lines {
        println!("{line}");
    }
    let candidates = Candidates {
        known: Known {
            live: settled.known_fired.clone(),
            ..Known::default()
        },
        ..Candidates::default()
    };
    for line in candidates.lines() {
        println!("{line}");
    }
    println!("LIVE RESULT {}", if settled.pass { "PASS" } else { "FAIL" });
    assert!(settled.pass, "the live comparisons disagree");
}
