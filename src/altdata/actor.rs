use std::time::Duration;
use crate::bus::types::Bus;
use crate::core::types::{Actor, RawNews, PolyMarketEvent};
use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use reqwest::Client;
use futures::future::join_all;
use tokio::time::interval;

pub struct AltDataActor {
    pub bus: Bus,
    pub client: Client,
    pub shutdown: CancellationToken
}

impl AltDataActor {
    const POLY_MARKET_GAMMA: &'static str = "https://gamma-api.polymarket.com/events";

    pub fn new(bus: Bus, client: Client, shutdown: CancellationToken) -> AltDataActor {
        Self { bus, client, shutdown }
    }

    async fn fetch_news(&self) -> Result<()> {
        // TODO: replace with your real fetch
        // e.g., let body = self.client.get(&self.url).send().await?.bytes().await?;
        Ok(())
    }

    async fn fetch_events_page(&self, offset: u32, limit: u32) -> Result<Vec<PolyMarketEvent>> {
        let res = self.client
            .get(Self::POLY_MARKET_GAMMA)
            .query(&[
                ("order", "id"),
                ("ascending", "false"),
                ("closed", "false"),
                ("limit", &offset.to_string()),
                ("offset", &limit.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<Vec<PolyMarketEvent>>()
            .await?;
        return Ok(res);
    }

    async fn fetch_all_active_polymarket_events(&self) -> Result<Vec<PolyMarketEvent>> {
        let mut rows = Vec::new();
        let mut offset = 0;
        let limit = 100;

        loop {
            let page = self.fetch_events_page(offset, limit).await?;

            if page.is_empty() { break; }
            let len = page.len();
            for ev in page {
                rows.push(ev);
            }
            if len < limit as usize { break; }
            offset += limit;
        }
        Ok(rows)
    }
}

#[async_trait::async_trait]
impl Actor for AltDataActor {
    async fn run(mut self) -> Result<()> {
        info!("AltDataActor started");

        let mut news_tick = interval(Duration::from_secs(3));
        let mut poly_tick = interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                // Graceful shutdown signal
                _ = self.shutdown.cancelled() => {
                    info!("AltDataActor: shutdown requested");
                    break;
                }

                // Fetch news
                // _ = news_tick.tick() => {
                //     match self.fetch_news().await {
                //         Ok(_) => {
                //             // TODO fetch news
                //             let raw_news = RawNews{
                //                 content_hash: "".to_string(),
                //                 source: "".to_string(),
                //                 url: "".to_string(),
                //                 title: "".to_string(),
                //                 lede: "".to_string(),
                //                 ts_ms: 0,
                //                 lang: "".to_string(),
                //             };
                //             self.bus.raw_news.publish(raw_news).await?;
                //         }
                //         Err(e) => { error!("AltDataActor: failed to fetch raw news: {}", e); }
                //     }
                // }

                //Fetch active polymarket events and markets
                _ = poly_tick.tick() => {
                     match self.fetch_all_active_polymarket_events().await  {
                        Ok(poly_event) => {
                           let results= join_all(
                                                            poly_event.into_iter().map(|ev| {
                                                                    let bus = self.bus.clone();
                                                                    async move {
                                                                        bus.polymarket_events.publish(ev).await
                                                                    }
                                                            })
                                                        ).await;
                            results.into_iter().collect::<Result<(), _>>()?;

                        }
                        Err(e) => {
                            error!("AltDataActor: failed to fetch active poly market event: {}", e);}
                    }
                }
            }
        }
        info!("AltDataActor stopped cleanly");
        Ok(())
    }
}