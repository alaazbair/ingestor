use std::time::Duration;
use crate::bus::types::Bus;
use crate::core::types::{Actor, RawNews};
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveTime, TimeZone, Utc};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use reqwest::{header, Client, Url};
use scraper::{Html, Selector};
use tokio::time::interval;
use crate::config::config::{FinJuiceCfg};

pub struct FinJuiceActor {
    pub bus: Bus,
    pub client: Client,
    pub cfg: FinJuiceCfg,
    pub shutdown: CancellationToken
}

/// Parse "HH:MM Mon DD" as **local time** (machine timezone) in the **current local year**,
/// then convert to UTC. Returns None if parsing fails (or on DST ambiguity it picks earliest).
fn parse_fj_time(s: &str) -> Option<DateTime<Utc>> {
    let parts: Vec<_> = s.split_whitespace().collect();
    if parts.len() != 3 { return None; }

    // HH:MM
    let time = NaiveTime::parse_from_str(parts[0], "%H:%M").ok()?;

    // Mon -> month number via a small trick
    let month = NaiveDate::parse_from_str(&format!("{} 01 2000", parts[1]), "%b %d %Y")
        .ok()?
        .month();

    let day: u32 = parts[2].parse().ok()?;
    let year = Local::now().year();

    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let naive = date.and_time(time);

    // Localize; handle potential DST ambiguity by picking earliest if needed.
    let local_dt = match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt,
        chrono::LocalResult::Ambiguous(dt_earliest, _dt_latest) => dt_earliest,
        chrono::LocalResult::None => return None, // nonexistent (spring-forward gap)
    };

    Some(local_dt.with_timezone(&Utc))
}

impl FinJuiceActor {
    pub fn new(bus: Bus, client: Client, cfg: FinJuiceCfg, shutdown: CancellationToken) -> FinJuiceActor {
        Self { bus, client, cfg, shutdown }
    }

    pub async fn fetch_html_and_parse(&self)-> Result<Vec<RawNews>>  {

        let resp = self.client
            .get(&self.cfg.baseUrl)
            .header(header::COOKIE, self.cfg.cookie.to_string())
            .send().await.context("GET FinancialJuice")?;
        let body = resp.error_for_status()?.text().await?;

        self.parse_html(&body)
    }
    fn parse_html(&self, html: &str) -> Result<Vec<RawNews>> {
        let doc = Html::parse_document(html);
        let sel_item = Selector::parse("div.headline-item.infinite-item").unwrap();
        let sel_title = Selector::parse("p.headline-title > span.headline-title-nolink").unwrap();
        let sel_time = Selector::parse("p.time").unwrap();
        let sel_labels = Selector::parse("span.news-label").unwrap();
        let sel_social = Selector::parse("ul.social-nav").unwrap();

        let base = Url::parse(&self.cfg.baseUrl)?;
        let mut out = Vec::new();

        for item in doc.select(&sel_item) {
            // filter ads/sponsored blocks
            let hid = item.value().attr("data-headlineid").unwrap_or("");
            if hid == "0" || hid.is_empty() { continue; }

            // title
            let title = if let Some(s) = item.select(&sel_title).next() {
                s.text().collect::<String>().trim().to_string()
            } else {
                continue; // no title, skip
            };
            if title.is_empty() { continue; }

            // link from social-nav data-link
            let url = if let Some(sn) = item.select(&sel_social).next() {
                if let Some(dl) = sn.value().attr("data-link") {
                    dl.to_string()
                } else { base.as_str().to_string() }
            } else { base.as_str().to_string() };

            // labels
            let labels = item.select(&sel_labels)
                .map(|n| n.text().collect::<String>().trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();

            // time
            let ts_utc = item.select(&sel_time).next()
                .and_then(|n| {
                    let raw = n.text().collect::<String>();
                    // FinancialJuice  use local tz
                    parse_fj_time(raw.trim())
                });

            out.push(RawNews {
                feed: "FinancialJuice".into(),
                title: title,
                description: "".into(),
                url: url,
                labels: labels,
                published: ts_utc,
            });
        }
        Ok(out)
    }
}

#[async_trait::async_trait]
impl Actor for FinJuiceActor {
    async fn run(mut self) -> Result<()> {
        info!("FinJuiceActor started");

        let mut tick = interval(Duration::from_secs(self.cfg.refresh.as_secs()));

        loop {
            tokio::select! {
                // Graceful shutdown signal
                _ = self.shutdown.cancelled() => {
                    info!("FinJuiceActor: shutdown requested");
                    break;
                }

                _ = tick.tick() => {
                    match self.fetch_html_and_parse().await {
                        Ok(events) => {
                            for n in events {
                                if let Err(e) = self.bus.raw_news.publish(n).await {
                                    tracing::warn!(?e, "publish raw news failed");
                                }
                            }
                        }
                         Err(e) => {
                            error!("RssActor: failed to fetch active poly market event: {}", e);
                            // backoff to avoid hot loop on repeated failures
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        }
                    }
                }
            }
        }

        info!("FinJuiceActor stopped cleanly");
        Ok(())
    }
}