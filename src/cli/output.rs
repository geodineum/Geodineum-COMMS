//! CLI output formatting

use crate::persistence::{ArchivedMessage, MessageStats};
use std::io::{self, Write};

/// Output format for CLI commands
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Table,
    Json,
    Csv,
}

impl OutputFormat {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "json" => OutputFormat::Json,
            "csv" => OutputFormat::Csv,
            _ => OutputFormat::Table,
        }
    }
}

/// Table printer for CLI output
pub struct TablePrinter;

impl TablePrinter {
    /// Print sites list
    pub fn print_sites(sites: &[String], format: OutputFormat) -> io::Result<()> {
        let stdout = io::stdout();
        let mut handle = stdout.lock();

        match format {
            OutputFormat::Json => {
                writeln!(handle, "{}", serde_json::to_string_pretty(sites).unwrap_or_default())?;
            }
            OutputFormat::Csv => {
                writeln!(handle, "site_id")?;
                for site in sites {
                    writeln!(handle, "{}", site)?;
                }
            }
            OutputFormat::Table => {
                writeln!(handle, "\n{:=^60}", " Registered Sites ")?;
                writeln!(handle, "")?;
                if sites.is_empty() {
                    writeln!(handle, "  No sites found.")?;
                } else {
                    for (i, site) in sites.iter().enumerate() {
                        writeln!(handle, "  {}. {}", i + 1, site)?;
                    }
                }
                writeln!(handle, "")?;
                writeln!(handle, "Total: {} sites", sites.len())?;
                writeln!(handle, "{:=^60}", "")?;
            }
        }

        Ok(())
    }

