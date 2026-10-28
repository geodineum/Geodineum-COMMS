//! The watcher against a throwaway ValKey: COMMS_TEST_VALKEY_PORT=<port> cargo test --test watch (tests/valkey.sh starts one).

use std::collections::HashMap;
use geodineum_comms::watch::{run, WatchOptions};

async fn conn() -> Option<redis::aio::MultiplexedConnection> {
    let port = std::env::var("COMMS_TEST_VALKEY_PORT").ok()?;
    redis::Client::open(format!("redis://127.0.0.1:{port}")).ok()?.get_multiplexed_async_connection().await.ok()
}

fn opts(site: &str, log: &std::path::Path) -> WatchOptions {
    WatchOptions { site: site.into(), environment: "production".into(), topology: "wtest".into(), node: "n1".into(),
        mail_logs: vec![log.to_path_buf()], bot_token: None, chat: None, digest_hour: 0, send: false,
        jobs_dir: log.parent().unwrap().join("jobs") }
}

async fn receipt(c: &mut redis::aio::MultiplexedConnection, site: &str, st: &str) {
    let _: String = redis::cmd("XADD").arg(format!("{{{site}}}:gnode:receipts:production")).arg("*")
        .arg(&[("cmd", "comms.deliver"), ("st", st), ("cid", "1-0"), ("ss", site)]).query_async(c).await.unwrap();
}

const H: u64 = 3_600_000;

