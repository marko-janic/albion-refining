use crate::ids::ITEM_IDS;
use crate::market::{city_name, parse_item_id, HistoryPoint, Incoming, IncomingHistory, Market, Window};
use crate::calc::PriceKey;
use crate::App;
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

pub const LIVE_FILE: &str = "market_live.txt";
const FEED_CAPACITY: usize = 500;

#[derive(Default)]
struct FeedInner {
    lines: VecDeque<String>,
    /// Number of lines ever pushed, so readers can ask for "everything after line n".
    total: u64,
}

/// The most recent log lines of the listener, shared with the interactive prompt.
#[derive(Default)]
pub struct Feed {
    inner: Mutex<FeedInner>,
}

impl Feed {
    fn push(&self, line: String) {
        let mut inner = self.inner.lock().unwrap();
        if inner.lines.len() == FEED_CAPACITY {
            inner.lines.pop_front();
        }
        inner.lines.push_back(line);
        inner.total += 1;
    }

    /// Lines pushed after the first `seen` ones, and the new count to pass next time.
    pub fn since(&self, seen: u64) -> (Vec<String>, u64) {
        let inner = self.inner.lock().unwrap();
        let first = inner.total - inner.lines.len() as u64;
        let skip = seen.saturating_sub(first) as usize;
        (inner.lines.iter().skip(skip).cloned().collect(), inner.total)
    }

