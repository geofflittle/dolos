use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use ledger_oracle_judge::{
    cbor::{self, EraBound, NodeEra, TxIn},
    compare::Tally,
    live::{
        block_bytes, chunk_blocks, compare_era_summary, compare_utxos, settle, Aligned, AtPoint,
        Endorsed, NodeBlock, NodeSlot, NodeTime, NodeTip, Point, ServedEra, ServedTime,
        UtxoBracket, UtxoReads,
    },
    manifest::{self, Candidates, Known, LiveLocus},
    record::PParams,
};
use pallas::{
    codec::minicbor,
    ledger::traverse::{self, Era, MultiEraTx},
};
use serde::de::DeserializeOwned;

use super::super::spec::{
    cardano,
    query::{self as q, any_chain_params, any_chain_tx, read_era_summary_response},
    sync::{self as s, any_chain_block},
};
use super::{
    endorser::{judge_served, node_txs, Judged, NodeEndorser},
    output, params,
    serve::{add_block, add_tx, Served, Service},
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

fn served_eras(response: q::ReadEraSummaryResponse) -> Vec<ServedEra> {
    let bound = |b: &cardano::EraBoundary| EraBound {
        slot: b.slot,
        epoch: b.epoch,
    };
    match response.summary {
        Some(read_era_summary_response::Summary::Cardano(eras)) => eras
            .summaries
            .into_iter()
            .map(|e| {
                let start = e
                    .start
                    .unwrap_or_else(|| panic!("LIVE era.json era {} serves no start", e.name));
                ServedEra {
                    start: bound(&start),
                    start_ms: start.time,
                    end: e.end.as_ref().map(bound),
                    name: e.name,
                }
            })
            .collect(),
        None => Vec::new(),
    }
}

/// The node's era history from its query answer.
fn node_eras(dir: &Path) -> Vec<NodeEra> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        #[serde(rename = "cborHex")]
        cbor_hex: String,
    }
    let envelope: Envelope = read(dir, "node-era-history.json");
    let raw = hex::decode(&envelope.cbor_hex).expect("LIVE node-era-history.json is hex");
    cbor::era_history(&raw).unwrap_or_else(|e| panic!("LIVE node-era-history.json {e}"))
}

/// The node's slot at each second it was asked to translate, keyed by the second.
fn node_slots(dir: &Path) -> BTreeMap<u64, NodeSlot> {
    let names = files(dir, "node-slot-");
    let answers = names.iter().filter_map(|name| {
        let second = name.strip_prefix("node-slot-")?.strip_suffix(".txt")?;
        let second = second.parse().unwrap_or_else(|e| panic!("LIVE {name} {e}"));
        let text = fs::read_to_string(dir.join(name)).unwrap_or_else(|e| panic!("LIVE {name} {e}"));
        let slot = text.parse().unwrap_or_else(|e| panic!("LIVE {name} {e}"));
        Some((second, slot))
    });
    answers.collect()
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

/// Counts the bytes of each served block against the node's immutable block at
/// its slot.
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
        let (endorsed, lines) = if certifying {
            let judged = endorsed(dir, &node, header.slot, &block);
            let lines = match &judged {
                Ok(judged) => judged.lines(Service::FetchBlock, header.slot),
                Err(e) => vec![format!("unjudged={e}")],
            };
            let agreed = judged.ok().and_then(|j| j.agreed());
            (
                Some(agreed.map_or(Endorsed::Differ, Endorsed::Agree)),
                lines,
            )
        } else {
            (None, Vec::new())
        };
        let verdict = block_bytes(node.get(&header.slot), &block.native_bytes, endorsed);
        tally.add(verdict.agrees(), || {
            let mut line = format!(
                "{name} slot={} certifying={certifying} bytes={}",
                header.slot,
                verdict.name()
            );
            for l in &lines {
                line.push(' ');
                line.push_str(l);
            }
            line
        });
    }
}

