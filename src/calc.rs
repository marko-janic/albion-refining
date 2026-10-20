use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Resource {
    Cloth,
    Metal,
    Leather,
    Plank,
    Stone,
}

pub const ALL_RESOURCES: [Resource; 5] = [
    Resource::Cloth,
    Resource::Metal,
    Resource::Leather,
    Resource::Plank,
    Resource::Stone,
];

impl Resource {
    pub fn key(self) -> &'static str {
        match self {
            Resource::Cloth => "cloth",
            Resource::Metal => "metal",
            Resource::Leather => "leather",
            Resource::Plank => "plank",
            Resource::Stone => "stone",
        }
    }

    pub fn refined_name(self) -> &'static str {
        match self {
            Resource::Cloth => "Cloth",
            Resource::Metal => "Metal Bar",
            Resource::Leather => "Leather",
            Resource::Plank => "Planks",
            Resource::Stone => "Stone Block",
        }
    }

    pub fn raw_name(self) -> &'static str {
        match self {
            Resource::Cloth => "Fiber",
            Resource::Metal => "Ore",
            Resource::Leather => "Hide",
            Resource::Plank => "Wood",
            Resource::Stone => "Rock",
        }
    }

    pub fn parse(s: &str) -> Option<Resource> {
        match s.to_lowercase().as_str() {
            "cloth" | "fiber" => Some(Resource::Cloth),
            "metal" | "bar" | "ore" | "metalbar" => Some(Resource::Metal),
            "leather" | "hide" => Some(Resource::Leather),
            "plank" | "planks" | "wood" => Some(Resource::Plank),
            "stone" | "rock" | "block" | "stoneblock" => Some(Resource::Stone),
            _ => None,
        }
    }
}

/// One standard refining recipe. `enchant` is the enchantment of the product,
/// `raw_enchant` that of the raw input (they differ for the stone block
/// recipes that turn enchanted rock into several plain blocks).
pub struct Recipe {
    pub res: Resource,
    pub tier: u8,
    pub enchant: u8,
    pub amount: u32,
    pub focus: f64,
    pub item_value: u32,
    pub raw_enchant: u8,
    pub raw_count: u32,
    pub prev_enchant: u8,
    pub prev_count: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct PriceKey {
    pub res: Resource,
    pub tier: u8,
    pub enchant: u8,
    pub raw: bool,
}

impl PriceKey {
    pub fn refined(res: Resource, tier: u8, enchant: u8) -> Self {
        Self { res, tier, enchant, raw: false }
    }

    pub fn raw(res: Resource, tier: u8, enchant: u8) -> Self {
        Self { res, tier, enchant, raw: true }
    }

    pub fn label(&self) -> String {
        let name = if self.raw { self.res.raw_name() } else { self.res.refined_name() };
        format!("T{}.{} {}", self.tier, self.enchant, name)
    }
}

pub struct Settings {
    /// Local production bonus of the city (18 royal city, 58 city with the refining bonus).
    pub lpb: f64,
    /// Extra production bonus points, e.g. from the daily bonus.
    pub daily_bonus: f64,
    /// Station usage fee in silver per 100 nutrition.
    pub usage_fee: f64,
    /// Market tax in percent: 6.5 for a sell order (4 tax + 2.5 listing fee), 4 when selling to a buy order.
    pub market_tax: f64,
    /// Refining specialization level (0-100) for tiers 4..=8.
    pub mastery: [f64; 5],
    /// Focus worth of refining the prices are averaged over (0 = just use the best order).
    pub focus_volume: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { lpb: 18.0, daily_bonus: 0.0, usage_fee: 0.0, market_tax: 6.5, mastery: [0.0; 5], focus_volume: 0.0 }
    }
}

const FOCUS_LPB: f64 = 59.0;

