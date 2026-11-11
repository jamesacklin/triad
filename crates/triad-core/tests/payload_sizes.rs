//! Milestone 2 measurement: which payload sizes p2panda delivers, live and
//! through catch-up sync. Run with `cargo test -p triad-core --test
//! payload_sizes -- --ignored --nocapture`.

use std::collections::BTreeMap;
use std::time::Duration;

use triad_core::envelope::Envelope;
use triad_core::identity::Identity;
use triad_core::net::{DEFAULT_RELAY, Net, NetConfig, NetEvent, NetEvents, random_topic};

const SIZES: [usize; 4] = [1024, 4 * 1024, 16 * 1024, 64 * 1024];

fn payload(size: usize, tag: u8) -> Envelope {
    let mut bytes = vec![tag; size];
    bytes[0] = (size / 1024) as u8;
    Envelope::app(bytes)
}

/// Collects foreign App payload sizes until all expected arrive or the window ends.
async fn collect(
    events: &mut NetEvents,
    me: [u8; 32],
    want: usize,
    window: Duration,
) -> BTreeMap<usize, String> {
    let mut got = BTreeMap::new();
    let deadline = tokio::time::Instant::now() + window;
    while got.len() < want {
        let Ok(Some(ev)) = tokio::time::timeout_at(deadline, events.next()).await else {
            break;
        };
        match ev {
            NetEvent::Op(op) => {
                if op.author != me
                    && let Envelope::App { ciphertext, .. } = &op.envelope
                {
                    got.insert(ciphertext.len(), op.source.to_string());
                }
                op.ack().await.unwrap();
            }
            NetEvent::Failed { error, .. } => eprintln!("failed: {error}"),
            NetEvent::SyncEnded { error: Some(e), .. } => eprintln!("sync error: {e}"),
            _ => {}
        }
    }
    got
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement; talks to the public relay"]
async fn payload_sizes_live_and_catch_up() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let id_a = Identity::load_or_create(dir_a.path()).unwrap();
    let id_b = Identity::load_or_create(dir_b.path()).unwrap();

    // Catch-up: A publishes while B is offline.
    let topic = random_topic();
    let (a, mut a_events) = Net::start(
        &id_a,
        NetConfig {
            data_dir: dir_a.path(),
            relay: DEFAULT_RELAY,
            bootstrap: None,
            topic,
        },
    )
    .await
    .unwrap();
    for size in SIZES {
        let hash = a.publish_unchecked(&payload(size, 1)).await;
        eprintln!("catch-up publish {size}: {:?}", hash.map(hex::encode));
    }
    // Drain A's own deliveries.
    let _ = collect(
        &mut a_events,
        id_a.node_id(),
        usize::MAX,
        Duration::from_secs(2),
    )
    .await;

    let (b, mut b_events) = Net::start(
        &id_b,
        NetConfig {
            data_dir: dir_b.path(),
            relay: DEFAULT_RELAY,
            bootstrap: Some(id_a.node_id()),
            topic,
        },
    )
    .await
    .unwrap();
    let catch_up = collect(
        &mut b_events,
        id_b.node_id(),
        SIZES.len(),
        Duration::from_secs(30),
    )
    .await;
    eprintln!("CATCH-UP received: {catch_up:?}");

    // Live: both online, A publishes again.
    for size in SIZES {
        a.publish_unchecked(&payload(size, 2)).await.unwrap();
    }
    let live = collect(
        &mut b_events,
        id_b.node_id(),
        SIZES.len(),
        Duration::from_secs(30),
    )
    .await;
    eprintln!("LIVE received: {live:?}");

    for size in SIZES {
        eprintln!(
            "RESULT {size:>6} bytes: catch-up={} live={}",
            catch_up.get(&size).map(String::as_str).unwrap_or("MISSING"),
            live.get(&size).map(String::as_str).unwrap_or("MISSING"),
        );
    }
    b.shutdown().await;
    a.shutdown().await;
    drop(a_events);
}
