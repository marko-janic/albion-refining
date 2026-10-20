mod calc;
mod config;
mod data;
mod ids;
mod ingest;
mod market;

use calc::{evaluate, group, Outcome, PriceKey, Prices, Quote, Quotes, Resource, Settings, Shortage, ALL_RESOURCES};
use market::{age_text, Market, Side, Window};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

const STATE_FILE: &str = "albion_state.txt";

pub struct App {
    settings: Settings,
    prices: Prices,
    /// City to buy raw resources in ("any" = the cheapest city).
    pub buy_city: String,
    /// City to sell refined products in ("any" = the best paying city).
    pub sell_city: String,
    /// Sell to the highest buy order instead of listing at the lowest sell order.
    pub sell_to_buy_orders: bool,
    /// Price history window the selling price is taken from instead of the current orders.
    pub sell_average: Option<Window>,
    ingest_port: u16,
    /// The background market listener, when it is running.
    listener: Option<ingest::Listener>,
}

fn main() {
    let mut app = App::load();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        app.execute(&args.join(" "));
        return;
    }

    println!("Albion refining profit calculator. Type 'help' for commands.");
    app.listener = ingest::start(app.ingest_port);
    loop {
        print!("> ");
        let _ = io::stdout().flush();
        let Some(line) = read_line() else { break };
        if !app.execute(&line) {
            break;
        }
    }
}

/// Lines typed by the user. A single thread owns stdin so that the prompt and the
/// live view can both wait for input without blocking the background listener.
fn input() -> &'static Mutex<Receiver<String>> {
    static INPUT: OnceLock<Mutex<Receiver<String>>> = OnceLock::new();
    INPUT.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let stdin = io::stdin();
            loop {
                let mut line = String::new();
                match stdin.lock().read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(line.trim().to_string()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Mutex::new(rx)
    })
}

fn read_line() -> Option<String> {
    input().lock().unwrap().recv().ok()
}

/// Waits up to `timeout` for the user to press Enter; true when they did or input has closed.
fn enter_pressed(timeout: Duration) -> bool {
    !matches!(input().lock().unwrap().recv_timeout(timeout), Err(RecvTimeoutError::Timeout))
}

fn discard_pending_input() {
    let rx = input().lock().unwrap();
    while !matches!(rx.try_recv(), Err(TryRecvError::Empty | TryRecvError::Disconnected)) {}
}

fn parse_item(s: &str) -> Option<(u8, u8)> {
    let s = s.trim_start_matches(['t', 'T']);
    let (tier, enchant) = match s.split_once('.') {
        Some((t, e)) => (t.parse().ok()?, e.parse().ok()?),
        None => (s.parse().ok()?, 0),
    };
    ((2..=8).contains(&tier) && enchant <= 4).then_some((tier, enchant))
}

/// Every price the recipes can use, ordered by tier and enchantment.
fn needed_keys(res: Resource) -> Vec<PriceKey> {
    let mut keys = BTreeSet::new();
    for r in data::RECIPES.iter().filter(|r| r.res == res) {
        keys.insert((r.tier, r.enchant, false));
        keys.insert((r.tier, r.raw_enchant, true));
    }
    let mut keys: Vec<_> = keys.into_iter().collect();
    keys.sort_by_key(|&(tier, enchant, raw)| (tier, enchant, !raw));
    keys.into_iter().map(|(tier, enchant, raw)| PriceKey { res, tier, enchant, raw }).collect()
}

impl App {
    pub fn load() -> App {
        let mut app = App {
            settings: Settings::default(),
            prices: Prices::new(),
            buy_city: "any".into(),
            sell_city: "any".into(),
            sell_to_buy_orders: false,
            sell_average: None,
            ingest_port: 8080,
            listener: None,
        };
        if let Ok(text) = fs::read_to_string(STATE_FILE) {
            for line in text.lines() {
                app.load_line(line);
            }
        }
        app.apply(config::load());
        app
    }