/// A number with single quotes between the thousands, e.g. `28'971'677` or `-1'589.5`.
pub fn group(value: f64, decimals: usize) -> String {
    let text = format!("{:.*}", decimals, value.abs());
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (text.as_str(), None),
    };
    let mut grouped = String::new();
    for (i, digit) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push('\'');
        }
        grouped.push(digit);
    }
    if let Some(fraction) = fraction {
        grouped.push('.');
        grouped.push_str(fraction);
    }
    // A value that rounds to zero is shown without a minus sign.
    let negative = value < 0.0 && text.chars().any(|c| c.is_ascii_digit() && c != '0');
    if negative { format!("-{grouped}") } else { grouped }
}

/// Fraction of ingredients returned for a given production bonus.
pub fn return_rate(lpb: f64) -> f64 {
    let lpb = lpb.max(0.0);
    lpb / (100.0 + lpb)
}

/// Focus cost after mastery (30 points per level in every tier) and
/// specialization (+250 points per level in the item's own tier);
/// every 10,000 points halve the cost.
pub fn focus_cost(base: f64, tier: u8, mastery: &[f64; 5]) -> f64 {
    let mut efficiency = 0.0;
    for (i, level) in mastery.iter().enumerate() {
        efficiency += 30.0 * level;
        if i as u8 + 4 == tier {
            efficiency += 250.0 * level;
        }
    }
    base * 0.5f64.powf(efficiency / 10_000.0)
}

pub struct Outcome {
    pub label: String,
    pub sell_price: f64,
    /// Where and for how much the product is sold.
    pub sell_quote: Quote,
    /// How many products all the crafts make (what the sell quote is for).
    pub sell_units: u64,
    /// Total cost of the ingredients of all crafts at the average prices, with focus.
    pub ingredient_cost: f64,
    /// Total profit of all crafts with focus: revenue after tax minus ingredients and station fee.
    pub expected_profit: f64,
    pub profit: f64,
    pub profit_focus: f64,
    pub focus: f64,
    pub profit_per_focus: f64,
    /// How many crafts the focus volume pays for (1 when no focus volume is set).
    pub crafts: u64,
    /// The ingredients of one craft with the price per unit the calculation used.
    pub components: Vec<Component>,
}

pub struct Component {
    pub label: String,
    pub count: u32,
    pub quote: Quote,
}

pub type Prices = HashMap<PriceKey, f64>;

/// Why a price could not be determined.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shortage {
    /// Nothing is known about the item.
    NoPrice,
    /// The item is on the market but not in the quantity needed.
    NotEnough,
}

/// The price of a quantity of items taken from the order book.
#[derive(Clone, PartialEq, Debug)]
pub struct Quote {
    /// Average price per unit over all the orders that are needed.
    pub average: f64,
    /// Price of the last order that is needed: the worst price you have to accept.
    pub limit: f64,
    /// Where the items are bought, cheapest city first. Empty for typed-in prices.
    pub sources: Vec<Source>,
    /// Set when the price is an average from the price history instead of current orders.
    pub history: Option<HistoryInfo>,
}

/// What a price from the price history is based on.
#[derive(Clone, PartialEq, Debug)]
pub struct HistoryInfo {
    /// Name of the time window, e.g. "4-week".
    pub window: &'static str,
    /// How many items were sold in that time.
    pub traded: u64,
    /// Unix seconds when the history was captured.
    pub updated: u64,
}

/// Part of a quote that comes from one city.
#[derive(Clone, PartialEq, Debug)]
pub struct Source {
    pub city: String,
    /// Price of the last order that is needed in this city.
    pub limit: f64,
    /// How many items to buy in this city.
    pub units: u64,
}

impl Quote {
    pub fn flat(price: f64) -> Self {
        Self { average: price, limit: price, sources: Vec::new(), history: None }
    }
}

/// Prices for a given quantity.
pub trait Quotes {
    /// Price for buying `qty` units, cheapest orders first.
    fn buy(&self, key: &PriceKey, qty: u64) -> Result<Quote, Shortage>;
    /// Price for selling `qty` units of a refined product.
    fn sell(&self, key: &PriceKey, qty: u64) -> Result<Quote, Shortage>;
}