    /// Print messages list
    pub fn print_messages(
        messages: &[ArchivedMessage],
        format: OutputFormat,
        verbose: bool,
    ) -> io::Result<()> {
        let stdout = io::stdout();
        let mut handle = stdout.lock();

        match format {
            OutputFormat::Json => {
                let output: Vec<serde_json::Value> = messages
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "id": m.id,
                            "site_id": m.site_id,
                            "stream_id": m.stream_id,
                            "environment": m.environment,
                            "message_type": m.message_type,
                            "priority": m.priority,
                            "status": m.status.as_str(),
                            "attempts": m.attempts,
                            "spam_score": m.spam_score,
                            "received_at": m.received_at,
                            "updated_at": m.updated_at,
                            "completed_at": m.completed_at,
                            "sender": m.sender(),
                            "content": m.content(),
                        })
                    })
                    .collect();
                writeln!(handle, "{}", serde_json::to_string_pretty(&output).unwrap_or_default())?;
            }
            OutputFormat::Csv => {
                writeln!(
                    handle,
                    "id,site_id,type,status,attempts,received_at,sender_email,subject"
                )?;
                for msg in messages {
                    let sender = msg.sender();
                    let content = msg.content();
                    writeln!(
                        handle,
                        "{},{},{},{},{},{},{},\"{}\"",
                        msg.id,
                        msg.site_id,
                        msg.message_type,
                        msg.status.as_str(),
                        msg.attempts,
                        msg.received_at,
                        sender.as_ref().and_then(|s| s.email.as_ref()).unwrap_or(&String::new()),
                        content.as_ref().and_then(|c| c.subject.as_ref()).unwrap_or(&String::new()).replace("\"", "\"\""),
                    )?;
                }
            }
            OutputFormat::Table => {
                writeln!(handle, "\n{:=^100}", " Messages ")?;
                writeln!(handle, "")?;

                if messages.is_empty() {
                    writeln!(handle, "  No messages found.")?;
                } else {
                    // Header
                    writeln!(
                        handle,
                        "  {:<12} {:<20} {:<14} {:<10} {:<8} {:<20}",
                        "ID", "SITE", "TYPE", "STATUS", "ATTEMPTS", "RECEIVED"
                    )?;
                    writeln!(handle, "  {}", "-".repeat(94))?;

                    for msg in messages {
                        let short_id = if msg.id.len() > 10 {
                            format!("{}...", &msg.id[..10])
                        } else {
                            msg.id.clone()
                        };

                        let short_site = if msg.site_id.len() > 18 {
                            format!("{}...", &msg.site_id[..18])
                        } else {
                            msg.site_id.clone()
                        };

                        writeln!(
                            handle,
                            "  {:<12} {:<20} {:<14} {:<10} {:<8} {:<20}",
                            short_id,
                            short_site,
                            msg.message_type,
                            msg.status.as_str(),
                            msg.attempts,
                            &msg.received_at[..19.min(msg.received_at.len())],
                        )?;

                        if verbose {
                            let sender = msg.sender();
                            let content = msg.content();
                            if let Some(s) = sender {
                                if let Some(email) = &s.email {
                                    writeln!(handle, "    From: {}", email)?;
                                }
                            }
                            if let Some(c) = content {
                                if let Some(subject) = &c.subject {
                                    let truncated = if subject.len() > 60 {
                                        format!("{}...", &subject[..60])
                                    } else {
                                        subject.clone()
                                    };
                                    writeln!(handle, "    Subject: {}", truncated)?;
                                }
                            }
                            writeln!(handle, "")?;
                        }
                    }
                }

                writeln!(handle, "")?;
                writeln!(handle, "Total: {} messages", messages.len())?;
                writeln!(handle, "{:=^100}", "")?;
            }
        }

        Ok(())
    }

    /// Print message detail
    pub fn print_message_detail(msg: &ArchivedMessage, format: OutputFormat) -> io::Result<()> {
        let stdout = io::stdout();
        let mut handle = stdout.lock();

        match format {
            OutputFormat::Json => {
                let output = serde_json::json!({
                    "id": msg.id,
                    "site_id": msg.site_id,
                    "stream_id": msg.stream_id,
                    "environment": msg.environment,
                    "message_type": msg.message_type,
                    "priority": msg.priority,
                    "status": msg.status.as_str(),
                    "attempts": msg.attempts,
                    "spam_score": msg.spam_score,
                    "received_at": msg.received_at,
                    "updated_at": msg.updated_at,
                    "completed_at": msg.completed_at,
                    "sender": msg.sender(),
                    "content": msg.content(),
                    "channel_results": msg.channel_results(),
                });
                writeln!(handle, "{}", serde_json::to_string_pretty(&output).unwrap_or_default())?;
            }
            OutputFormat::Table | OutputFormat::Csv => {
                writeln!(handle, "\n{:=^80}", " Message Detail ")?;
                writeln!(handle, "")?;
                writeln!(handle, "  ID:           {}", msg.id)?;
                writeln!(handle, "  Site:         {}", msg.site_id)?;
                writeln!(handle, "  Stream ID:    {}", msg.stream_id)?;
                writeln!(handle, "  Environment:  {}", msg.environment)?;
                writeln!(handle, "  Type:         {}", msg.message_type)?;
                writeln!(handle, "  Priority:     {}", msg.priority)?;
                writeln!(handle, "  Status:       {}", msg.status.as_str())?;
                writeln!(handle, "  Attempts:     {}", msg.attempts)?;
                if let Some(score) = msg.spam_score {
                    writeln!(handle, "  Spam Score:   {:.2}", score)?;
                }
                writeln!(handle, "  Received:     {}", msg.received_at)?;
                writeln!(handle, "  Updated:      {}", msg.updated_at)?;
                if let Some(ref completed) = msg.completed_at {
                    writeln!(handle, "  Completed:    {}", completed)?;
                }

                writeln!(handle, "")?;
                writeln!(handle, "  {:─^76}", " Sender ")?;
                if let Some(sender) = msg.sender() {
                    if let Some(ref name) = sender.name {
                        writeln!(handle, "  Name:         {}", name)?;
                    }
                    if let Some(ref email) = sender.email {
                        writeln!(handle, "  Email:        {}", email)?;
                    }
                    if let Some(ref phone) = sender.phone {
                        writeln!(handle, "  Phone:        {}", phone)?;
                    }
                }

                writeln!(handle, "")?;
                writeln!(handle, "  {:─^76}", " Content ")?;
                if let Some(content) = msg.content() {
                    if let Some(ref subject) = content.subject {
                        writeln!(handle, "  Subject:      {}", subject)?;
                    }
                    if let Some(ref body) = content.body {
                        writeln!(handle, "  Body:")?;
                        for line in body.lines().take(10) {
                            writeln!(handle, "    {}", line)?;
                        }
                        if body.lines().count() > 10 {
                            writeln!(handle, "    ... (truncated)")?;
                        }
                    }
                }

                let results = msg.channel_results();
                if !results.is_empty() {
                    writeln!(handle, "")?;
                    writeln!(handle, "  {:─^76}", " Channel Results ")?;
                    for result in results {
                        let status = if result.success { "OK" } else { "FAILED" };
                        writeln!(
                            handle,
                            "  {:<12} [{:<6}] {}",
                            result.channel,
                            status,
                            result.error.as_deref().unwrap_or("-")
                        )?;
                    }
                }

                writeln!(handle, "")?;
                writeln!(handle, "{:=^80}", "")?;
            }
        }

        Ok(())
    }

    /// Print stats
    pub fn print_stats(stats: &[(String, MessageStats)], format: OutputFormat) -> io::Result<()> {
        let stdout = io::stdout();
        let mut handle = stdout.lock();

        match format {
            OutputFormat::Json => {
                let output: serde_json::Value = if stats.len() == 1 {
                    let (site, s) = &stats[0];
                    serde_json::json!({
                        "site_id": site,
                        "total": s.total,
                        "by_status": {
                            "pending": s.pending,
                            "sent": s.sent,
                            "partial_sent": s.partial_sent,
                            "failed": s.failed,
                            "spam": s.spam,
                        },
                        "by_channel": {
                            "email_sent": s.by_channel.email_sent,
                            "email_failed": s.by_channel.email_failed,
                            "telegram_sent": s.by_channel.telegram_sent,
                            "telegram_failed": s.by_channel.telegram_failed,
                            "sms_sent": s.by_channel.sms_sent,
                            "sms_failed": s.by_channel.sms_failed,
                        },
                        "sent_today": s.sent_today,
                        "sent_this_week": s.sent_this_week,
                    })
                } else {
                    let mut total = MessageStats::default();
                    let by_site: Vec<serde_json::Value> = stats
                        .iter()
                        .map(|(site, s)| {
                            total.total += s.total;
                            total.pending += s.pending;
                            total.sent += s.sent;
                            total.partial_sent += s.partial_sent;
                            total.failed += s.failed;
                            total.spam += s.spam;
                            total.sent_today += s.sent_today;
                            total.sent_this_week += s.sent_this_week;
                            total.by_channel.email_sent += s.by_channel.email_sent;
                            total.by_channel.email_failed += s.by_channel.email_failed;
                            total.by_channel.telegram_sent += s.by_channel.telegram_sent;
                            total.by_channel.telegram_failed += s.by_channel.telegram_failed;
                            total.by_channel.sms_sent += s.by_channel.sms_sent;
                            total.by_channel.sms_failed += s.by_channel.sms_failed;

                            serde_json::json!({
                                "site_id": site,
                                "total": s.total,
                                "sent": s.sent,
                                "failed": s.failed,
                            })
                        })
                        .collect();

                    serde_json::json!({
                        "global": {
                            "total": total.total,
                            "by_status": {
                                "pending": total.pending,
                                "sent": total.sent,
                                "partial_sent": total.partial_sent,
                                "failed": total.failed,
                                "spam": total.spam,
                            },
                            "by_channel": {
                                "email_sent": total.by_channel.email_sent,
                                "email_failed": total.by_channel.email_failed,
                                "telegram_sent": total.by_channel.telegram_sent,
                                "telegram_failed": total.by_channel.telegram_failed,
                                "sms_sent": total.by_channel.sms_sent,
                                "sms_failed": total.by_channel.sms_failed,
                            },
                            "sent_today": total.sent_today,
                            "sent_this_week": total.sent_this_week,
                        },
                        "by_site": by_site
                    })
                };
                writeln!(handle, "{}", serde_json::to_string_pretty(&output).unwrap_or_default())?;
            }
            OutputFormat::Table | OutputFormat::Csv => {
                let is_global = stats.len() > 1;

                if is_global {
                    writeln!(handle, "\n{:=^70}", " Global Statistics ")?;
                } else if let Some((site, _)) = stats.first() {
                    writeln!(handle, "\n{:=^70}", format!(" Statistics: {} ", site))?;
                }
                writeln!(handle, "")?;

                let mut total = MessageStats::default();

                if is_global {
                    // Per-site summary
                    writeln!(
                        handle,
                        "  {:<25} {:>8} {:>8} {:>8} {:>8}",
                        "SITE", "TOTAL", "SENT", "PENDING", "FAILED"
                    )?;
                    writeln!(handle, "  {}", "-".repeat(65))?;

                    for (site, s) in stats {
                        total.total += s.total;
                        total.pending += s.pending;
                        total.sent += s.sent;
                        total.partial_sent += s.partial_sent;
                        total.failed += s.failed;
                        total.spam += s.spam;
                        total.sent_today += s.sent_today;
                        total.sent_this_week += s.sent_this_week;
                        total.by_channel.email_sent += s.by_channel.email_sent;
                        total.by_channel.email_failed += s.by_channel.email_failed;
                        total.by_channel.telegram_sent += s.by_channel.telegram_sent;
                        total.by_channel.telegram_failed += s.by_channel.telegram_failed;
                        total.by_channel.sms_sent += s.by_channel.sms_sent;
                        total.by_channel.sms_failed += s.by_channel.sms_failed;

                        let short_site = if site.len() > 23 {
                            format!("{}...", &site[..23])
                        } else {
                            site.clone()
                        };

                        writeln!(
                            handle,
                            "  {:<25} {:>8} {:>8} {:>8} {:>8}",
                            short_site, s.total, s.sent, s.pending, s.failed
                        )?;
                    }

                    writeln!(handle, "  {}", "-".repeat(65))?;
                    writeln!(
                        handle,
                        "  {:<25} {:>8} {:>8} {:>8} {:>8}",
                        "TOTAL", total.total, total.sent, total.pending, total.failed
                    )?;
                } else if let Some((_, s)) = stats.first() {
                    total = s.clone();
                }

                // Status breakdown
                writeln!(handle, "")?;
                writeln!(handle, "  {:─^66}", " By Status ")?;
                writeln!(handle, "  Pending:      {:>8}", total.pending)?;
                writeln!(handle, "  Sent:         {:>8}", total.sent)?;
                writeln!(handle, "  Partial Sent: {:>8}", total.partial_sent)?;
                writeln!(handle, "  Failed:       {:>8}", total.failed)?;
                writeln!(handle, "  Spam:         {:>8}", total.spam)?;

                // Time breakdown
                writeln!(handle, "")?;
                writeln!(handle, "  {:─^66}", " Recent Activity ")?;
                writeln!(handle, "  Sent Today:      {:>8}", total.sent_today)?;
                writeln!(handle, "  Sent This Week:  {:>8}", total.sent_this_week)?;

                // Channel breakdown
                writeln!(handle, "")?;
                writeln!(handle, "  {:─^66}", " By Channel ")?;
                writeln!(handle, "  Email:     {:>8} sent, {:>8} failed", total.by_channel.email_sent, total.by_channel.email_failed)?;
                writeln!(handle, "  Telegram:  {:>8} sent, {:>8} failed", total.by_channel.telegram_sent, total.by_channel.telegram_failed)?;
                writeln!(handle, "  SMS:       {:>8} sent, {:>8} failed", total.by_channel.sms_sent, total.by_channel.sms_failed)?;

                writeln!(handle, "")?;
                writeln!(handle, "{:=^70}", "")?;
            }
        }

        Ok(())
    }
}