    fn apply(&mut self, config: config::Config) {
        self.settings = config.settings;
        self.buy_city = config.buy_city;
        self.sell_city = config.sell_city;
        self.sell_to_buy_orders = config.sell_to_buy_orders;
        self.sell_average = config.sell_average;
        self.ingest_port = config.ingest_port;
    }

    /// Only the typed-in prices live in the state file; all settings are in config.json.
    fn load_line(&mut self, line: &str) -> Option<()> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let ["price", res, item, kind, v] = parts.as_slice() {
            let res = Resource::parse(res)?;
            let (tier, enchant) = parse_item(item)?;
            self.prices.insert(PriceKey { res, tier, enchant, raw: *kind == "raw" }, v.parse().ok()?);
        }
        Some(())
    }

    fn save(&self) {
        let mut out = String::new();
        let mut prices: Vec<_> = self.prices.iter().collect();
        prices.sort_by_key(|(k, _)| (k.res.key(), k.tier, k.enchant, k.raw));
        for (k, v) in prices {
            out.push_str(&format!(
                "price {} {}.{} {} {}\n",
                k.res.key(),
                k.tier,
                k.enchant,
                if k.raw { "raw" } else { "refined" },
                v
            ));
        }
        if let Err(e) = fs::write(STATE_FILE, out) {
            eprintln!("Could not save {STATE_FILE}: {e}");
        }
    }

    /// Runs one command; returns false when the program should exit.
    fn execute(&mut self, line: &str) -> bool {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some((&cmd, args)) = parts.split_first() else { return true };
        self.apply(config::load());
        let mut changed = true;
        match cmd.to_lowercase().as_str() {
            "help" | "?" => {
                print_help();
                changed = false;
            }
            "quit" | "exit" | "q" => return false,
            "show" => {
                self.show();
                changed = false;
            }
            "city" | "lpb" | "daily" | "fee" | "tax" | "mastery" | "buyfrom" | "sellin" | "sellmode" => {
                println!("'{cmd}' is now set in {}. Edit the file, then type 'show' to check it.", config::CONFIG_FILE);
                changed = false;
            }
            "prices" => self.guided_prices(args),
            "set" => self.set_price(args),
            "list" => {
                self.list(args);
                changed = false;
            }
            "clear" => self.prices.clear(),
            "ingest" => {
                println!("The market listener starts automatically; type 'live' to watch incoming data.");
                changed = false;
            }
            "purge" => {
                self.purge(args);
                changed = false;
            }
            "live" => {
                self.live();
                changed = false;
            }
            "top" | "run" => {
                self.top(args);
                changed = false;
            }
            _ => {
                println!("Unknown command '{cmd}'. Type 'help'.");
                changed = false;
            }
        }
        if changed {
            self.save();
        }
        true
    }

    /// Runs `f` on the market data: the copy the background listener works with when it is
    /// running, otherwise the one in the data file.
    fn with_market<R>(&self, f: impl FnOnce(&mut Market) -> R) -> R {
        match &self.listener {
            Some(listener) => f(&mut listener.market.lock().unwrap()),
            None => f(&mut Market::load()),
        }
    }

    /// Deletes all captured data of one city after asking for confirmation.
    fn purge(&self, args: &[&str]) {
        let cities = self.with_market(|m| m.cities());
        let city = args.join(" ");
        if city.is_empty() || city.eq_ignore_ascii_case("any") {
            println!("Usage: purge <city>");
            if cities.is_empty() {
                println!("There is no market data yet.");
            }
            for (name, groups, orders) in &cities {
                println!("  {name}: {} orders in {groups} item lists", group(*orders as f64, 0));
            }
            return;
        }

        let (groups, orders) = self.with_market(|m| m.city_totals(&city));
        let histories = self.with_market(|m| m.history_totals(&city));
        if groups == 0 && histories == 0 {
            let names: Vec<&str> = cities.iter().map(|(name, ..)| name.as_str()).collect();
            println!("No market data for '{city}'. Cities with data: {}", if names.is_empty() { "none".into() } else { names.join(", ") });
            return;
        }

        let histories_text = |n: usize| match n {
            0 => String::new(),
            1 => " and 1 price history".to_string(),
            n => format!(" and {n} price histories"),
        };
        print!(
            "Delete all data of {city} ({} orders in {groups} item lists{})? This cannot be undone. Type 'yes' to confirm: ",
            group(orders as f64, 0),
            histories_text(histories)
        );
        let _ = io::stdout().flush();
        if !read_line().is_some_and(|answer| answer.eq_ignore_ascii_case("yes")) {
            println!("Cancelled, nothing was deleted.");
            return;
        }

        let (groups, orders) = self.with_market(|market| {
            let removed = market.purge_city(&city);
            market.save();
            ingest::write_live(market);
            removed
        });
        println!(
            "Deleted {} orders in {groups} item lists{} of {city}.",
            group(orders as f64, 0),
            histories_text(histories)
        );
    }

    /// Prints the market data as the listener receives it until Enter is pressed.
    fn live(&self) {
        let Some(listener) = &self.listener else {
            println!("The market listener is not running (see the message at startup).");
            return;
        };
        let feed = &listener.feed;
        println!("Live market feed - press Enter to go back to the prompt.");
        let (backlog, mut seen) = feed.recent(15);
        if backlog.is_empty() {
            println!("(nothing received yet - open the market in the game with the Albion Data client running)");
        }
        for line in backlog {
            println!("{line}");
        }
        discard_pending_input();
        while !enter_pressed(Duration::from_millis(250)) {
            let (lines, total) = feed.since(seen);
            seen = total;
            for line in lines {
                println!("{line}");
            }
        }
    }

    fn set_price(&mut self, args: &[&str]) {
        let parsed = (|| {
            let res = Resource::parse(args.first()?)?;
            let (tier, enchant) = parse_item(args.get(1)?)?;
            let raw = match *args.get(2)? {
                "raw" | "buy" => true,
                "refined" | "sell" => false,
                _ => return None,
            };
            Some((PriceKey { res, tier, enchant, raw }, args.get(3)?.parse::<f64>().ok()?))
        })();
        match parsed {
            Some((key, price)) => {
                self.prices.insert(key, price);
                println!("{} = {}", key.label(), price);
            }
            None => println!("Usage: set <cloth|metal|leather|plank|stone> <tier.enchant> <raw|refined> <price>"),
        }
    }

    fn guided_prices(&mut self, args: &[&str]) {
        let resources: Vec<Resource> = match args.first() {
            None | Some(&"all") => ALL_RESOURCES.to_vec(),
            Some(r) => match Resource::parse(r) {
                Some(r) => vec![r],
                None => {
                    println!("Usage: prices [cloth|metal|leather|plank|stone|all] [tier]");
                    return;
                }
            },
        };
        let tier_filter = args.get(1).and_then(|t| t.trim_start_matches(['t', 'T']).parse::<u8>().ok());
        println!("Enter prices. Empty = keep current, '-' = remove, 'next' = skip this resource, 'stop' = finish.");
        println!("Raw resources are prices you BUY at, refined items are prices you SELL at.");
        for res in resources {
            println!("--- {} ---", res.refined_name());
            for key in needed_keys(res).into_iter().filter(|k| tier_filter.is_none_or(|t| k.tier == t)) {
                let current = self.prices.get(&key).map(|p| p.to_string()).unwrap_or_default();
                print!("{:<18} [{}] > ", key.label(), current);
                let _ = io::stdout().flush();
                let Some(input) = read_line() else { return };
                match input.as_str() {
                    "" => {}
                    "stop" => return,
                    "next" => break,
                    "-" => {
                        self.prices.remove(&key);
                    }
                    v => match v.replace(',', "").parse::<f64>() {
                        Ok(p) => {
                            self.prices.insert(key, p);
                        }
                        Err(_) => println!("  not a number, keeping current"),
                    },
                }
            }
        }
    }

    fn list(&self, args: &[&str]) {
        let resources: Vec<Resource> = match args.first().and_then(|r| Resource::parse(r)) {
            Some(r) => vec![r],
            None => ALL_RESOURCES.to_vec(),
        };
        let prices = self.prices_with(&Market::load());
        for res in resources {
            let entered: Vec<String> = needed_keys(res)
                .iter()
                .map(|k| format!("{:<16} {}", k.label(), prices.get(k).map_or("-".to_string(), |p| p.to_string())))
                .collect();
            println!("--- {} ---", res.refined_name());
            for line in entered {
                println!("  {line}");
            }
        }
    }

    fn show(&self) {
        let s = &self.settings;
        println!("City production bonus: {} (+{} daily)", s.lpb, s.daily_bonus);
        println!(
            "Return rate: {:.1}% without focus, {:.1}% with focus",
            calc::return_rate(s.lpb + s.daily_bonus) * 100.0,
            calc::return_rate(s.lpb + s.daily_bonus + 59.0) * 100.0
        );
        println!("Usage fee: {} silver / 100 nutrition", s.usage_fee);
        if self.sell_to_buy_orders {
            println!("Market tax: {}% (no listing fee when selling to buy orders)", s.market_tax);
        } else {
            println!("Market tax: {}% (sales tax plus listing fee of a sell order)", s.market_tax);
        }
        if s.focus_volume > 0.0 {
            println!(
                "Focus volume: prices are averaged over the orders needed for {} focus of refining",
                group(s.focus_volume, 0)
            );
        } else {
            println!("Focus volume: off (prices are the best single order)");
        }
        println!(
            "Mastery T4-T8: {}",
            s.mastery.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(" / ")
        );
        println!("Prices typed in: {}, captured from the market: {}", self.prices.len(), Market::load().groups.len());
        println!(
            "Buying raw resources: {}, selling refined products: {} ({})",
            self.buy_city,
            self.sell_city,
            if self.sell_to_buy_orders { "to the highest buy order" } else { "at the lowest sell order" }
        );
        match self.sell_average {
            Some(window) => println!(
                "Selling price: {} average from the price history (current orders where there is none)",
                window.label()
            ),
            None => println!("Selling price: from the current orders"),
        }
    }

    /// Typed-in prices and whatever the Albion Data client has captured. A typed-in price of a
    /// refined product (the one that is sold) wins over the market; typed-in raw prices are only
    /// used for items the market data knows nothing about.
    pub fn prices_with(&self, market: &Market) -> Prices {
        let mut prices = market.live_prices(&self.buy_city, &self.sell_city, self.sell_to_buy_orders);
        for (key, price) in &self.prices {
            if !key.raw || !prices.contains_key(key) {
                prices.insert(*key, *price);
            }
        }
        prices
    }

    /// The ranking table of the most profitable recipes.
    pub fn ranking_text(
        &self,
        market: &Market,
        count: usize,
        sort: &str,
        only: Option<Resource>,
        details: bool,
    ) -> Result<String, String> {
        let quotes = MarketQuotes { app: self, market };
        let mut outcomes: Vec<Outcome> = Vec::new();
        let mut too_shallow: Vec<String> = Vec::new();
        for recipe in data::RECIPES.iter().filter(|r| only.is_none_or(|o| r.res == o)) {
            match evaluate(recipe, &quotes, &self.settings) {
                Ok(outcome) => outcomes.push(outcome),
                Err(Shortage::NotEnough) => too_shallow.push(calc::recipe_label(recipe)),
                Err(Shortage::NoPrice) => {}
            }
        }
        let volume_note = if self.settings.focus_volume > 0.0 {
            format!(
                "Prices are averaged over the orders needed to refine {} focus worth of each product, counting the crafts made \
                 from returned resources; the units to buy are what is left after those returns. \
                 {} recipes were left out because the market does not have that much volume (listed below).\n",
                group(self.settings.focus_volume, 0),
                too_shallow.len()
            )
        } else {
            String::new()
        };
        let mut left_out = String::new();
        if !too_shallow.is_empty() {
            left_out.push_str(&format!(
                "\nLeft out - not enough on market for {} focus:\n",
                group(self.settings.focus_volume, 0)
            ));
            for label in &too_shallow {
                left_out.push_str(&format!("  {label}\n"));
            }
        }
        if outcomes.is_empty() {
            let reason = if too_shallow.is_empty() {
                "No recipe has all of its prices yet (type them in or browse the market with the client running).\n"
            } else {
                "No recipe has enough orders on the market for the focus volume (lower focus_volume in config.json or browse more of the market).\n"
            };
            return Err(format!("{reason}{volume_note}{left_out}"));
        }
        let key = |o: &Outcome| match sort {
            "nofocus" => o.profit,
            "perfocus" => o.profit_per_focus,
            _ => o.profit_focus,
        };
        outcomes.sort_by(|a, b| key(b).total_cmp(&key(a)));

        let mut out = format!(
            "Sorted by {} ({} recipes with complete prices). Profit+foc and Profit/foc are per craft; Ingredients and \
             Exp. profit are totals for all Crafts (with focus, after market tax and station fee).\n{volume_note}",
            match sort {
                "nofocus" => "profit without focus",
                "perfocus" => "profit per focus point",
                _ => "profit with focus",
            },
            outcomes.len()
        );
        if details {
            if self.settings.focus_volume > 0.0 {
                out.push_str(
                    "Indented lines: ingredients for one craft as <amount> x <item> @ average <price per unit>, then where to \
                     buy them (take the sell orders of each city up to the given price) and finally where to sell the products.\n",
                );
            } else {
                out.push_str(
                    "Indented lines: ingredients for one craft, as <amount> x <item> @ <price per unit> (city), \
                     then where to sell the product.\n",
                );
            }
        }
        out.push_str(&format!(
            "{:<34} {:>10} {:>11} {:>7} {:>7} {:>10} {:>15} {:>15}\n",
            "Product", "Sell", "Profit+foc", "Focus", "Crafts", "Profit/foc", "Ingredients", "Exp. profit"
        ));
        for o in outcomes.iter().take(count) {
            out.push_str(&format!(
                "{:<34} {:>10} {:>11} {:>7} {:>7} {:>10} {:>15} {:>15}\n",
                o.label,
                group(o.sell_price, 0),
                group(o.profit_focus, 0),
                group(o.focus, 0),
                group(o.crafts as f64, 0),
                group(o.profit_per_focus, 1),
                group(o.ingredient_cost, 0),
                group(o.expected_profit, 0)
            ));
            if details {
                for c in &o.components {
                    out.push_str(&component_text(c, self.settings.focus_volume > 0.0));
                }
                out.push_str(&sell_text(
                    o,
                    &self.sell_city,
                    self.sell_to_buy_orders,
                    self.sell_average,
                    ingest::now(),
                ));
            }
        }
        out.push_str(&left_out);
        Ok(out)
    }

    fn top(&self, args: &[&str]) {
        let mut count = 15usize;
        let mut sort = "perfocus";
        let mut only: Option<Resource> = None;
        for a in args {
            if let Ok(n) = a.parse::<usize>() {
                count = n;
            } else if let Some(r) = Resource::parse(a) {
                only = Some(r);
            } else if matches!(*a, "focus" | "nofocus" | "perfocus") {
                sort = a;
            } else {
                println!("Usage: top [count] [resource] [perfocus|focus|nofocus]");
                return;
            }
        }
        match self.ranking_text(&Market::load(), count, sort, only, true) {
            Ok(text) | Err(text) => print!("{text}"),
        }
    }
}