    /// The last `count` lines and the current count.
    pub fn recent(&self, count: usize) -> (Vec<String>, u64) {
        let inner = self.inner.lock().unwrap();
        let skip = inner.lines.len().saturating_sub(count);
        (inner.lines.iter().skip(skip).cloned().collect(), inner.total)
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn clock() -> String {
    let secs = now() % 86_400;
    format!("{:02}:{:02}:{:02} UTC", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// What the background listener shares with the prompt.
pub struct Listener {
    pub feed: Arc<Feed>,
    /// The market data, also used by the listener while it is receiving orders. Changes made here
    /// (like purging a city) have to go through this copy, or the listener would write the old data back.
    pub market: Arc<Mutex<Market>>,
}

/// Starts a local HTTP server in a background thread that receives what the Albion Data
/// client captures. Returns None when the port is not available.
pub fn start(port: u16) -> Option<Listener> {
    let server = match tiny_http::Server::http(("127.0.0.1", port)) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("Could not listen on port {port} ({e}). Is another copy of this program running?");
            eprintln!("The market listener is off; change \"ingest_port\" in config.json to use another port.");
            return None;
        }
    };
    println!("Market listener running in the background on http://localhost:{port}");
    println!("  Start the Albion Data client with: albiondata-client.exe -i http://localhost:{port}");
    println!("  Type 'live' to watch incoming market data; prices also go to {LIVE_FILE}.");

    let listener = Listener { feed: Arc::new(Feed::default()), market: Arc::new(Mutex::new(Market::load())) };
    let feed = Arc::clone(&listener.feed);
    let market = Arc::clone(&listener.market);
    thread::spawn(move || serve(server, &feed, &market));
    Some(listener)
}

fn serve(server: tiny_http::Server, feed: &Feed, shared: &Mutex<Market>) {
    write_live(&shared.lock().unwrap());

    let mut ignored_topics = HashSet::new();
    let mut history_note_shown = false;
    for mut request in server.incoming_requests() {
        let topic = request.url().trim_start_matches('/').to_string();
        let mut body = Vec::new();
        let _ = request.as_reader().read_to_end(&mut body);
        let _ = request.respond(tiny_http::Response::empty(200));

        if topic.starts_with("markethistories.ingest") {
            match parse_history(&body) {
                Ok(Some(history)) => {
                    let mut market = shared.lock().unwrap();
                    let line = market.ingest_history(history, now());
                    feed.push(format!("[{}] {line}", clock()));
                    market.save();
                    write_live(&market);
                }
                Ok(None) => {
                    if !history_note_shown {
                        history_note_shown = true;
                        feed.push(format!("[{}] ignoring price history of items that are not refining resources", clock()));
                    }
                }
                Err(e) => {
                    let sample: String = String::from_utf8_lossy(&body).chars().take(300).collect();
                    feed.push(format!("[{}] could not read a price history message ({e}): {sample}", clock()));
                }
            }
            continue;
        }
        if !topic.starts_with("marketorders.ingest") {
            if ignored_topics.insert(topic.clone()) {
                feed.push(format!("[{}] ignoring '{topic}' messages", clock()));
            }
            continue;
        }
        let (incoming, total) = match parse_orders(&body) {
            Ok(parsed) => parsed,
            Err(e) => {
                let sample: String = String::from_utf8_lossy(&body).chars().take(300).collect();
                feed.push(format!("[{}] could not read a market message ({e}): {sample}", clock()));
                continue;
            }
        };
        if incoming.is_empty() {
            feed.push(format!("[{}] {total} orders received, none of them refining resources", clock()));
            continue;
        }
        let mut market = shared.lock().unwrap();
        let lines = market.ingest(incoming, now());
        feed.push(format!("[{}] {total} orders received", clock()));
        for line in lines {
            feed.push(format!("  {line}"));
        }
        market.save();
        write_live(&market);
    }
}

/// The refining item behind a numeric item id of the game.
fn item_for_id(id: i64) -> Option<PriceKey> {
    let id = u32::try_from(id).ok()?;
    let index = ITEM_IDS.binary_search_by_key(&id, |(number, _)| *number).ok()?;
    parse_item_id(ITEM_IDS[index].1, 0)
}

/// Reads a price history message. Returns None for items that are not refining resources.
fn parse_history(body: &[u8]) -> Result<Option<IncomingHistory>, String> {
    let value: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let id = value.get("AlbionId").and_then(Value::as_i64).ok_or("no AlbionId")?;
    let Some(key) = item_for_id(id) else { return Ok(None) };
    if value.get("QualityLevel").and_then(Value::as_u64).unwrap_or(1) > 1 {
        return Ok(None);
    }
    let window = value
        .get("Timescale")
        .and_then(Value::as_u64)
        .and_then(|scale| u8::try_from(scale).ok())
        .and_then(Window::from_scale)
        .ok_or("unknown Timescale")?;
    let location = match value.get("LocationId") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err("no LocationId".into()),
    };
    let entries = value.get("MarketHistories").and_then(Value::as_array).ok_or("no MarketHistories list")?;

    let points = entries
        .iter()
        .filter_map(|e| {
            Some(HistoryPoint {
                amount: e.get("ItemAmount")?.as_i64()?,
                // Like the order prices, the silver amounts are multiplied by 10,000.
                silver: e.get("SilverAmount")?.as_f64()? / 10_000.0,
                timestamp: e.get("Timestamp").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect();
    Ok(Some(IncomingHistory { city: city_name(&location), key, window, points }))
}

/// Returns the refining-related orders and the total number of orders in the message.
fn parse_orders(body: &[u8]) -> Result<(Vec<Incoming>, usize), String> {
    let value: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let orders = value.get("Orders").and_then(Value::as_array).ok_or("no Orders list")?;

    let mut incoming = Vec::new();
    for o in orders {
        let Some(item) = o.get("ItemTypeId").and_then(Value::as_str) else { continue };
        let enchant = o.get("EnchantmentLevel").and_then(Value::as_u64).unwrap_or(0).min(255) as u8;
        let Some(key) = parse_item_id(item, enchant) else { continue };
        let location = match o.get("LocationId") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => continue,
        };
        // The game reports prices multiplied by 10,000.
        let Some(price) = o.get("UnitPriceSilver").and_then(Value::as_f64) else { continue };
        let auction = o.get("AuctionType").and_then(Value::as_str).unwrap_or("offer");
        incoming.push(Incoming {
            id: o.get("Id").and_then(Value::as_i64).unwrap_or(0),
            key,
            city: city_name(&location),
            request: auction.to_lowercase().contains("request"),
            price: price / 10_000.0,
            amount: o.get("Amount").and_then(Value::as_i64).unwrap_or(1),
            expires: o.get("Expires").and_then(Value::as_str).unwrap_or("").to_string(),
        });
    }
    Ok((incoming, orders.len()))
}

/// Rewrites the file you can keep open to watch the market change.
pub fn write_live(market: &Market) {
    let app = App::load();
    let ranking = app
        .ranking_text(market, 25, "perfocus", None, false)
        .unwrap_or_else(|e| e);
    let history = if market.histories.is_empty() {
        String::new()
    } else {
        format!("\n=== Price history seen so far ===\n{}", market.history_table(now()))
    };
    let text = format!(
        "Albion market, live view - updated {}\n\
         Buying raw resources: {} | selling refined: {} ({}, {})\n\n\
         === Most profitable products ===\n{}\n\
         === Market prices seen so far ===\n{}{}",
        clock(),
        app.buy_city,
        app.sell_city,
        if app.sell_to_buy_orders { "to the highest buy order" } else { "at the lowest sell order" },
        app.sell_average.map_or("current orders".to_string(), |w| format!("{} average price", w.label())),
        ranking,
        market.table(now()),
        history,
    );
    let _ = fs::write(LIVE_FILE, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_of(name: &str) -> i64 {
        ITEM_IDS.iter().find(|(_, n)| *n == name).unwrap().0 as i64
    }

    #[test]
    fn reads_a_price_history_message() {
        let id = id_of("T4_METALBAR_LEVEL1@1");
        let body = format!(
            r#"{{"AlbionId": {id}, "LocationId": "3008", "QualityLevel": 1, "Timescale": 2,
                "MarketHistories": [
                  {{"ItemAmount": 10, "SilverAmount": 12000000, "Timestamp": 639000000000000000}},
                  {{"ItemAmount": 30, "SilverAmount": 42000000, "Timestamp": 639000000000000001}}]}}"#
        );
        let history = parse_history(body.as_bytes()).unwrap().unwrap();
        assert_eq!(history.city, "Martlock");
        assert_eq!(history.key, PriceKey::refined(crate::calc::Resource::Metal, 4, 1));
        assert_eq!(history.window, Window::Month);
        assert_eq!(history.points.len(), 2);
        // 10 items for 1,200 silver and 30 for 4,200 (prices come multiplied by 10,000).
        assert_eq!(history.points[0].silver, 1200.0);
        assert_eq!(history.points[1].amount, 30);
    }

    #[test]
    fn ignores_other_items_and_rejects_broken_messages() {
        assert!(parse_history(br#"{"AlbionId": 1, "LocationId": "3008", "Timescale": 0, "MarketHistories": []}"#)
            .unwrap()
            .is_none());
        let id = id_of("T5_ORE");
        let bad_scale = format!(r#"{{"AlbionId": {id}, "LocationId": "3008", "Timescale": 9, "MarketHistories": []}}"#);
        assert!(parse_history(bad_scale.as_bytes()).is_err());
        assert!(parse_history(b"not json").is_err());
    }

    #[test]
    fn the_item_ids_are_sorted_and_cover_the_refining_items() {
        assert!(ITEM_IDS.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(ITEM_IDS.iter().all(|(_, name)| parse_item_id(name, 0).is_some()), "every name must be understood");
        assert_eq!(item_for_id(id_of("T4_ORE")), Some(PriceKey::raw(crate::calc::Resource::Metal, 4, 0)));
        assert_eq!(item_for_id(-5), None);
    }

    #[test]
    fn feed_returns_only_new_lines() {
        let feed = Feed::default();
        for i in 0..3 {
            feed.push(format!("line {i}"));
        }
        let (recent, seen) = feed.recent(2);
        assert_eq!(recent, ["line 1", "line 2"]);
        assert_eq!(seen, 3);
        assert!(feed.since(seen).0.is_empty());
        feed.push("line 3".into());
        assert_eq!(feed.since(seen).0, ["line 3"]);
    }

    #[test]
    fn feed_drops_the_oldest_lines() {
        let feed = Feed::default();
        for i in 0..FEED_CAPACITY + 10 {
            feed.push(i.to_string());
        }
        let (lines, total) = feed.since(0);
        assert_eq!(lines.len(), FEED_CAPACITY);
        assert_eq!(lines[0], "10");
        assert_eq!(total as usize, FEED_CAPACITY + 10);
    }
}