/// Plain prices without any depth: every quantity is available at the same price.
impl Quotes for Prices {
    fn buy(&self, key: &PriceKey, _qty: u64) -> Result<Quote, Shortage> {
        self.get(key).copied().map(Quote::flat).ok_or(Shortage::NoPrice)
    }

    fn sell(&self, key: &PriceKey, _qty: u64) -> Result<Quote, Shortage> {
        self.get(key).copied().map(Quote::flat).ok_or(Shortage::NoPrice)
    }
}

/// Name of a recipe's product, e.g. `T4.0 Stone Block x2 (from .1 Rock)`.
pub fn recipe_label(recipe: &Recipe) -> String {
    let mut label = format!("T{}.{} {}", recipe.tier, recipe.enchant, recipe.res.refined_name());
    if recipe.amount > 1 || recipe.raw_enchant != recipe.enchant {
        label.push_str(&format!(" x{} (from .{} {})", recipe.amount, recipe.raw_enchant, recipe.res.raw_name()));
    }
    label
}

/// Number of units to buy to be able to make `crafts` crafts in total when the resources that
/// come back from every craft are used for further crafts (all with focus).
///
/// Buying `x` crafts' worth gives `x` crafts, which return `x * r` crafts' worth, which give
/// `x * r` more crafts, and so on: `x * (1 + r + r^2 + ...) = x / (1 - r)` crafts in total.
/// Solving that backwards for `crafts` gives `crafts * (1 - r)` crafts' worth to buy.
fn units_needed(crafts: u64, per_craft: u32, return_rate: f64) -> u64 {
    ((crafts as f64 * per_craft as f64 * (1.0 - return_rate)).ceil() as u64).max(1)
}