/// The lines of one ingredient below a product in the ranking.
fn component_text(c: &calc::Component, volume: bool) -> String {
    let q = &c.quote;
    if !volume {
        let city = q.sources.first().map_or(String::new(), |s| format!(" ({})", s.city));
        return format!("      {} x {} @ {} each{city}\n", c.count, c.label, group(q.average, 0));
    }
    let mut text = format!("      {} x {} @ average {} each\n", c.count, c.label, group(q.average, 0));
    for (i, s) in q.sources.iter().enumerate() {
        let lead = if i == 0 { "buy in" } else { "and in" };
        text.push_str(&format!("          {lead} {} up to {} ({})\n", s.city, group(s.limit, 0), units(s.units)));
    }
    text
}

fn units(n: u64) -> String {
    if n == 1 { "1 unit".to_string() } else { format!("{} units", group(n as f64, 0)) }
}

/// The line below a product that says where to sell it. With `sell_city` set to "any" the
/// quote was made for the best paying city; otherwise it is the city from the config.
fn sell_text(o: &Outcome, sell_city: &str, to_buy_orders: bool, sell_average: Option<Window>, now: u64) -> String {
    let q = &o.sell_quote;
    let any = sell_city.eq_ignore_ascii_case("any");
    let city = match q.sources.first() {
        Some(source) => source.city.clone(),
        None if !any => sell_city.to_string(),
        None => String::new(),
    };
    let place = match (city.is_empty(), any) {
        (true, _) => String::new(),
        (false, true) => format!(" in {city} (best paying city)"),
        (false, false) => format!(" in {city}"),
    };
    let n = units(o.sell_units);
    let (average, limit) = (group(q.average, 0), group(q.limit, 0));
    if q.sources.is_empty() {
        return format!("      sell {n}{place} at {average} each (using custom value)\n");
    }
    if let Some(h) = &q.history {
        return format!(
            "      sell {n}{place} at the {} average of {average} each ({} sold, captured {} ago)\n",
            h.window,
            group(h.traded as f64, 0),
            age_text(now.saturating_sub(h.updated))
        );
    }
    let line = if to_buy_orders {
        format!("      sell {n}{place} to buy orders down to {limit} (average {average} each)")
    } else {
        format!("      sell {n}{place}: list at {average} each (lowest sell offer there)")
    };
    match sell_average {
        Some(window) => format!("{line} [no {} price history captured, using current orders]\n", window.label()),
        None => format!("{line}\n"),
    }
}

