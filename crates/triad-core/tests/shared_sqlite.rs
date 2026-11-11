//! Findings question: can p2panda, OpenMLS and the app share one SQLite file?
//! Run with `cargo test -p triad-core --test shared_sqlite -- --ignored --nocapture`.

use triad_core::envelope::Envelope;
use triad_core::identity::Identity;
use triad_core::mls::Mls;
use triad_core::net::{DEFAULT_RELAY, Net, NetConfig, NetEvent, P2PANDA_FILE, random_topic};
use triad_core::state::{Outcome, State};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "experiment; talks to the public relay"]
async fn three_owners_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let id = Identity::load_or_create(dir.path()).unwrap();
    let topic = random_topic();
    let shared = dir.path().join(P2PANDA_FILE);

    // p2panda creates and migrates the file first.
    let (net, mut events) = Net::start(
        &id,
        NetConfig {
            data_dir: dir.path(),
            relay: DEFAULT_RELAY,
            bootstrap: None,
            topic,
        },
    )
    .await
    .unwrap();

    // OpenMLS and the app open the same file with their own rusqlite connections.
    let mut mls = Mls::open(&shared, id.node_id()).unwrap();
    mls.create_group(&topic).unwrap();
    let mut state = State::open_path(&shared).unwrap();

    for i in 0..20 {
        let ct = mls.encrypt(format!("m{i}").as_bytes()).unwrap();
        let hash = net.publish(&Envelope::app(ct)).await.unwrap();
        state.record(&hash, Outcome::Ok, None, None).unwrap();
        // Let p2panda write while the other two hold connections.
        if let Some(NetEvent::Op(op)) = events.next().await {
            op.ack().await.unwrap();
        }
    }
    drop(mls);
    let mut mls = Mls::open(&shared, id.node_id()).unwrap();
    assert!(mls.load_group(&topic).unwrap());
    assert_eq!(state.counts().unwrap().ok, 20);
    net.shutdown().await;

    let conn = rusqlite::Connection::open(&shared).unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    eprintln!("TABLES in one file: {tables:?}");
    assert!(tables.iter().any(|t| t == "operations_v1"));
    assert!(tables.iter().any(|t| t == "openmls_group_data"));
    assert!(tables.iter().any(|t| t == "processed_ops"));
}