/// The served transactions of a certifying block judged against the node's
/// block, its parent among the node's blocks and the endorser block the node's
/// leios store holds for it in `node-eb-<slot>.json`.
fn endorsed(
    dir: &Path,
    node: &BTreeMap<u64, NodeBlock>,
    slot: u64,
    served: &s::AnyChainBlock,
) -> Result<Judged, String> {
    let block = node.get(&slot).ok_or("node block absent")?;
    let decoded = traverse::MultiEraBlock::decode(&block.bytes).map_err(|e| e.to_string())?;
    let prev = decoded
        .header()
        .previous_hash()
        .ok_or("the node block names no parent")?;
    let parent = node
        .range(..slot)
        .rev()
        .find(|(_, b)| traverse::MultiEraBlock::decode(&b.bytes).is_ok_and(|p| p.hash() == prev))
        .ok_or(format!("node parent {prev} absent"))?
        .1;
    let path = dir.join(format!("node-eb-{slot}.json"));
    let text = fs::read_to_string(&path).map_err(|e| format!("{} {e}", path.display()))?;
    let stored: NodeEndorser = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let txs = node_txs(&parent.bytes, &block.bytes, Some(&stored))?;
    judge_served(served, &txs)
}

/// The fields of a served output that state other values than the output
/// bytes hold.
fn fields(ledger: &[u8], served: &cardano::TxOutput) -> Vec<String> {
    match output::differing(ledger, served) {
        Ok(fields) => fields.iter().map(ToString::to_string).collect(),
        Err(e) => vec![format!("undecodable({e})")],
    }
}

