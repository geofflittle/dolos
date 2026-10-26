//! What a tip subscription delivers, taken against the real `DomainAdapter`.
//!
//! A caller subscribes from the last point it applied, so that point must not
//! come back: applying it twice applies its transactions twice. The test
//! double in `dolos-testing` already excludes it, so only a subscription taken
//! against the adapter the node runs can say whether the node does.

mod node;

use std::collections::HashMap;
use std::sync::Arc;

use dolos::engine::DomainBuilder;
use dolos_cardano::CardanoDelta;
use dolos_core::{
    ChainPoint, Domain as _, LogEntry, LogValue, TipEvent, TipSubscription as _, WalStore as _,
};

/// A point whose hash carries its slot, so a delivered point names the entry
/// it came from.
fn point(slot: u64) -> ChainPoint {
    let mut hash = [7u8; 32];
    hash[0..8].copy_from_slice(&slot.to_be_bytes());
    ChainPoint::Specific(slot, hash.into())
}

fn wal_entry(slot: u64) -> LogEntry<CardanoDelta> {
    let value = LogValue {
        block: vec![slot as u8],
        delta: vec![],
        inputs: HashMap::new(),
    };

    (point(slot), value)
}

/// A domain over a fresh node directory whose wal holds one entry per slot.
fn domain_with_wal(
    slots: std::ops::RangeInclusive<u64>,
) -> (node::Node, dolos::adapters::DomainAdapter) {
    let node = node::Node::new();
    dolos::storage::ensure_storage_path(&node.config).unwrap();

    let genesis = Arc::new(dolos_cardano::include::preview::load());
    let domain = DomainBuilder::new(&node.config, genesis).build().unwrap();

    let entries: Vec<_> = slots.map(wal_entry).collect();
    domain.wal().append_entries(entries).unwrap();

    (node, domain)
}

/// The slot of every point the subscription delivers before the sentinel, in
/// delivery order.
///
/// The sentinel is broadcast after the subscription is taken, so it arrives
/// behind the whole replay. Reading until it appears is what makes the replay
/// length an assertion rather than a wait: a replay one longer than expected
/// shows up as an extra slot in this list, and one shorter shows up as the
/// sentinel arriving early.
const SENTINEL_SLOT: u64 = 9_999;

fn delivered_before_sentinel(
    domain: &dolos::adapters::DomainAdapter,
    from: Option<ChainPoint>,
) -> Vec<u64> {
    let mut subscription = domain.watch_tip(from).unwrap();

    domain.notify_tip(TipEvent::Apply(
        point(SENTINEL_SLOT),
        Arc::new(vec![0xffu8]),
    ));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    runtime.block_on(async {
        let mut slots = Vec::new();

        loop {
            let slot = match subscription.next_tip().await {
                TipEvent::Apply(point, _) => point.slot(),
                TipEvent::Undo(point, _) => panic!("unexpected undo at slot {}", point.slot()),
                TipEvent::Mark(point) => panic!("unexpected mark at slot {}", point.slot()),
            };

            if slot == SENTINEL_SLOT {
                return slots;
            }

            slots.push(slot);
            assert!(
                slots.len() <= 64,
                "the subscription never reached the sentinel"
            );
        }
    })
}

/// The point the caller names is the one it has already applied, so the
/// subscription does not deliver it. Every point after it arrives, oldest
/// first.
#[test]
fn a_subscription_skips_the_point_it_intersects_at() {
    let (_node, domain) = domain_with_wal(1..=5);

    let delivered = delivered_before_sentinel(&domain, Some(point(3)));

    assert_eq!(delivered, vec![4, 5]);

    domain.shutdown().unwrap();
}

/// With no point named the caller has applied nothing, so nothing is skipped.
#[test]
fn a_subscription_from_no_point_receives_the_whole_wal() {
    let (_node, domain) = domain_with_wal(1..=5);

    let delivered = delivered_before_sentinel(&domain, None);

    assert_eq!(delivered, vec![1, 2, 3, 4, 5]);

    domain.shutdown().unwrap();
}

/// A subscriber that applies every delivery applies each point after the
/// intersect once.
#[test]
fn every_point_after_the_intersect_is_applied_exactly_once() {
    let (_node, domain) = domain_with_wal(1..=8);

    let delivered = delivered_before_sentinel(&domain, Some(point(1)));

    let mut distinct = delivered.clone();
    distinct.sort_unstable();
    distinct.dedup();

    assert_eq!(delivered.len(), distinct.len(), "a point was applied twice");
    assert_eq!(distinct, vec![2, 3, 4, 5, 6, 7, 8]);
    assert!(!delivered.contains(&1), "the intersect point was replayed");

    domain.shutdown().unwrap();
}

/// The wal tip is the point the crawler subscribes from once it has read the
/// whole wal, and there is nothing after it, so the subscription delivers
/// nothing until the chain moves.
#[test]
fn a_subscription_at_the_wal_tip_replays_nothing() {
    let (_node, domain) = domain_with_wal(1..=5);

    let delivered = delivered_before_sentinel(&domain, Some(point(5)));

    assert_eq!(delivered, Vec::<u64>::new());

    domain.shutdown().unwrap();
}