/// Profit per craft. With a focus volume set, prices are averaged over the orders that
/// are needed to refine that much focus worth of products.
pub fn evaluate(recipe: &Recipe, quotes: &dyn Quotes, s: &Settings) -> Result<Outcome, Shortage> {
    let focus = focus_cost(recipe.focus, recipe.tier, &s.mastery);
    let lpb = s.lpb + s.daily_bonus;
    let rate = return_rate(lpb);
    let rate_focus = return_rate(lpb + FOCUS_LPB);

    let crafts = if s.focus_volume > 0.0 { ((s.focus_volume / focus).floor() as u64).max(1) } else { 1 };
    let (raw_qty, prev_qty, sell_qty) = if s.focus_volume > 0.0 {
        (
            units_needed(crafts, recipe.raw_count, rate_focus),
            units_needed(crafts, recipe.prev_count, rate_focus),
            crafts * recipe.amount as u64,
        )
    } else {
        (1, 1, 1)
    };

    let product = PriceKey::refined(recipe.res, recipe.tier, recipe.enchant);
    let raw_key = PriceKey::raw(recipe.res, recipe.tier, recipe.raw_enchant);
    let prev_key = PriceKey::refined(recipe.res, recipe.tier.saturating_sub(1), recipe.prev_enchant);
    let sell = quotes.sell(&product, sell_qty);
    let raw = quotes.buy(&raw_key, raw_qty);
    let prev = if recipe.prev_count > 0 { quotes.buy(&prev_key, prev_qty) } else { Ok(Quote::flat(0.0)) };

    let results = [sell, raw, prev];
    if results.contains(&Err(Shortage::NoPrice)) {
        return Err(Shortage::NoPrice);
    }
    let [Ok(sell), Ok(raw), Ok(prev)] = results else { return Err(Shortage::NotEnough) };

    let gross_materials = raw.average * recipe.raw_count as f64 + prev.average * recipe.prev_count as f64;
    let material_cost = gross_materials * (1.0 - rate);
    let material_cost_focus = gross_materials * (1.0 - rate_focus);

    let usage_fee = if recipe.tier <= 2 {
        0.0
    } else {
        0.1125 * recipe.item_value as f64 * s.usage_fee / 100.0
    };
    let revenue = sell.average * recipe.amount as f64 * (1.0 - s.market_tax / 100.0);
    let profit = revenue - material_cost - usage_fee;
    let profit_focus = revenue - material_cost_focus - usage_fee;

    // Totals for all the crafts, with focus. The ingredients are the whole units that have to be bought.
    let ingredient_cost = if s.focus_volume > 0.0 {
        raw_qty as f64 * raw.average + prev_qty as f64 * prev.average
    } else {
        material_cost_focus
    };
    let expected_profit = (revenue - usage_fee) * crafts as f64 - ingredient_cost;

    let mut components = vec![Component { label: raw_key.label(), count: recipe.raw_count, quote: raw }];
    if recipe.prev_count > 0 {
        components.push(Component { label: prev_key.label(), count: recipe.prev_count, quote: prev });
    }

    Ok(Outcome {
        label: recipe_label(recipe),
        sell_price: sell.average,
        sell_quote: sell,
        sell_units: crafts * recipe.amount as u64,
        ingredient_cost,
        expected_profit,
        profit,
        profit_focus,
        focus,
        profit_per_focus: profit_focus / focus,
        crafts,
        components,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Crafts made when `units` are bought and everything that comes back is crafted again,
    /// simulated round by round (expected values).
    fn crafts_with_recrafting(units: u64, per_craft: u32, return_rate: f64) -> f64 {
        let mut stock = units as f64;
        let mut crafts = 0.0;
        while stock / per_craft as f64 > 1e-9 {
            let round = stock / per_craft as f64;
            crafts += round;
            stock = round * per_craft as f64 * return_rate;
        }
        crafts
    }

    #[test]
    fn units_needed_is_the_smallest_amount_that_pays_for_all_crafts_with_recrafting() {
        for (per_craft, lpb) in [(2u32, 77.0), (3, 77.0), (4, 117.0), (5, 117.0), (2, 59.0)] {
            let rate = return_rate(lpb);
            for crafts in [1u64, 10, 111, 477, 836] {
                let units = units_needed(crafts, per_craft, rate);
                assert!(
                    crafts_with_recrafting(units, per_craft, rate) >= crafts as f64 - 1e-6,
                    "{units} units are not enough for {crafts} crafts ({per_craft} per craft, lpb {lpb})"
                );
                if units > 1 {
                    assert!(
                        crafts_with_recrafting(units - 1, per_craft, rate) < crafts as f64,
                        "{units} units are more than needed for {crafts} crafts ({per_craft} per craft, lpb {lpb})"
                    );
                }
            }
        }
    }

    #[test]
    fn groups_thousands_with_single_quotes() {
        assert_eq!(group(0.0, 0), "0");
        assert_eq!(group(999.0, 0), "999");
        assert_eq!(group(1000.0, 0), "1'000");
        assert_eq!(group(28971677.4, 0), "28'971'677");
        assert_eq!(group(-1589.5, 0), "-1'590");
        assert_eq!(group(-1589.54, 1), "-1'589.5");
        assert_eq!(group(1234.56, 1), "1'234.6");
        assert_eq!(group(100000.0, 0), "100'000");
        assert_eq!(group(-0.2, 0), "0");
        assert_eq!(group(-0.2, 1), "-0.2");
    }

    #[test]
    fn return_rates_match_game_values() {
        assert!((return_rate(18.0) - 0.1525).abs() < 1e-3);
        assert!((return_rate(18.0 + FOCUS_LPB) - 0.435).abs() < 1e-3);
        assert!((return_rate(58.0) - 0.367).abs() < 1e-3);
        assert!((return_rate(58.0 + FOCUS_LPB) - 0.539).abs() < 1e-3);
    }

    #[test]
    fn focus_halves_every_10000_points() {
        let mut mastery = [0.0; 5];
        assert_eq!(focus_cost(100.0, 6, &mastery), 100.0);
        mastery[2] = 100.0; // T6: 100 * (30 + 250) = 28,000 points
        let expected = 100.0 * 0.5f64.powf(2.8);
        assert!((focus_cost(100.0, 6, &mastery) - expected).abs() < 1e-9);
        // Another tier only gets the 30-per-level part.
        let expected = 100.0 * 0.5f64.powf(0.3);
        assert!((focus_cost(100.0, 5, &mastery) - expected).abs() < 1e-9);
    }

    #[test]
    fn evaluates_a_simple_recipe() {
        let recipe = Recipe {
            res: Resource::Metal,
            tier: 4,
            enchant: 0,
            amount: 1,
            focus: 54.0,
            item_value: 16,
            raw_enchant: 0,
            raw_count: 2,
            prev_enchant: 0,
            prev_count: 1,
        };
        let mut prices = Prices::new();
        prices.insert(PriceKey::refined(Resource::Metal, 4, 0), 1000.0);
        prices.insert(PriceKey::raw(Resource::Metal, 4, 0), 100.0);
        prices.insert(PriceKey::refined(Resource::Metal, 3, 0), 200.0);
        let settings = Settings { market_tax: 0.0, ..Settings::default() };
        let out = evaluate(&recipe, &prices, &settings).unwrap();
        let rrr = return_rate(18.0);
        assert!((out.profit - (1000.0 - 400.0 * (1.0 - rrr))).abs() < 1e-9);
        assert!(out.profit_focus > out.profit);
    }

    /// Only 12 units of anything can be bought; selling is unlimited.
    struct Shallow;

    impl Quotes for Shallow {
        fn buy(&self, _key: &PriceKey, qty: u64) -> Result<Quote, Shortage> {
            if qty <= 12 {
                Ok(Quote { average: 100.0, limit: 100.0 + qty as f64, sources: Vec::new(), history: None })
            } else {
                Err(Shortage::NotEnough)
            }
        }

        fn sell(&self, _key: &PriceKey, _qty: u64) -> Result<Quote, Shortage> {
            Ok(Quote::flat(1000.0))
        }
    }

    #[test]
    fn focus_volume_needs_enough_orders() {
        let recipe = Recipe {
            res: Resource::Metal,
            tier: 4,
            enchant: 0,
            amount: 1,
            focus: 54.0,
            item_value: 16,
            raw_enchant: 0,
            raw_count: 2,
            prev_enchant: 0,
            prev_count: 1,
        };
        // 540 focus = 10 crafts = 20 ore, of which 43.5% come back: 12 have to be bought.
        let settings = Settings { focus_volume: 540.0, ..Settings::default() };
        let outcome = evaluate(&recipe, &Shallow, &settings).unwrap();
        assert_eq!(outcome.crafts, 10);
        assert_eq!(outcome.sell_units, 10);
        // 12 ore and 6 lower tier bars at 100 each; 10 bars sell for 1000 minus 6.5% tax.
        assert_eq!(outcome.ingredient_cost, 1800.0);
        assert!((outcome.expected_profit - (10.0 * 1000.0 * 0.935 - 1800.0)).abs() < 1e-9);
        // The ore is the first component; the limit is the price of the last unit that is bought.
        assert_eq!(outcome.components[0].quote.limit, 100.0 + 12.0);
        // 1080 focus = 20 crafts needs 23 ore.
        let settings = Settings { focus_volume: 1080.0, ..Settings::default() };
        assert_eq!(evaluate(&recipe, &Shallow, &settings).err(), Some(Shortage::NotEnough));
        // Without a focus volume a single order is enough.
        assert_eq!(evaluate(&recipe, &Shallow, &Settings::default()).unwrap().crafts, 1);
    }

    #[test]
    fn missing_prices_are_not_a_volume_problem() {
        let recipe = &crate::data::RECIPES[0];
        let settings = Settings { focus_volume: 1000.0, ..Settings::default() };
        assert_eq!(evaluate(recipe, &Prices::new(), &settings).err(), Some(Shortage::NoPrice));
    }
}
