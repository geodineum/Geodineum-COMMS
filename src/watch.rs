//! COMMS's failsafe: `geodineum-comms watch` (a timer runs it hourly) and `geodineum-comms ledger`.
//!
//! "Sent" in COMMS means the local mail server accepted the message, never that it left. The watcher
//! closes that gap and makes silence loud:
//!   - every failed, partly sent or skipped delivery receipt of a production site is an alert;
//!   - a missing COMMS heartbeat is an alert;
//!   - once a day COMMS mails the operator the per-site summary through its own pipeline (the canary),
//!     and the next run reads the mail log for that message's Message-ID: accepted, signed (DKIM), sent;
//!     anything short of that is an alert;
//!   - once a day one line says all is well, so a silent watcher is itself noticed;
//!   - every nightly job that leaves a status file in the jobs directory: a failed run, or no success
//!     within its `max_age_h`, is an alert.
//!
//! Alerts go to Telegram straight from here, not through COMMS, so they still go when COMMS does not.
//! This module holds what can be decided without ValKey or a mail server: the mail log, the alerts.

use std::collections::HashMap;

/// What the mail log says about one message (by its Message-ID).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MailTrace {
    pub queue_id: String,
    pub signed: bool,
    /// Each recipient's last delivery status: `sent`, `deferred`, `bounced`, …
    pub status: Vec<String>,
    pub relay: Vec<String>,
}

impl MailTrace {
    pub fn delivered(&self) -> bool {
        !self.status.is_empty() && self.status.iter().all(|s| s == "sent")
    }
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let i = line.find(key)? + key.len();
    let rest = &line[i..];
    Some(rest.split([',', ' ']).next().unwrap_or(rest))
}

/// The queue id of a postfix/opendkim line (`… opendkim[884]: 3F09C121AD6: DKIM-Signature …`).
fn queue_id(line: &str) -> Option<&str> {
    let after = line.split("]: ").nth(1)?;
    let qid = after.split(':').next()?;
    (qid.len() >= 6 && qid.chars().all(|c| c.is_ascii_alphanumeric())).then_some(qid)
}

/// Read mail log lines into traces by Message-ID (without the angle brackets).
pub fn parse_mail_log<'a>(lines: impl Iterator<Item = &'a str>) -> HashMap<String, MailTrace> {
    let mut by_qid: HashMap<String, MailTrace> = HashMap::new();
    let mut mid_of: HashMap<String, String> = HashMap::new();
    for line in lines {
        let Some(qid) = queue_id(line) else { continue };
        if let Some(mid) = field(line, "message-id=<") {
            mid_of.insert(qid.to_string(), mid.trim_end_matches('>').to_string());
            by_qid.entry(qid.to_string()).or_default().queue_id = qid.to_string();
        } else if line.contains("DKIM-Signature field added") {
            by_qid.entry(qid.to_string()).or_default().signed = true;
        } else if let Some(st) = field(line, "status=") {
            let t = by_qid.entry(qid.to_string()).or_default();
            t.status.push(st.to_string());
            if let Some(r) = field(line, "relay=") {
                t.relay.push(r.to_string());
            }
        }
    }
    let mut out = HashMap::new();
    for (qid, mid) in mid_of {
        if let Some(mut t) = by_qid.remove(&qid) {
            t.queue_id = qid;
            out.insert(mid, t);
        }
    }
    out
}

/// What the canary's check found, as one line for the operator; None when all is well.
pub fn canary_problem(trace: Option<&MailTrace>, comms_status: Option<&str>) -> Option<String> {
    match (comms_status, trace) {
        (Some(st), _) if st != "sent" => Some(format!("COMMS marked the daily mail {st}")),
        (_, None) => Some("the daily mail never reached the mail server (no Message-ID in the mail log)".into()),
        (_, Some(t)) if !t.signed => Some(format!("the daily mail left unsigned (no DKIM signature; queue {})", t.queue_id)),
        (_, Some(t)) if t.status.is_empty() => Some(format!("the daily mail is still in the mail server's queue ({})", t.queue_id)),
        (_, Some(t)) if !t.delivered() => Some(format!("the daily mail was not delivered: {} (queue {})", t.status.join(", "), t.queue_id)),
        _ => None,
    }
}