/// Prices for the profit calculation, taken from the captured order books. Raw resources and the
/// refined ingredient are bought at the cheapest sell offers. The product is sold at the average
/// price of the price history when `sell_average` is set and the history is known, otherwise to the
/// highest buy orders (`sell_mode: request`) or listed at the lowest sell offer. Typed-in prices
/// are used for items the market data knows nothing about.
struct MarketQuotes<'a> {
    app: &'a App,
    market: &'a Market,
}

impl Quotes for MarketQuotes<'_> {
    fn buy(&self, key: &PriceKey, qty: u64) -> Result<Quote, Shortage> {
        match self.market.buy_quote(key, &self.app.buy_city, qty) {
            Err(Shortage::NoPrice) => self.app.prices.buy(key, qty),
            result => result,
        }
    }

    fn sell(&self, key: &PriceKey, qty: u64) -> Result<Quote, Shortage> {
        // A price typed in with `set` or `prices` always wins over the market for the product.
        if self.app.prices.contains_key(key) {
            return self.app.prices.sell(key, qty);
        }
        if let Some(window) = self.app.sell_average {
            if let Some(quote) = self.market.history_quote(key, window, &self.app.sell_city) {
                return Ok(quote);
            }
        }
        if self.app.sell_to_buy_orders {
            self.market.quote(key, Side::Request, &self.app.sell_city, qty, true)
        } else {
            self.market.quote(key, Side::Offer, &self.app.sell_city, 1, true)
        }
    }
}
fn print_help() {
    println!(
        "Commands:
  top [count] [resource] [perfocus|focus|nofocus]   most profitable products (default: 15, by profit per focus point)
  prices [resource|all] [tier]                      type in prices step by step (raw = buy price, refined = sell price)
  set <resource> <tier.ench> <raw|refined> <price>  set a single price, e.g. 'set metal 4.1 refined 950'
  list [resource]                                   show entered prices
  show                                              current settings (read from config.json)
  live                                              watch market data from the game as it arrives (Enter to leave)
  purge [city]                                      delete all captured data of a city (asks for confirmation); without a city it lists them
  clear                                             delete all typed-in prices
  quit
Settings (refining levels, city, fees, buy/sell cities, focus volume, sell price average) are in config.json and are re-read on every command.
Typed-in prices are saved to {STATE_FILE}; captured market data to market_data.json and the live view to market_live.txt."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use calc::Source;

    fn outcome(sell_quote: Quote, sell_units: u64) -> Outcome {
        Outcome {
            label: String::new(),
            sell_price: sell_quote.average,
            sell_quote,
            sell_units,
            ingredient_cost: 0.0,
            expected_profit: 0.0,
            profit: 0.0,
            profit_focus: 0.0,
            focus: 1.0,
            profit_per_focus: 0.0,
            crafts: 1,
            components: Vec::new(),
        }
    }

    fn quote(city: &str) -> Quote {
        Quote {
            average: 7776.0,
            limit: 7700.0,
            sources: vec![Source { city: city.into(), limit: 7700.0, units: 111 }],
            history: None,
        }
    }

    fn history_quote(city: &str) -> Quote {
        Quote {
            average: 7776.0,
            limit: 7776.0,
            sources: vec![Source { city: city.into(), limit: 7776.0, units: 0 }],
            history: Some(calc::HistoryInfo { window: "4-week", traded: 12345, updated: 1000 }),
        }
    }

    #[test]
    fn says_which_city_pays_best_when_the_sell_city_is_any() {
        let o = outcome(quote("Martlock"), 111);
        assert_eq!(
            sell_text(&o, "any", true, None, 0),
            "      sell 111 units in Martlock (best paying city) to buy orders down to 7'700 (average 7'776 each)\n"
        );
        assert_eq!(
            sell_text(&o, "any", false, None, 0),
            "      sell 111 units in Martlock (best paying city): list at 7'776 each (lowest sell offer there)\n"
        );
    }

    #[test]
    fn sells_at_the_history_average_when_there_is_one() {
        let o = outcome(history_quote("Martlock"), 111);
        // Captured 2 hours before "now".
        assert_eq!(
            sell_text(&o, "any", false, Some(Window::Month), 1000 + 7200),
            "      sell 111 units in Martlock (best paying city) at the 4-week average of 7'776 each \
             (12'345 sold, captured 2h ago)\n"
        );
        assert_eq!(
            sell_text(&o, "Martlock", true, Some(Window::Month), 1000 + 30),
            "      sell 111 units in Martlock at the 4-week average of 7'776 each (12'345 sold, captured 30s ago)\n"
        );
    }

    #[test]
    fn says_when_the_history_is_missing_and_current_orders_are_used() {
        let o = outcome(quote("Martlock"), 1);
        assert_eq!(
            sell_text(&o, "any", false, Some(Window::Day), 0),
            "      sell 1 unit in Martlock (best paying city): list at 7'776 each (lowest sell offer there) \
             [no 24-hour price history captured, using current orders]\n"
        );
    }

    #[test]
    fn names_the_configured_city_otherwise() {
        let o = outcome(quote("Martlock"), 1);
        assert_eq!(
            sell_text(&o, "Martlock", true, None, 0),
            "      sell 1 unit in Martlock to buy orders down to 7'700 (average 7'776 each)\n"
        );
    }

    #[test]
    fn custom_values_have_no_city_unless_one_is_configured() {
        let o = outcome(Quote::flat(500.0), 4);
        assert_eq!(sell_text(&o, "any", false, None, 0), "      sell 4 units at 500 each (using custom value)\n");
        assert_eq!(
            sell_text(&o, "Thetford", false, Some(Window::Week), 0),
            "      sell 4 units in Thetford at 500 each (using custom value)\n"
        );
    }
}