/// The bytes of each output in the node's UTxO query answer, keyed by its
/// reference as `hash#index`.
fn node_outputs(cbor: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, minicbor::decode::Error> {
    let mut d = minicbor::Decoder::new(cbor);
    let len = d.map()?;
    let mut outputs = BTreeMap::new();
    let mut read = 0;
    loop {
        match len {
            Some(len) if read == len => break,
            None if d.datatype()? == minicbor::data::Type::Break => break,
            _ => read += 1,
        }
        d.array()?;
        let hash = hex::encode(d.bytes()?);
        let index = d.u64()?;
        let start = d.position();
        d.skip()?;
        let output = cbor[start..d.position()].to_vec();
        outputs.insert(format!("{hash}#{index}"), output);
    }
    Ok(outputs)
}

/// The output bytes of a transaction and of each of its sub transactions,
/// keyed by `hash#index` with each hash computed from the bytes.
fn authenticated_outputs(native: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, traverse::Error> {
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, native)
        .or_else(|_| MultiEraTx::decode(native))?;
    let mut outputs = BTreeMap::new();
    for tx in std::iter::once(tx.clone()).chain(tx.sub_transactions()) {
        let hash = hex::encode(tx.hash());
        for (index, output) in tx.outputs().iter().enumerate() {
            outputs.insert(format!("{hash}#{index}"), output.encode());
        }
    }
    Ok(outputs)
}

/// The outputs the transactions in the producer reads hold, keyed by
/// `hash#index` with each hash computed from the served bytes.
fn producer_outputs(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut outputs = BTreeMap::new();
    for name in files(dir, "producer-") {
        let response: q::ReadTxResponse = read(dir, &name);
        let native = response.tx.map(|t| t.native_bytes).unwrap_or_default();
        match authenticated_outputs(&native) {
            Ok(read) => outputs.extend(read),
            Err(e) => println!("LIVE {name} undecodable {e}"),
        }
    }
    outputs
}

/// Counts one served `asOutput` against the output bytes its input names.
fn judge_as_output(
    producers: &BTreeMap<String, Vec<u8>>,
    name: &str,
    input: &TxIn,
    served: &cardano::TxOutput,
    tally: &mut Tally,
) {
    let key = show_input(input);
    match producers.get(&key) {
        None => tally.add(false, || format!("{name} input={key} producer=absent")),
        Some(ledger) => {
            let differing = fields(ledger, served);
            tally.add(differing.is_empty(), || {
                format!("{name} input={key} differing={}", differing.join(","))
            });
        }
    }
}

/// Counts each output FetchBlock and ReadTx serve for an input of a batch
/// transaction against the producing transaction's output bytes.
fn compare_as_outputs(dir: &Path, tally: &mut Tally) {
    let producers = producer_outputs(dir);
    let mut reads = Vec::new();
    for name in files(dir, "tx-") {
        let response: q::ReadTxResponse = read(dir, &name);
        let mut served = Served::new();
        if let Some(any_chain_tx::Chain::Cardano(tx)) = response.tx.and_then(|t| t.chain) {
            add_tx(&mut served, &tx);
        }
        reads.push((name, served));
    }
    for name in files(dir, "block-") {
        let response: s::FetchBlockResponse = read(dir, &name);
        let mut served = Served::new();
        if let Some(block) = &response.block {
            add_block(&mut served, block);
            let batches = batch_hashes(block);
            served.retain(|hash, _| batches.contains(hash));
        }
        reads.push((name, served));
    }
    for (name, served) in &reads {
        for (input, output) in served.values().flatten() {
            if let Some(output) = output {
                judge_as_output(&producers, name, input, output, tally);
            }
        }
    }
}

/// The point a node tip query names.
fn node_point(dir: &Path, name: &str) -> Point {
    #[derive(serde::Deserialize)]
    struct Tip {
        slot: u64,
        hash: String,
    }
    let tip: Tip = read(dir, name);
    let hash = hex::decode(&tip.hash).unwrap_or_else(|e| panic!("LIVE {name} {e}"));
    Point {
        slot: tip.slot,
        hash,
    }
}

/// The ledger point a ReadUtxos answer names.
fn served_point(response: &q::ReadUtxosResponse, name: &str) -> Point {
    let tip = response
        .ledger_tip
        .as_ref()
        .unwrap_or_else(|| panic!("LIVE {name} names no ledger tip"));
    Point {
        slot: tip.slot,
        hash: tip.hash.to_vec(),
    }
}

/// Counts the outputs of the served blocks by their fields on Dolos and the
/// node when both read them at one point, and returns how many neither holds,
/// none when the reads name two points.
fn compare_served_utxos(dir: &Path, tally: &mut Tally) -> Option<usize> {
    let refs = fs::read_to_string(dir.join("refs.txt")).expect("LIVE refs.txt");
    let refs = refs.lines().map(|line| {
        let fields: Vec<&str> = line.split(' ').collect();
        let [_, hash, index] = fields[..] else {
            panic!("LIVE refs.txt line {line}");
        };
        format!("{hash}#{index}")
    });
    let hex_text = fs::read_to_string(dir.join("node-utxo.cbor")).expect("LIVE node-utxo.cbor");
    let cbor = hex::decode(hex_text.trim()).unwrap_or_else(|e| panic!("LIVE node-utxo.cbor {e}"));
    let node = node_outputs(&cbor).unwrap_or_else(|e| panic!("LIVE node-utxo.cbor {e}"));
    let served: q::ReadUtxosResponse = read(dir, "utxos.json");
    let bracket = UtxoBracket {
        node_before: node_point(dir, "node-utxo-tip-before.json"),
        dolos_before: served_point(&read(dir, "utxos-tip-before.json"), "utxos-tip-before.json"),
        dolos_after: served_point(&served, "utxos.json"),
        node_after: node_point(dir, "node-utxo-tip-after.json"),
    };
    let mut dolos = BTreeMap::new();
    for item in served.items {
        let txo = item
            .txo_ref
            .expect("LIVE a served UTxO names its reference");
        let output = item
            .parsed_state
            .map(|q::any_utxo_data::ParsedState::Cardano(output)| output);
        dolos.insert(format!("{}#{}", hex::encode(&txo.hash), txo.index), output);
    }
    let reads = UtxoReads {
        refs: refs.collect(),
        node,
        dolos,
    };
    match AtPoint::new(&bracket, reads) {
        Some(reads) => Some(compare_utxos(&reads, tally, |node, served| {
            fields(node, served)
        })),
        None => {
            tally.add(false, || format!("utxo reads at two points {bracket}"));
            None
        }
    }
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
        tip.tip
            .as_ref()
            .map_or("none".to_string(), |t| t.slot.to_string()),
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
    let history = node_eras(&dir);
    let current = history
        .last()
        .expect("LIVE the node's era history holds an era");
    println!(
        "LIVE era_history eras={} epoch_size={} horizon={} served=no_epoch_size,no_horizon",
        history.len(),
        current.epoch_size,
        current.end.slot
    );
    let tip_time = tip.tip.as_ref().map(|t| ServedTime {
        slot: t.slot,
        ms: t.timestamp,
    });
    let slots = node_slots(&dir);
    let node = NodeTime {
        era: &before.era,
        history: &history,
        slots: &slots,
    };
    let eras = served_eras(read(&dir, "era.json"));
    let era = tallies.entry(LiveLocus::EraSummary).or_default();
    match tip_time {
        Some(tip_time) => compare_era_summary(&eras, tip_time, &node, era),
        None => era.add(false, || "tip=none".to_string()),
    }
    compare_children(&dir, tallies.entry(LiveLocus::ChildAsOutput).or_default());
    compare_as_outputs(&dir, tallies.entry(LiveLocus::ChildAsOutput).or_default());
    compare_blocks(&dir, tallies.entry(LiveLocus::Block).or_default());
    let spent = compare_served_utxos(&dir, tallies.entry(LiveLocus::Utxo).or_default());
    println!(
        "LIVE utxo spent_on_both={}",
        spent.map_or("not_compared".to_string(), |n| n.to_string())
    );

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

#[cfg(test)]
mod tests {
    use ledger_oracle_judge::{corpus, record::CorpusKind};

    use super::*;

    const NODE_ANSWER: &str =
        "a18258200021fc925172ee50e1b095d94d3460e9bfecbd5cbce8b7783fc3bb4d0dc25641\
        0082581d60ee43965ca7686881c92d9b2ea91d9fee7fdd152e0b96643482f06dbf1b000000024e80d2c0";
    const KEY: &str = "0021fc925172ee50e1b095d94d3460e9bfecbd5cbce8b7783fc3bb4d0dc25641#0";
    const OUTPUT: &str =
        "82581d60ee43965ca7686881c92d9b2ea91d9fee7fdd152e0b96643482f06dbf1b000000024e80d2c0";

    #[test]
    fn a_node_answer_keys_each_output_bytes_by_its_reference() {
        let definite = hex::decode(NODE_ANSWER).unwrap();
        let outputs = node_outputs(&definite).unwrap();
        let expected = BTreeMap::from([(KEY.to_string(), hex::decode(OUTPUT).unwrap())]);
        assert_eq!(outputs, expected);
        let indefinite = [&[0xbf], &definite[1..], &[0xff]].concat();
        assert_eq!(node_outputs(&indefinite).unwrap(), expected);
        assert!(node_outputs(&definite[..definite.len() - 1]).is_err());
    }

    #[test]
    fn a_served_output_agrees_only_with_the_fields_the_node_bytes_hold() {
        let node = hex::decode(OUTPUT).unwrap();
        let served = output::mapped(&node);
        assert_eq!(fields(&node, &served), Vec::<String>::new());
        let mut moved = served.clone();
        moved.address = vec![0x60; 29].into();
        assert_eq!(fields(&node, &moved), vec!["address"]);
    }

    #[test]
    fn an_as_output_agrees_only_with_the_output_its_input_hash_authenticates() {
        let corpus = corpus::load(CorpusKind::Supp).unwrap_or_else(|e| panic!("{e}"));
        let record = corpus.kept().flat_map(|run| run.records()).next().unwrap();
        let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0).unwrap();
        let producers = authenticated_outputs(&record.tx.0).unwrap();
        let input = TxIn {
            tx: *tx.hash(),
            ix: 0,
        };
        let served = output::mapped(&producers[&show_input(&input)]);
        let mut t = Tally::default();
        judge_as_output(&producers, "block", &input, &served, &mut t);
        assert_eq!((t.compared, t.disagree.len()), (1, 0));

        let mut moved = served.clone();
        moved.address = vec![0x60; 29].into();
        judge_as_output(&producers, "block", &input, &moved, &mut t);
        let unclaimed = TxIn { tx: [0; 32], ix: 0 };
        judge_as_output(&producers, "block", &unclaimed, &served, &mut t);
        let key = show_input(&input);
        let zero = show_input(&unclaimed);
        assert_eq!(
            t.disagree,
            [
                format!("block input={key} differing=address"),
                format!("block input={zero} producer=absent"),
            ]
        );
    }
}