/// A delivery receipt worth an alert (failed, partly sent, or skipped, in production): (message id, status, error).
pub fn receipt_problem(fields: &HashMap<String, String>) -> Option<(String, String, String)> {
    if fields.get("cmd").map(String::as_str) != Some("comms.deliver") {
        return None;
    }
    let st = fields.get("st").map(String::as_str).unwrap_or("");
    if !matches!(st, "failed" | "partial_sent" | "skipped") {
        return None;
    }
    let cid = fields.get("cid").cloned().unwrap_or_else(|| "?".into());
    let err = fields.get("e").map(|e| e.chars().take(200).collect::<String>()).unwrap_or_default();
    Some((cid, st.replace('_', " "), err))
}

/// Why a message was not delivered, in words, with what fixes it. `bot_site` is the operators' own site.
pub fn explain(site: &str, err: &str, bot_site: &str) -> String {
    let channel = ["email", "telegram", "sms"].into_iter().find(|c| err.contains(&format!("[{c}]")));
    let named = if channel == Some("email") { "email" } else if channel == Some("telegram") { "Telegram" } else { "SMS" };
    if err.contains("not configured") {
        return if site == bot_site {
            format!("the operators' {named} is not set up (no {named} channel in {{{bot_site}}}:comms:config)")
        } else if channel == Some("telegram") {
            format!("{site} has no Telegram channel; a message meant for the operators must be addressed to them (to: operator)")
        } else {
            format!("{site} has no {named} channel set up")
        };
    }
    if err.contains("no_recipients") {
        return format!("{site}'s {named} channel has no recipient for this kind of message");
    }
    if err.contains("nonprod_dry_run") {
        return format!("not sent on purpose: {site} is not in production");
    }
    if err.is_empty() { "no reason recorded".into() } else { err.to_string() }
}

/// One line per message (not per attempt): the problems of the receipts read, their attempts counted.
pub fn problem_lines(problems: &[(String, String, String, String)], subjects: &HashMap<(String, String), String>, bot_site: &str) -> Vec<String> {
    let mut order: Vec<(String, String)> = Vec::new();
    let mut last: HashMap<(String, String), (String, String, usize)> = HashMap::new();
    for (site, cid, st, err) in problems {
        let k = (site.clone(), cid.clone());
        let e = last.entry(k.clone()).or_insert_with(|| { order.push(k.clone()); (String::new(), String::new(), 0) });
        *e = (st.clone(), err.clone(), e.2 + 1);
    }
    order.into_iter().map(|k| {
        let (st, err, n) = &last[&k];
        let what = subjects.get(&k).map(|s| format!("\"{s}\"")).unwrap_or_else(|| format!("message {}", k.1));
        let tries = if *n > 1 { format!(" ({n} attempts)") } else { String::new() };
        format!("{}: {what} {st}: {}{tries}", k.0, explain(&k.0, err, bot_site))
    }).collect()
}

/// What a job's status file (`key=value` lines: `what`, `max_age_h`, `ok_ms`, `run_ms`, `error`) says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobStatus {
    pub what: String,
    pub max_age_h: u64,
    pub ok_ms: u64,
    pub run_ms: u64,
    pub error: String,
}

pub fn parse_job_status(name: &str, text: &str) -> JobStatus {
    let mut j = JobStatus { what: name.to_string(), max_age_h: 26, ..Default::default() };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "what" if !v.is_empty() => j.what = v.to_string(),
            "max_age_h" => j.max_age_h = v.parse().unwrap_or(26),
            "ok_ms" => j.ok_ms = v.parse().unwrap_or(0),
            "run_ms" => j.run_ms = v.parse().unwrap_or(0),
            "error" => j.error = v.chars().take(300).collect(),
            _ => {}
        }
    }
    j
}

/// What was last said about a job, so a failed run alerts once and a stale job once a day.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JobSeen {
    #[serde(default)]
    pub failed_run_ms: u64,
    #[serde(default)]
    pub stale_alerted_ms: u64,
}

fn utc(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_else(|| "never".into())
}

pub fn job_alert(j: &JobStatus, seen: &mut JobSeen, now_ms: u64) -> Option<String> {
    if !j.error.is_empty() && j.run_ms > j.ok_ms {
        if seen.failed_run_ms == j.run_ms {
            return None;
        }
        seen.failed_run_ms = j.run_ms;
        return Some(format!("{} failed at {}: {}", j.what, utc(j.run_ms), j.error));
    }
    if now_ms.saturating_sub(j.ok_ms) > j.max_age_h * 3_600_000 {
        if now_ms.saturating_sub(seen.stale_alerted_ms) < 86_400_000 {
            return None;
        }
        seen.stale_alerted_ms = now_ms;
        let last = if j.ok_ms == 0 { "never succeeded".to_string() } else { format!("last succeeded {}", utc(j.ok_ms)) };
        return Some(format!("{}: no success in {} h ({last})", j.what, j.max_age_h));
    }
    seen.stale_alerted_ms = 0;
    None
}

