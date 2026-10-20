use crate::calc::{group as grouped, PriceKey, Prices, Quote, Resource, Shortage, Source};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;

pub const MARKET_FILE: &str = "market_data.json";
const PAGE_SIZE: usize = 50;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Order {
    pub id: i64,
    /// Price in silver per item.
    pub price: f64,
    pub amount: i64,
    pub expires: String,
}

/// All orders we currently know of for one item in one city and on one side of the market.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Group {
    pub city: String,
    pub res: String,
    pub tier: u8,
    pub enchant: u8,
    pub raw: bool,
    /// true for buy orders, false for sell orders (offers).
    pub request: bool,
    /// Unix seconds of the last time this group was updated by the game.
    pub updated: u64,
    pub orders: Vec<Order>,
}

impl Group {
    pub fn key(&self) -> Option<PriceKey> {
        Some(PriceKey { res: Resource::parse(&self.res)?, tier: self.tier, enchant: self.enchant, raw: self.raw })
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct Market {
    pub groups: Vec<Group>,
}

pub struct Incoming {
    pub id: i64,
    pub key: PriceKey,
    pub city: String,
    pub request: bool,
    pub price: f64,
    pub amount: i64,
    pub expires: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Offer,
    Request,
}

pub fn city_name(location: &str) -> String {
    let digits: String = location.chars().take_while(|c| c.is_ascii_digit()).collect();
    match digits.parse::<u32>() {
        Ok(7 | 301) => "Thetford".into(),
        Ok(1002 | 1301) => "Lymhurst".into(),
        Ok(2004 | 2301) => "Bridgewatch".into(),
        Ok(3003) => "Black Market".into(),
        Ok(3005 | 3013) => "Caerleon".into(),
        Ok(3008 | 3301) => "Martlock".into(),
        Ok(4002 | 4301) => "Fort Sterling".into(),
        Ok(5003) => "Brecilien".into(),
        _ => location.to_string(),
    }
}

fn normalize(city: &str) -> String {
    city.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase()
}

pub fn city_matches(city: &str, filter: &str) -> bool {
    filter == "any" || normalize(city) == normalize(filter)
}

/// Turns an item id such as `T4_METALBAR_LEVEL1` or `T4_ORE@1` into a refining item.
pub fn parse_item_id(id: &str, enchant_field: u8) -> Option<PriceKey> {
    let (base, at_enchant) = match id.split_once('@') {
        Some((b, e)) => (b, e.parse::<u8>().ok()),
        None => (id, None),
    };
    let (tier, name) = base.strip_prefix('T')?.split_once('_')?;
    let tier: u8 = tier.parse().ok()?;
    let (name, level) = match name.rfind("_LEVEL") {
        Some(i) => (&name[..i], name[i + 6..].parse::<u8>().ok()),
        None => (name, None),
    };
    let (res, raw) = match name {
        "CLOTH" => (Resource::Cloth, false),
        "FIBER" => (Resource::Cloth, true),
        "METALBAR" => (Resource::Metal, false),
        "ORE" => (Resource::Metal, true),
        "LEATHER" => (Resource::Leather, false),
        "HIDE" => (Resource::Leather, true),
        "PLANKS" => (Resource::Plank, false),
        "WOOD" => (Resource::Plank, true),
        "STONEBLOCK" => (Resource::Stone, false),
        "ROCK" => (Resource::Stone, true),
        _ => return None,
    };
    let enchant = at_enchant.or(level).unwrap_or(enchant_field);
    ((2..=8).contains(&tier) && enchant <= 4).then_some(PriceKey { res, tier, enchant, raw })
}

impl Market {
    pub fn load() -> Market {
        fs::read_to_string(MARKET_FILE)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Ok(text) = serde_json::to_string(self) else { return };
        let tmp = format!("{MARKET_FILE}.tmp");
        if fs::write(&tmp, text).is_ok() && fs::rename(&tmp, MARKET_FILE).is_err() {
            // Another program may be reading the file right now; try once more.
            std::thread::sleep(std::time::Duration::from_millis(100));
            let _ = fs::rename(&tmp, MARKET_FILE);
        }
    }

    /// Cities that have data with their number of item lists and orders.
    pub fn cities(&self) -> Vec<(String, usize, usize)> {
        let mut cities: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for g in &self.groups {
            let entry = cities.entry(g.city.clone()).or_default();
            entry.0 += 1;
            entry.1 += g.orders.len();
        }
        cities.into_iter().map(|(city, (groups, orders))| (city, groups, orders)).collect()
    }

    /// Number of item lists and orders stored for a city (matched like the city settings, but
    /// "any" matches nothing here so that it can never wipe everything by accident).
    pub fn city_totals(&self, city: &str) -> (usize, usize) {
        let mut totals = (0, 0);
        for g in self.groups.iter().filter(|g| city != "any" && city_matches(&g.city, city)) {
            totals.0 += 1;
            totals.1 += g.orders.len();
        }
        totals
    }

    /// Deletes everything stored for a city. Returns the number of item lists and orders removed.
    pub fn purge_city(&mut self, city: &str) -> (usize, usize) {
        let totals = self.city_totals(city);
        if totals.0 > 0 {
            self.groups.retain(|g| !city_matches(&g.city, city));
        }
        totals
    }

    /// Merges a batch of orders from the game. Returns one log line per updated group.
    ///
    /// The game sends pages of up to `PAGE_SIZE` orders sorted by price. A page that is not
    /// full replaces everything we knew; for a full page, a stored order that lies inside its
    /// price range but is missing from it has been sold or cancelled.
    pub fn ingest(&mut self, incoming: Vec<Incoming>, now: u64) -> Vec<String> {
        let mut batches: BTreeMap<(String, PriceKey, bool), Vec<Order>> = BTreeMap::new();
        for o in incoming {
            batches
                .entry((o.city, o.key, o.request))
                .or_default()
                .push(Order { id: o.id, price: o.price, amount: o.amount, expires: o.expires });
        }

        let mut log = Vec::new();
        for ((city, key, request), orders) in batches {
            let idx = match self.groups.iter().position(|g| {
                g.city == city && g.key() == Some(key) && g.request == request
            }) {
                Some(i) => i,
                None => {
                    self.groups.push(Group {
                        city: city.clone(),
                        res: key.res.key().to_string(),
                        tier: key.tier,
                        enchant: key.enchant,
                        raw: key.raw,
                        request,
                        updated: now,
                        orders: Vec::new(),
                    });
                    self.groups.len() - 1
                }
            };
            let group = &mut self.groups[idx];
            let lo = orders.iter().map(|o| o.price).fold(f64::INFINITY, f64::min);
            let hi = orders.iter().map(|o| o.price).fold(f64::NEG_INFINITY, f64::max);
            let ids: HashSet<i64> = orders.iter().map(|o| o.id).collect();
            if orders.len() < PAGE_SIZE {
                // A page that is not full is the complete list.
                group.orders.clear();
            }
            group.orders.retain(|o| !ids.contains(&o.id) && !(o.price >= lo && o.price <= hi));
            group.orders.extend(orders);
            group.orders.sort_by(|a, b| a.price.total_cmp(&b.price));
            group.updated = now;

            let best = if request { group.orders.last() } else { group.orders.first() };
            log.push(format!(
                "{city}: {} {} orders, {} {}",
                key.label(),
                if request { "buy" } else { "sell" },
                if request { "highest" } else { "lowest" },
                best.map_or("-".to_string(), |o| grouped(o.price, 0)),
            ));
        }
        log
    }

    /// The price of buying `qty` units from the sell offers of the matching cities. The offers of
    /// all of these cities are put together and taken cheapest first, so the quantity may be bought
    /// in several cities; the quote says how much to buy in each city and up to which price.
    pub fn buy_quote(&self, key: &PriceKey, city: &str, qty: u64) -> Result<Quote, Shortage> {
        let qty = qty.max(1);
        let mut pool: Vec<(&str, &Order)> = Vec::new();
        for g in self.groups.iter().filter(|g| g.key() == Some(*key) && !g.request && city_matches(&g.city, city)) {
            pool.extend(g.orders.iter().filter(|o| o.amount > 0).map(|o| (g.city.as_str(), o)));
        }
        if pool.is_empty() {
            return Err(Shortage::NoPrice);
        }
        pool.sort_by(|a, b| a.1.price.total_cmp(&b.1.price));

        let mut missing = qty as f64;
        let mut cost = 0.0;
        let mut limit = 0.0;
        let mut sources: Vec<Source> = Vec::new();
        for (city, order) in pool {
            let take = missing.min(order.amount as f64);
            cost += take * order.price;
            limit = order.price;
            missing -= take;
            match sources.iter_mut().find(|s| s.city == city) {
                Some(source) => {
                    source.limit = order.price;
                    source.units += take as u64;
                }
                None => sources.push(Source { city: city.to_string(), limit: order.price, units: take as u64 }),
            }
            if missing <= 0.0 {
                break;
            }
        }
        if missing > 0.0 {
            return Err(Shortage::NotEnough);
        }
        Ok(Quote { average: cost / qty as f64, limit, sources })
    }

    /// The average price per unit for `qty` units in the matching cities, walking through the
    /// orders from the best one on: cheapest first for sell offers, highest first for buy orders.
    /// The quote also holds the price of the last order that is needed, i.e. how far down the order
    /// book you have to go. Across cities the lowest (`prefer_high == false`) or highest average is
    /// taken among the cities that have enough orders.
    pub fn quote(&self, key: &PriceKey, side: Side, city: &str, qty: u64, prefer_high: bool) -> Result<Quote, Shortage> {
        let qty = qty.max(1);
        let mut known = false;
        let mut best: Option<Quote> = None;
        for g in self
            .groups
            .iter()
            .filter(|g| g.key() == Some(*key) && g.request == (side == Side::Request) && city_matches(&g.city, city))
        {
            let live: Vec<&Order> = match side {
                Side::Offer => g.orders.iter().filter(|o| o.amount > 0).collect(),
                Side::Request => g.orders.iter().rev().filter(|o| o.amount > 0).collect(),
            };
            if live.is_empty() {
                continue;
            }
            known = true;

            let mut missing = qty as f64;
            let mut cost = 0.0;
            let mut limit = 0.0;
            for o in live {
                let take = missing.min(o.amount as f64);
                cost += take * o.price;
                limit = o.price;
                missing -= take;
                if missing <= 0.0 {
                    break;
                }
            }
            if missing > 0.0 {
                continue;
            }
            let quote = Quote {
                average: cost / qty as f64,
                limit,
                sources: vec![Source { city: g.city.clone(), limit, units: qty }],
            };
            let better = match &best {
                Some(b) if prefer_high => quote.average > b.average,
                Some(b) => quote.average < b.average,
                None => true,
            };
            if better {
                best = Some(quote);
            }
        }
        match best {
            Some(quote) => Ok(quote),
            None if known => Err(Shortage::NotEnough),
            None => Err(Shortage::NoPrice),
        }
    }

    /// The best price over the matching cities: per city the lowest sell offer or the highest
    /// buy order, then the lowest (`prefer_high == false`) or highest of those across cities.
    pub fn price(&self, key: &PriceKey, side: Side, city: &str, prefer_high: bool) -> Option<f64> {
        let per_city = self
            .groups
            .iter()
            .filter(|g| g.key() == Some(*key) && g.request == (side == Side::Request) && city_matches(&g.city, city))
            .filter_map(|g| {
                let prices = g.orders.iter().filter(|o| o.amount > 0).map(|o| o.price);
                match side {
                    Side::Offer => prices.fold(None, |a: Option<f64>, p| Some(a.map_or(p, |a| a.min(p)))),
                    Side::Request => prices.fold(None, |a: Option<f64>, p| Some(a.map_or(p, |a| a.max(p)))),
                }
            });
        if prefer_high {
            per_city.fold(None, |a: Option<f64>, p| Some(a.map_or(p, |a| a.max(p))))
        } else {
            per_city.fold(None, |a: Option<f64>, p| Some(a.map_or(p, |a| a.min(p))))
        }
    }

    /// Prices for the profit calculation: raw resources are bought at the lowest sell offer,
    /// refined products are sold at the lowest sell offer or the highest buy order.
    pub fn live_prices(&self, buy_city: &str, sell_city: &str, sell_to_buy_orders: bool) -> Prices {
        let keys: HashSet<PriceKey> = self.groups.iter().filter_map(|g| g.key()).collect();
        let mut prices = HashMap::new();
        for key in keys {
            let price = if key.raw {
                self.price(&key, Side::Offer, buy_city, false)
            } else if sell_to_buy_orders {
                self.price(&key, Side::Request, sell_city, true)
            } else {
                self.price(&key, Side::Offer, sell_city, true)
            };
            if let Some(p) = price {
                prices.insert(key, p);
            }
        }
        prices
    }

    /// One row per item and city with its lowest sell offer and highest buy order.
    pub fn table(&self, now: u64) -> String {
        struct Row {
            offer: Option<(f64, usize)>,
            request: Option<(f64, usize)>,
            updated: u64,
        }
        let mut rows: BTreeMap<(String, u8, u8, bool, String), Row> = BTreeMap::new();
        for g in &self.groups {
            let live: Vec<&Order> = g.orders.iter().filter(|o| o.amount > 0).collect();
            if live.is_empty() {
                continue;
            }
            let row = rows
                .entry((g.res.clone(), g.tier, g.enchant, !g.raw, g.city.clone()))
                .or_insert(Row { offer: None, request: None, updated: 0 });
            row.updated = row.updated.max(g.updated);
            if g.request {
                let best = live.iter().map(|o| o.price).fold(f64::MIN, f64::max);
                row.request = Some((best, live.len()));
            } else {
                let best = live.iter().map(|o| o.price).fold(f64::MAX, f64::min);
                row.offer = Some((best, live.len()));
            }
        }

        let mut out = format!(
            "{:<20} {:<14} {:>16} {:>16} {:>8}\n",
            "Item", "City", "Lowest sell", "Highest buy", "Age"
        );
        for ((res, tier, enchant, refined, city), row) in rows {
            let name = Resource::parse(&res)
                .map(|r| if refined { r.refined_name() } else { r.raw_name() })
                .unwrap_or("?");
            let cell = |v: Option<(f64, usize)>| {
                v.map_or("-".to_string(), |(p, n)| format!("{} ({}x)", grouped(p, 0), grouped(n as f64, 0)))
            };
            let age = now.saturating_sub(row.updated);
            let age = if age < 120 { format!("{age}s") } else if age < 7200 { format!("{}m", age / 60) } else { format!("{}h", age / 3600) };
            out.push_str(&format!(
                "{:<20} {:<14} {:>16} {:>16} {:>8}\n",
                format!("T{tier}.{enchant} {name}"),
                city,
                cell(row.offer),
                cell(row.request),
                age
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incoming(id: i64, price: f64, request: bool) -> Incoming {
        Incoming {
            id,
            key: PriceKey::raw(Resource::Metal, 4, 1),
            city: "Martlock".into(),
            request,
            price,
            amount: 10,
            expires: String::new(),
        }
    }

    #[test]
    fn parses_item_ids() {
        assert_eq!(parse_item_id("T4_METALBAR_LEVEL1", 0), Some(PriceKey::refined(Resource::Metal, 4, 1)));
        assert_eq!(parse_item_id("T5_ORE@2", 0), Some(PriceKey::raw(Resource::Metal, 5, 2)));
        assert_eq!(parse_item_id("T6_PLANKS", 3), Some(PriceKey::refined(Resource::Plank, 6, 3)));
        assert_eq!(parse_item_id("T4_SWORD", 0), None);
    }

    #[test]
    fn maps_locations() {
        assert_eq!(city_name("3008"), "Martlock");
        assert_eq!(city_name("0007"), "Thetford");
        assert!(city_matches("Fort Sterling", "fortsterling"));
    }

    #[test]
    fn a_partial_page_replaces_everything() {
        let mut m = Market::default();
        m.ingest(vec![incoming(1, 90.0, false), incoming(2, 100.0, false), incoming(3, 120.0, false)], 1);
        m.ingest(vec![incoming(2, 100.0, false), incoming(4, 105.0, false)], 2);
        let key = PriceKey::raw(Resource::Metal, 4, 1);
        assert_eq!(m.price(&key, Side::Offer, "any", false), Some(100.0));
        assert_eq!(m.groups[0].orders.len(), 2);
    }

    #[test]
    fn a_full_page_only_updates_its_price_range() {
        let mut m = Market::default();
        // Known orders: one inside the range of the next page (sold since), one outside it.
        m.ingest(vec![incoming(1000, 120.5, false), incoming(1001, 500.0, false)], 1);
        let page: Vec<Incoming> = (0..50).map(|i| incoming(i, 100.0 + i as f64, false)).collect();
        m.ingest(page, 2);
        let ids: Vec<i64> = m.groups[0].orders.iter().map(|o| o.id).collect();
        assert!(!ids.contains(&1000));
        assert!(ids.contains(&1001));
        assert_eq!(ids.len(), 51);
    }

    #[test]
    fn picks_best_price_across_cities() {
        let mut m = Market::default();
        m.ingest(vec![incoming(1, 90.0, false)], 1);
        let mut other = incoming(2, 70.0, false);
        other.city = "Thetford".into();
        m.ingest(vec![other], 1);
        let key = PriceKey::raw(Resource::Metal, 4, 1);
        assert_eq!(m.price(&key, Side::Offer, "any", false), Some(70.0));
        assert_eq!(m.price(&key, Side::Offer, "any", true), Some(90.0));
        assert_eq!(m.price(&key, Side::Offer, "martlock", false), Some(90.0));
    }

    #[test]
    fn quote_averages_over_the_cheapest_orders() {
        let mut m = Market::default();
        // Three sell offers of 10 items each at 100, 110 and 200.
        m.ingest(vec![incoming(1, 100.0, false), incoming(2, 110.0, false), incoming(3, 200.0, false)], 1);
        let key = PriceKey::raw(Resource::Metal, 4, 1);
        assert_eq!(avg(&m, &key, Side::Offer, "any", 5), Ok(100.0));
        assert_eq!(avg(&m, &key, Side::Offer, "any", 20), Ok(105.0));
        // 25 items: 10 * 100 + 10 * 110 + 5 * 200 = 3100
        assert_eq!(avg(&m, &key, Side::Offer, "any", 25), Ok(124.0));
        assert_eq!(avg(&m, &key, Side::Offer, "any", 31), Err(Shortage::NotEnough));
        assert_eq!(avg(&m, &PriceKey::raw(Resource::Cloth, 4, 0), Side::Offer, "any", 1), Err(Shortage::NoPrice));
    }

    fn avg(m: &Market, key: &PriceKey, side: Side, city: &str, qty: u64) -> Result<f64, Shortage> {
        m.quote(key, side, city, qty, side == Side::Request).map(|q| q.average)
    }

    #[test]
    fn quote_limit_is_the_last_order_that_is_needed() {
        let mut m = Market::default();
        m.ingest(vec![incoming(1, 100.0, false), incoming(2, 110.0, false), incoming(3, 200.0, false)], 1);
        let key = PriceKey::raw(Resource::Metal, 4, 1);
        let limit = |qty| m.quote(&key, Side::Offer, "any", qty, false).unwrap().limit;
        assert_eq!(limit(1), 100.0);
        assert_eq!(limit(10), 100.0);
        assert_eq!(limit(11), 110.0);
        assert_eq!(limit(25), 200.0);
    }

    #[test]
    fn purging_a_city_removes_only_that_city() {
        let mut m = Market::default();
        m.ingest(vec![incoming(1, 100.0, false), incoming(2, 90.0, true)], 1);
        let mut other = incoming(3, 70.0, false);
        other.city = "Fort Sterling".into();
        m.ingest(vec![other], 1);

        assert_eq!(m.cities(), [("Fort Sterling".to_string(), 1, 1), ("Martlock".to_string(), 2, 2)]);
        assert_eq!(m.city_totals("any"), (0, 0));
        assert_eq!(m.purge_city("any"), (0, 0));
        assert_eq!(m.groups.len(), 3);

        assert_eq!(m.purge_city("martlock"), (2, 2));
        assert_eq!(m.cities(), [("Fort Sterling".to_string(), 1, 1)]);
        assert_eq!(m.purge_city("fortsterling"), (1, 1));
        assert!(m.groups.is_empty());
        assert_eq!(m.purge_city("martlock"), (0, 0));
    }

    #[test]
    fn buy_quote_mixes_cities() {
        let mut m = Market::default();
        // Martlock: 10 at 100 and 10 at 130. Thetford: 10 at 110 and 10 at 120.
        m.ingest(vec![incoming(1, 100.0, false), incoming(2, 130.0, false)], 1);
        let mut a = incoming(3, 110.0, false);
        a.city = "Thetford".into();
        let mut b = incoming(4, 120.0, false);
        b.city = "Thetford".into();
        m.ingest(vec![a, b], 1);
        let key = PriceKey::raw(Resource::Metal, 4, 1);

        // 25 items: 10 * 100 + 10 * 110 + 5 * 120 = 2700
        let q = m.buy_quote(&key, "any", 25).unwrap();
        assert_eq!(q.average, 108.0);
        assert_eq!(q.limit, 120.0);
        let sources: Vec<_> = q.sources.iter().map(|s| (s.city.as_str(), s.limit, s.units)).collect();
        assert_eq!(sources, [("Martlock", 100.0, 10), ("Thetford", 120.0, 15)]);

        // One city alone does not have enough, and a missing item is something else.
        assert_eq!(m.buy_quote(&key, "martlock", 25), Err(Shortage::NotEnough));
        assert_eq!(m.buy_quote(&key, "martlock", 15).unwrap().sources.len(), 1);
        assert_eq!(m.buy_quote(&PriceKey::raw(Resource::Cloth, 4, 0), "any", 1), Err(Shortage::NoPrice));
    }

    #[test]
    fn quote_sells_to_the_highest_buy_orders_first() {
        let mut m = Market::default();
        m.ingest(vec![incoming(1, 300.0, true), incoming(2, 280.0, true), incoming(3, 100.0, true)], 1);
        let key = PriceKey::raw(Resource::Metal, 4, 1);
        assert_eq!(avg(&m, &key, Side::Request, "any", 15), Ok((10.0 * 300.0 + 5.0 * 280.0) / 15.0));
        assert_eq!(m.quote(&key, Side::Request, "any", 15, true).unwrap().limit, 280.0);
    }

    #[test]
    fn quote_uses_a_city_that_has_enough_orders() {
        let mut m = Market::default();
        m.ingest(vec![incoming(1, 100.0, false)], 1);
        let mut deep = incoming(2, 150.0, false);
        deep.city = "Thetford".into();
        deep.amount = 100;
        m.ingest(vec![deep], 1);
        let key = PriceKey::raw(Resource::Metal, 4, 1);
        // Martlock is cheaper but has only 10 items.
        assert_eq!(avg(&m, &key, Side::Offer, "any", 50), Ok(150.0));
        assert_eq!(avg(&m, &key, Side::Offer, "martlock", 50), Err(Shortage::NotEnough));
    }
}
