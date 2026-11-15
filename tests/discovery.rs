//! Site discovery against a throwaway ValKey: COMMS_TEST_VALKEY_PORT=<port> cargo test --test discovery
//! (tests/valkey.sh starts one). Without the variable the tests return early.
//! A site announced on the broadcast stream is consumed on the next read; one not announced
//! waits for the discovery interval; a user without the read grant falls back to the interval.

use geodineum_comms::{SiteDiscovery, StreamConsumer};

const BROADCAST: &str = "{geodineum}:gnode:broadcast:global";

fn port() -> Option<String> {
    std::env::var("COMMS_TEST_VALKEY_PORT").ok()
}

async fn conn(url: &str) -> redis::aio::MultiplexedConnection {
    redis::Client::open(url).unwrap().get_multiplexed_async_connection().await.unwrap()
}

async fn cmd(c: &mut redis::aio::MultiplexedConnection, args: &[&str]) -> redis::Value {
    let mut q = redis::cmd(args[0]);
    for a in &args[1..] {
        q.arg(*a);
    }
    q.query_async(c).await.unwrap()
}

async fn consumer(url: &str) -> StreamConsumer {
    let c = conn(url).await;
    let mut s = StreamConsumer::new(c.clone(), SiteDiscovery::new(c, "production".into()), "t1".into())
        .with_block_ms(10)
        .with_discovery_interval(3600)
        .with_broadcast("geodineum");
    s.initialize_groups().await.unwrap();
    s
}

async fn consumed(c: &mut redis::aio::MultiplexedConnection, site: &str) -> bool {
    let key = format!("{{{site}}}:gnode:comms:production");
    let groups: redis::RedisResult<Vec<Vec<redis::Value>>> = redis::cmd("XINFO").arg("GROUPS").arg(&key).query_async(c).await;
    groups.map(|g| !g.is_empty()).unwrap_or(false)
}

async fn register(c: &mut redis::aio::MultiplexedConnection, site: &str) {
    cmd(c, &["HSET", &format!("gnode:site:{site}:meta"), "status", "active"]).await;
}

async fn announce(c: &mut redis::aio::MultiplexedConnection, t: &str, site: &str) {
    let data = format!(r#"{{"site_id":"{site}"}}"#);
    cmd(c, &["XADD", BROADCAST, "*", "type", t, "data", &data]).await;
}

async fn next_look(s: &mut StreamConsumer) {
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    s.read_messages().await.unwrap();
}

// One test, in order: the cases share the broadcast stream.
#[tokio::test]
async fn announced_sites_are_consumed_at_once_and_the_interval_stays() {
    let Some(p) = port() else { return };
    let url = format!("redis://127.0.0.1:{p}");
    let mut admin = conn(&url).await;
    cmd(&mut admin, &["FLUSHALL"]).await;

    // History is not replayed: an announcement older than the consumer is not acted on.
    announce(&mut admin, "site_registered", "old_com").await;
    let mut s = consumer(&url).await;
    register(&mut admin, "old_com").await;
    s.read_messages().await.unwrap();
    next_look(&mut s).await;
    assert!(!consumed(&mut admin, "old_com").await, "an announcement older than the consumer was replayed");

    register(&mut admin, "quiet_com").await;
    announce(&mut admin, "topology_update", "quiet_com").await;
    next_look(&mut s).await;
    assert!(!consumed(&mut admin, "quiet_com").await, "an event that is not about discovery re-ran it");

    register(&mut admin, "loud_com").await;
    announce(&mut admin, "site_registered", "loud_com").await;
    next_look(&mut s).await;
    assert!(consumed(&mut admin, "loud_com").await, "announced site not consumed");
    assert!(consumed(&mut admin, "quiet_com").await, "the re-run reads every site");

    // A user without the read grant: the look turns off, nothing fails, the interval stays.
    cmd(&mut admin, &["ACL", "SETUSER", "comms_nobroadcast", "on", ">pw", "resetkeys", "~*:gnode:comms:*",
        "%R~gnode:site:*", "+@all"]).await;
    let mut limited = consumer(&format!("redis://comms_nobroadcast:pw@127.0.0.1:{p}")).await;
    limited.read_messages().await.unwrap();
    register(&mut admin, "refused_com").await;
    announce(&mut admin, "site_registered", "refused_com").await;
    next_look(&mut limited).await;
    assert!(!consumed(&mut admin, "refused_com").await, "consumed without a readable announcement");
    cmd(&mut admin, &["ACL", "DELUSER", "comms_nobroadcast"]).await;
}
