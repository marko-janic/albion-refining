use crate::market::{city_name, parse_item_id, Incoming, Market};
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
    for mut request in server.incoming_requests() {
        let topic = request.url().trim_start_matches('/').to_string();
        let mut body = Vec::new();
        let _ = request.as_reader().read_to_end(&mut body);
        let _ = request.respond(tiny_http::Response::empty(200));

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
    let text = format!(
        "Albion market, live view - updated {}\n\
         Buying raw resources: {} | selling refined: {} ({})\n\n\
         === Most profitable products ===\n{}\n\
         === Market prices seen so far ===\n{}",
        clock(),
        app.buy_city,
        app.sell_city,
        if app.sell_to_buy_orders { "to the highest buy order" } else { "at the lowest sell order" },
        ranking,
        market.table(now()),
    );
    let _ = fs::write(LIVE_FILE, text);
}

#[cfg(test)]
mod tests {
    use super::*;

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