/// Per-site counts for the daily summary.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tally {
    pub sent: u64,
    pub failed: u64,
    pub skipped: u64,
}

/// Messages per site by how they ended: a message retried five times counts once, as its last receipt says.
pub fn tally(receipts: &[(String, HashMap<String, String>)]) -> std::collections::BTreeMap<String, Tally> {
    let mut last: std::collections::BTreeMap<(String, String), String> = Default::default();
    for (n, (site, f)) in receipts.iter().enumerate() {
        if f.get("cmd").map(String::as_str) != Some("comms.deliver") {
            continue;
        }
        let cid = f.get("cid").cloned().unwrap_or_else(|| format!("#{n}"));
        let st = f.get("st").cloned().unwrap_or_default();
        let e = last.entry((site.clone(), cid)).or_default();
        if e.as_str() != "sent" {
            *e = st;
        }
    }
    let mut out: std::collections::BTreeMap<String, Tally> = Default::default();
    for ((site, _), st) in last {
        let t = out.entry(site).or_default();
        match st.as_str() {
            "sent" => t.sent += 1,
            "failed" | "partial_sent" => t.failed += 1,
            "skipped" => t.skipped += 1,
            _ => {}
        }
    }
    out
}

pub fn summary_text(day: &str, tallies: &std::collections::BTreeMap<String, Tally>) -> String {
    let mut lines = vec![format!("COMMS, {day}: what each site sent")];
    if tallies.is_empty() {
        lines.push("nothing was sent".into());
    }
    for (site, t) in tallies {
        lines.push(format!("{site}: {} delivered, {} not delivered, {} skipped", t.sent, t.failed, t.skipped));
    }
    lines.push(String::new());
    lines.push("This mail is also the canary: the watcher checks within the hour that it left this server signed and was delivered.".into());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
Oct 26 10:32:28 aesir-solutions postfix/cleanup[1490]: 3F09C121AD6: message-id=<comms.ab12-cd34@geodineum.com>
Oct 26 10:32:28 aesir-solutions opendkim[884]: 3F09C121AD6: DKIM-Signature field added (s=default, d=geodineum.com)
Oct 26 10:32:28 aesir-solutions postfix/qmgr[3659]: 3F09C121AD6: from=<noreply@geodineum.com>, size=804, nrcpt=1 (queue active)
Oct 26 10:32:36 aesir-solutions postfix/smtp[1490833]: 3F09C121AD6: to=<op@example.invalid>, relay=mail.protonmail.ch[185.205.70.128]:25, delay=7.8, dsn=2.0.0, status=sent (250 2.0.0 Ok)
Oct 26 10:40:01 aesir-solutions postfix/cleanup[1491]: 51AA0121AD7: message-id=<comms.unsigned@nierto.dev>
Oct 26 10:40:02 aesir-solutions opendkim[884]: 51AA0121AD7: no signing table match for 'x@nierto.dev'
Oct 26 10:40:05 aesir-solutions postfix/smtp[1490834]: 51AA0121AD7: to=<a@example.invalid>, relay=mx.example.invalid[1.2.3.4]:25, dsn=4.4.1, status=deferred (connect timed out)
";

    #[test]
    fn the_mail_log_is_read_by_message_id() {
        let t = parse_mail_log(LOG.lines());
        let ok = &t["comms.ab12-cd34@geodineum.com"];
        assert_eq!(ok.queue_id, "3F09C121AD6");
        assert!(ok.signed && ok.delivered());
        assert_eq!(ok.relay, vec!["mail.protonmail.ch[185.205.70.128]:25"]);
        let bad = &t["comms.unsigned@nierto.dev"];
        assert!(!bad.signed && !bad.delivered());
        assert_eq!(bad.status, vec!["deferred"]);
    }

    #[test]
    fn the_canary_says_what_is_missing() {
        let t = parse_mail_log(LOG.lines());
        assert_eq!(canary_problem(t.get("comms.ab12-cd34@geodineum.com"), Some("sent")), None);
        assert!(canary_problem(t.get("comms.unsigned@nierto.dev"), Some("sent")).unwrap().contains("unsigned"));
        assert!(canary_problem(None, Some("sent")).unwrap().contains("never reached"));
        assert!(canary_problem(None, Some("failed")).unwrap().contains("failed"));
        let queued = MailTrace { queue_id: "Q".into(), signed: true, status: vec![], relay: vec![] };
        assert!(canary_problem(Some(&queued), None).unwrap().contains("still in the mail server's queue"));
    }

    fn rec(cmd: &str, st: &str) -> HashMap<String, String> {
        HashMap::from([("cmd".into(), cmd.into()), ("st".into(), st.into()), ("cid".into(), "1-0".into())])
    }

    #[test]
    fn failed_partial_and_skipped_deliveries_alert_and_sent_ones_do_not() {
        assert_eq!(receipt_problem(&rec("comms.deliver", "failed")).unwrap().1, "failed");
        assert_eq!(receipt_problem(&rec("comms.deliver", "partial_sent")).unwrap().1, "partial sent");
        assert!(receipt_problem(&rec("comms.deliver", "skipped")).is_some());
        assert!(receipt_problem(&rec("comms.deliver", "sent")).is_none());
        assert!(receipt_problem(&rec("relay.reply", "failed")).is_none());
    }

    #[test]
    fn a_message_retried_five_times_is_one_line_in_words() {
        let err = "Channel error [email]: Channel not configured".to_string();
        let mut p: Vec<(String, String, String, String)> = (0..5).map(|_| ("geodineum".into(), "17-0".into(), "failed".into(), err.clone())).collect();
        p.push(("nierto_com".into(), "18-0".into(), "failed".into(), "Channel error [telegram]: Channel not configured".into()));
        let subjects = HashMap::from([(("geodineum".to_string(), "17-0".to_string()), "[account watch] aesir: nothing new".to_string())]);
        let lines = problem_lines(&p, &subjects, "geodineum");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "geodineum: \"[account watch] aesir: nothing new\" failed: the operators' email is not set up (no email channel in {geodineum}:comms:config) (5 attempts)");
        assert!(lines[1].starts_with("nierto_com: message 18-0 failed: nierto_com has no Telegram channel"), "{}", lines[1]);
        assert!(explain("a_com", "Channel error [email]: no_recipients", "geodineum").contains("no recipient"));
    }

    #[test]
    fn the_summary_counts_per_site() {
        let with = |cid: &str, st: &str| { let mut r = rec("comms.deliver", st); r.insert("cid".into(), cid.into()); r };
        let rs = vec![("a_com".to_string(), with("1-0", "sent")), ("a_com".into(), with("2-0", "failed")), ("a_com".into(), with("2-0", "failed")),
            ("a_com".into(), with("3-0", "failed")), ("a_com".into(), with("3-0", "sent")),
            ("b_com".into(), with("4-0", "skipped")), ("b_com".into(), rec("other", "sent"))];
        let t = tally(&rs);
        assert_eq!(t["a_com"], Tally { sent: 2, failed: 1, skipped: 0 }, "a retried message counts once, as it ended");
        assert_eq!(t["b_com"], Tally { sent: 0, failed: 0, skipped: 1 });
        assert!(summary_text("2026-10-26", &t).contains("b_com: 0 delivered, 0 not delivered, 1 skipped"));
    }

    const H: u64 = 3_600_000;
    const T0: u64 = 1_790_000_000_000;

    #[test]
    fn a_job_status_file_is_read_with_defaults() {
        let j = parse_job_status("sites-backup", "what=sites backup\nok_ms=5\nrun_ms=5\nerror=\nnoise\n");
        assert_eq!(j, JobStatus { what: "sites backup".into(), max_age_h: 26, ok_ms: 5, run_ms: 5, error: String::new() });
        assert_eq!(parse_job_status("x", "").what, "x");
    }

    #[test]
    fn a_failed_run_alerts_once() {
        let j = JobStatus { what: "sites backup".into(), max_age_h: 26, ok_ms: T0, run_ms: T0 + 24 * H, error: "upload".into() };
        let mut seen = JobSeen::default();
        assert!(job_alert(&j, &mut seen, T0 + 25 * H).unwrap().contains("sites backup failed at"));
        assert_eq!(job_alert(&j, &mut seen, T0 + 26 * H - 1), None);
        let next = JobStatus { run_ms: T0 + 48 * H, ..j.clone() };
        assert!(job_alert(&next, &mut seen, T0 + 48 * H).is_some());
    }

    #[test]
    fn a_job_without_a_recent_success_alerts_once_a_day_and_clears() {
        let j = JobStatus { what: "sites backup".into(), max_age_h: 26, ok_ms: T0, run_ms: T0, error: String::new() };
        let mut seen = JobSeen::default();
        assert_eq!(job_alert(&j, &mut seen, T0 + 25 * H), None);
        assert!(job_alert(&j, &mut seen, T0 + 27 * H).unwrap().contains("no success in 26 h"));
        assert_eq!(job_alert(&j, &mut seen, T0 + 30 * H), None);
        assert!(job_alert(&j, &mut seen, T0 + 52 * H).is_some());
        let ok = JobStatus { ok_ms: T0 + 53 * H, run_ms: T0 + 53 * H, ..j.clone() };
        assert_eq!(job_alert(&ok, &mut seen, T0 + 54 * H), None);
        assert_eq!(seen.stale_alerted_ms, 0);
        let never = JobStatus { ok_ms: 0, run_ms: 0, ..j };
        assert!(job_alert(&never, &mut JobSeen::default(), T0).unwrap().contains("never succeeded"));
    }
}