#[tokio::test]
async fn the_watcher_alerts_on_what_went_wrong_and_reads_its_daily_mail_back() {
    let Some(mut c) = conn().await else { return };
    let dir = std::env::temp_dir().join(format!("wt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("mail.log");
    std::fs::write(&log, "").unwrap();
    let o = opts("wt_bot", &log);
    let t0: u64 = 1_800_000_000_000 / 86_400_000 * 86_400_000 + 8 * H; // 08:00 UTC
    receipt(&mut c, "wt_a", "failed").await; // before the first look: history, not an alert

    let a = run(&mut c, &o, t0).await.unwrap();
    assert_eq!(a.len(), 1, "{a:?}");
    assert!(a[0].contains("not running on n1"), "no heartbeat → alert");
    let state: String = redis::cmd("GET").arg("{wt_bot}:gnode:comms:watch").query_async(&mut c).await.unwrap();
    let st: serde_json::Value = serde_json::from_str(&state).unwrap();
    let canary_id = st["canary"]["id"].as_str().unwrap().to_string();
    let entry = st["canary"]["entry"].as_str().unwrap().to_string();
    let sent: Vec<(String, HashMap<String, String>)> = redis::cmd("XRANGE").arg("{wt_bot}:gnode:comms:production").arg("-").arg("+").query_async(&mut c).await.unwrap();
    assert_eq!(sent.len(), 1, "the daily mail went on COMMS's own stream");
    assert!(sent[0].1["dispatch"].contains("email") && sent[0].1["content"].contains("COMMS daily"));

    let _: () = redis::cmd("SET").arg("{wtest}:gnode:heartbeat:production:comms:n1").arg("{}").query_async(&mut c).await.unwrap();
    receipt(&mut c, "wt_a", "sent").await;
    receipt(&mut c, "wt_a", "skipped").await;
    let a = run(&mut c, &o, t0 + H / 2).await.unwrap();
    assert_eq!(a.len(), 1, "{a:?}");
    assert!(a[0].contains("wt_a: a message skipped"), "a new skipped delivery → alert; the old failure is not replayed");

    // the mail log shows the daily mail signed and delivered: nothing to say
    std::fs::write(&log, format!("Oct 26 06:00:01 h postfix/cleanup[1]: ABC123: message-id=<comms.{canary_id}@geodineum.com>\n\
Oct 26 06:00:01 h opendkim[2]: ABC123: DKIM-Signature field added (s=default, d=geodineum.com)\n\
Oct 26 06:00:05 h postfix/smtp[3]: ABC123: to=<op@example.invalid>, relay=mx.example.invalid[1.2.3.4]:25, dsn=2.0.0, status=sent (250 ok)\n")).unwrap();
    let _: () = redis::cmd("HSET").arg(format!("{{wt_bot}}:gnode:comms:status:{entry}")).arg("status").arg("sent").query_async(&mut c).await.unwrap();
    let a = run(&mut c, &o, t0 + H).await.unwrap();
    assert!(a.is_empty(), "{a:?}");
    let st: serde_json::Value = serde_json::from_str(&redis::cmd("GET").arg("{wt_bot}:gnode:comms:watch").query_async::<String>(&mut c).await.unwrap()).unwrap();
    assert!(st["canary"].is_null(), "checked and cleared");

    // the next day's mail leaves unsigned: said after three hours of waiting
    let a = run(&mut c, &o, t0 + 24 * H).await.unwrap();
    assert!(a.is_empty());
    let st: serde_json::Value = serde_json::from_str(&redis::cmd("GET").arg("{wt_bot}:gnode:comms:watch").query_async::<String>(&mut c).await.unwrap()).unwrap();
    let id2 = st["canary"]["id"].as_str().unwrap().to_string();
    std::fs::write(&log, format!("Oct 27 06:00:01 h postfix/cleanup[1]: DEF456: message-id=<comms.{id2}@geodineum.com>\n\
Oct 27 06:00:05 h postfix/smtp[3]: DEF456: to=<op@example.invalid>, relay=mx.example.invalid[1.2.3.4]:25, dsn=2.0.0, status=sent (250 ok)\n")).unwrap();
    assert!(run(&mut c, &o, t0 + 25 * H).await.unwrap().is_empty(), "within three hours it waits");
    let a = run(&mut c, &o, t0 + 28 * H).await.unwrap();
    assert!(a.iter().any(|x| x.contains("unsigned")), "{a:?}");
    std::fs::remove_dir_all(&dir).ok();
}

// the watcher reads every site's receipts, so alerts from the other test can show up here
fn mine(a: Vec<String>) -> Vec<String> {
    a.into_iter().filter(|x| x.starts_with("sites backup")).collect()
}

#[tokio::test]
async fn a_nightly_job_that_failed_or_went_quiet_is_an_alert() {
    let Some(mut c) = conn().await else { return };
    let dir = std::env::temp_dir().join(format!("wj-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("jobs")).unwrap();
    let log = dir.join("mail.log");
    std::fs::write(&log, "").unwrap();
    let o = WatchOptions { digest_hour: 23, topology: "wjtest".into(), ..opts("wj_bot", &log) };
    let _: () = redis::cmd("SET").arg("{wjtest}:gnode:heartbeat:production:comms:n1").arg("{}").query_async(&mut c).await.unwrap();
    let t0: u64 = 1_800_000_000_000 / 86_400_000 * 86_400_000 + 8 * H;
    let job = dir.join("jobs/sites-backup.status");

    std::fs::write(&job, format!("what=sites backup\nok_ms={}\nrun_ms={t0}\nerror=upload\n", t0 - 24 * H)).unwrap();
    let a = mine(run(&mut c, &o, t0 + H).await.unwrap());
    assert!(a.len() == 1 && a[0].contains("sites backup failed at") && a[0].contains("upload"), "{a:?}");
    assert!(mine(run(&mut c, &o, t0 + 2 * H).await.unwrap()).is_empty(), "said once");

    std::fs::write(&job, format!("what=sites backup\nok_ms={t0}\nrun_ms={t0}\nerror=\n")).unwrap();
    let a = mine(run(&mut c, &o, t0 + 3 * H).await.unwrap());
    assert!(a.is_empty(), "{a:?}");
    let a = mine(run(&mut c, &o, t0 + 27 * H).await.unwrap());
    assert!(a.iter().any(|x| x.contains("no success in 26 h")), "{a:?}");
    std::fs::remove_dir_all(&dir).ok();
}