// ----- the run: ValKey, the mail log, Telegram -----

/// Where the watcher looks and whom it tells.
pub struct WatchOptions {
    /// The site that holds the bot and receives the daily mail (COMMS_INBOUND_SITE).
    pub site: String,
    pub environment: String,
    pub topology: String,
    pub node: String,
    pub mail_logs: Vec<std::path::PathBuf>,
    pub bot_token: Option<String>,
    pub chat: Option<String>,
    /// UTC hour from which the day's summary and canary go out.
    pub digest_hour: u32,
    /// false: print what would be sent (delivery off).
    pub send: bool,
    /// Nightly jobs' status files (`<job>.status`), read by the watcher's group.
    pub jobs_dir: std::path::PathBuf,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct WatchState {
    #[serde(default)]
    cursors: HashMap<String, String>,
    #[serde(default)]
    canary: Option<Canary>,
    #[serde(default)]
    digest_date: Option<String>,
    #[serde(default)]
    alerts: Vec<(u64, String)>,
    #[serde(default)]
    heartbeat_alerted_ms: u64,
    #[serde(default)]
    jobs: HashMap<String, JobSeen>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Canary {
    id: String,
    entry: String,
    sent_ms: u64,
}

fn state_key(site: &str) -> String {
    format!("{{{}}}:gnode:comms:watch", site)
}

fn day_of(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

fn read_logs(paths: &[std::path::PathBuf]) -> HashMap<String, MailTrace> {
    let mut text = String::new();
    for p in paths.iter().rev() {
        if let Ok(s) = std::fs::read_to_string(p) {
            text.push_str(&s);
        }
    }
    parse_mail_log(text.lines())
}

/// The trace of a COMMS message by its id (the Message-ID is `<comms.<id>@domain>`).
pub fn trace_for<'a>(traces: &'a HashMap<String, MailTrace>, comms_id: &str) -> Option<&'a MailTrace> {
    let prefix = crate::channels::email::message_id_for(comms_id, "").trim_start_matches('<').trim_end_matches('>').to_string();
    traces.iter().find(|(mid, _)| mid.starts_with(&prefix)).map(|(_, t)| t)
}

async fn xrange_after(conn: &mut redis::aio::MultiplexedConnection, key: &str, after: &str, count: usize) -> Vec<(String, HashMap<String, String>)> {
    let start = format!("({after}");
    let v: redis::RedisResult<Vec<(String, HashMap<String, String>)>> =
        redis::cmd("XRANGE").arg(key).arg(&start).arg("+").arg("COUNT").arg(count).query_async(conn).await;
    v.unwrap_or_default()
}

async fn telegram(opts: &WatchOptions, text: &str) -> bool {
    let (Some(token), Some(chat)) = (opts.bot_token.as_deref(), opts.chat.as_deref()) else { return false };
    if !opts.send {
        println!("[would send to Telegram]\n{text}");
        return true;
    }
    let url = format!("{}/bot{token}/sendMessage", crate::telegram_api());
    reqwest::Client::new().post(&url).json(&serde_json::json!({"chat_id": chat, "text": text}))
        .timeout(std::time::Duration::from_secs(20)).send().await.map(|r| r.status().is_success()).unwrap_or(false)
}

/// One watch: returns the alerts it raised (already sent when a bot is configured).
pub async fn run(conn: &mut redis::aio::MultiplexedConnection, opts: &WatchOptions, now_ms: u64) -> anyhow::Result<Vec<String>> {
    let key = state_key(&opts.site);
    let raw: Option<String> = redis::cmd("GET").arg(&key).query_async(conn).await?;
    let mut st: WatchState = raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default();
    let mut alerts: Vec<String> = Vec::new();

    // COMMS itself
    let hb = format!("{{{}}}:gnode:heartbeat:{}:comms:{}", opts.topology, opts.environment, opts.node);
    let alive: i64 = redis::cmd("EXISTS").arg(&hb).query_async(conn).await.unwrap_or(0);
    if alive == 0 {
        if now_ms.saturating_sub(st.heartbeat_alerted_ms) > 6 * 3_600_000 {
            alerts.push(format!("COMMS is not running on {} (no heartbeat): no mail or Telegram leaves this server", opts.node));
            st.heartbeat_alerted_ms = now_ms;
        }
    } else {
        st.heartbeat_alerted_ms = 0;
    }

    // Every production site's delivery receipts since the last look
    let streams = crate::valkey_scan::scan_keys(conn, &format!("{{*}}:gnode:receipts:{}", opts.environment)).await.unwrap_or_default();
    let mut yesterday: Vec<(String, HashMap<String, String>)> = Vec::new();
    let mut problems: Vec<(String, String, String, String)> = Vec::new();
    let day_start = now_ms / 86_400_000 * 86_400_000;
    for stream in &streams {
        let site = stream.trim_start_matches('{').split('}').next().unwrap_or("").to_string();
        let Some(after) = st.cursors.get(&site).cloned() else {
            // first look: start from now, no history replayed as alerts
            let last: Vec<(String, HashMap<String, String>)> =
                redis::cmd("XREVRANGE").arg(stream).arg("+").arg("-").arg("COUNT").arg(1).query_async(conn).await.unwrap_or_default();
            st.cursors.insert(site.clone(), last.first().map(|(id, _)| id.clone()).unwrap_or_else(|| format!("{now_ms}-0")));
            continue;
        };
        let mut cursor = after;
        loop {
            let page = xrange_after(conn, stream, &cursor, 500).await;
            for (id, f) in &page {
                if let Some((cid, st, err)) = receipt_problem(f) {
                    problems.push((site.clone(), cid, st, err));
                }
                cursor = id.clone();
            }
            if page.len() < 500 {
                break;
            }
        }
        st.cursors.insert(site.clone(), cursor);
        // for the summary: yesterday's receipts, read by their stream ids
        let y0 = day_start.saturating_sub(86_400_000);
        let ys: Vec<(String, HashMap<String, String>)> = redis::cmd("XRANGE").arg(stream).arg(format!("{y0}-0")).arg(format!("{}-0", day_start - 1))
            .arg("COUNT").arg(10_000).query_async(conn).await.unwrap_or_default();
        yesterday.extend(ys.into_iter().map(|(_, f)| (site.clone(), f)));
    }

    // Each problem once, named by the subject the producer gave it
    let mut subjects: HashMap<(String, String), String> = HashMap::new();
    for (site, cid, _, _) in &problems {
        let k = (site.clone(), cid.clone());
        if subjects.contains_key(&k) || !cid.contains('-') {
            continue;
        }
        let stream = format!("{{{site}}}:gnode:comms:{}", opts.environment);
        let e: Vec<(String, HashMap<String, String>)> =
            redis::cmd("XRANGE").arg(&stream).arg(cid).arg(cid).query_async(conn).await.unwrap_or_default();
        let subject = e.first().and_then(|(_, f)| f.get("content")).and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
            .and_then(|v| v.get("subject").and_then(|s| s.as_str()).map(|s| s.chars().take(80).collect::<String>()));
        if let Some(s) = subject {
            subjects.insert(k, s);
        }
    }
    alerts.extend(problem_lines(&problems, &subjects, &opts.site));

    // Nightly jobs outside COMMS
    if let Ok(dir) = std::fs::read_dir(&opts.jobs_dir) {
        let mut names: Vec<String> = Vec::new();
        for e in dir.flatten() {
            let path = e.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".status")).map(String::from) else { continue };
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(err) => format!("error=cannot read {}: {err}\nrun_ms={}", path.display(), now_ms / 86_400_000 * 86_400_000),
            };
            let seen = st.jobs.entry(name.clone()).or_default();
            if let Some(a) = job_alert(&parse_job_status(&name, &text), seen, now_ms) {
                alerts.push(a);
            }
            names.push(name);
        }
        st.jobs.retain(|k, _| names.contains(k));
    }

    // Yesterday's canary, read back from the mail log
    if let Some(c) = st.canary.clone() {
        if now_ms.saturating_sub(c.sent_ms) >= 20 * 60_000 {
            let traces = read_logs(&opts.mail_logs);
            let status_key = format!("{{{}}}:gnode:comms:status:{}", opts.site, c.entry);
            let status: Option<String> = redis::cmd("HGET").arg(&status_key).arg("status").query_async(conn).await.unwrap_or(None);
            let problem = canary_problem(trace_for(&traces, &c.id), status.as_deref());
            let waited = now_ms.saturating_sub(c.sent_ms);
            match problem {
                None => st.canary = None,
                Some(p) if waited > 3 * 3_600_000 => {
                    alerts.push(format!("Daily mail check: {p}. It goes to the operators' email, the email channel of site {}: without one nothing can send it.", opts.site));
                    st.canary = None;
                }
                Some(_) => {}
            }
        }
    }

    // Once a day: the summary mail (the canary) and the line that says the watcher lives
    let today = day_of(now_ms);
    let hour = ((now_ms / 3_600_000) % 24) as u32;
    if hour >= opts.digest_hour && st.digest_date.as_deref() != Some(today.as_str()) {
        let tallies = tally(&yesterday);
        let body = summary_text(&day_of(day_start.saturating_sub(86_400_000)), &tallies);
        let id = format!("watch-{}-{}", today, now_ms % 100_000);
        let fields: Vec<(String, String)> = vec![
            ("id".into(), id.clone()),
            ("type".into(), "system".into()),
            ("timestamp".into(), chrono::Utc::now().to_rfc3339()),
            ("site_id".into(), opts.site.clone()),
            ("environment".into(), opts.environment.clone()),
            ("priority".into(), "2".into()),
            ("sender".into(), serde_json::json!({"name": "COMMS watch"}).to_string()),
            ("content".into(), serde_json::json!({"subject": format!("COMMS daily, {today}"), "body": body}).to_string()),
            ("metadata".into(), serde_json::json!({"form_type": "comms.watch", "environment": opts.environment}).to_string()),
            ("dispatch".into(), serde_json::json!({"channels": ["email"], "status": "pending", "attempts": 0}).to_string()),
        ];
        let stream = format!("{{{}}}:gnode:comms:{}", opts.site, opts.environment);
        let entry: String = redis::cmd("XADD").arg(&stream).arg("*").arg(&fields).query_async(conn).await?;
        st.canary = Some(Canary { id, entry, sent_ms: now_ms });
        let recent = st.alerts.iter().filter(|(t, _)| now_ms.saturating_sub(*t) < 86_400_000).count() + alerts.len();
        let sent: u64 = tallies.values().map(|t| t.sent).sum();
        let failed: u64 = tallies.values().map(|t| t.failed + t.skipped).sum();
        let line = if recent == 0 {
            format!("COMMS watch, {today}: all well. Yesterday COMMS delivered {sent} messages for {} sites, {failed} not delivered. The daily mail is on its way; it is checked within the hour.", tallies.len())
        } else {
            format!("COMMS watch, {today}: {recent} problems in the last day (listed above). Yesterday COMMS delivered {sent} messages, {failed} not delivered.")
        };
        telegram(opts, &line).await;
        st.digest_date = Some(today);
    }

    if !alerts.is_empty() {
        let text = format!("COMMS watch on {}:\n{}", opts.node, alerts.iter().map(|a| format!("• {a}")).collect::<Vec<_>>().join("\n"));
        telegram(opts, &text).await;
        for a in &alerts {
            st.alerts.push((now_ms, a.chars().take(200).collect()));
        }
    }
    st.alerts.retain(|(t, _)| now_ms.saturating_sub(*t) < 7 * 86_400_000);
    st.alerts.truncate(200);
    let _: () = redis::cmd("SET").arg(&key).arg(serde_json::to_string(&st)?).query_async(conn).await?;
    Ok(alerts)
}
