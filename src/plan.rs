//! Splits a focus budget over several recipes.
//!
//! Every recipe earns less per craft the more of it is made: the cheapest orders of the
//! ingredients are used up first and, when selling to buy orders, the best buy orders as well.
//! The profit of a recipe is therefore a concave function of the focus put into it, and for
//! concave profits the best split is found by always giving the next small amount of focus to
//! the recipe that earns the most extra profit per focus point right now. The recipes that share an
//! order book (for example the same lower tier bar) use it up together, because the plan
//! remembers what has been bought and sold so far.
//!
//! All recipes compete for every step, but at most `max_recipes` of them can be started: the
//! plan picks them as it goes, by the extra profit per focus they offer when they are started.

use crate::calc::{
    craft_fee, focus_cost, focus_return_rate, net_units, PriceKey, Quote, Quotes, Recipe, Settings, Shortage,
};
use std::collections::HashMap;

/// The budget is handed out in steps of this fraction.
const STEPS: f64 = 400.0;

pub struct Row<'a> {
    pub recipe: &'a Recipe,
    pub crafts: u64,
    pub focus: f64,
    /// Cost of the ingredients that were bought for this recipe.
    pub ingredients: f64,
    /// Revenue of its products after market tax.
    pub revenue: f64,
    /// Station fees of all its crafts.
    pub fees: f64,
}

impl Row<'_> {
    pub fn profit(&self) -> f64 {
        self.revenue - self.ingredients - self.fees
    }
}

/// An ingredient to buy for the whole plan.
pub struct Purchase {
    pub key: PriceKey,
    pub units: u64,
    pub quote: Quote,
    /// Names of the recipes of the plan that use it.
    pub used_by: Vec<String>,
}

/// A product to sell for the whole plan.
pub struct Sale {
    pub key: PriceKey,
    pub units: u64,
    pub quote: Quote,
}

/// Why part of the budget was not spent.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Unused {
    /// Focus that is left.
    pub focus: f64,
    /// More crafts of the recipes in the plan would lose silver.
    pub unprofitable: bool,
    /// The market has no more ingredients or buyers for more crafts of the recipes in the plan.
    pub market_too_small: bool,
    /// Another recipe would still earn something, but the plan may not use more recipes.
    pub recipe_limit: bool,
}

pub struct Plan<'a> {
    pub rows: Vec<Row<'a>>,
    pub purchases: Vec<Purchase>,
    pub sales: Vec<Sale>,
    pub focus_used: f64,
    pub ingredient_cost: f64,
    pub revenue: f64,
    pub fees: f64,
    pub unused: Unused,
}

impl Plan<'_> {
    pub fn profit(&self) -> f64 {
        self.revenue - self.ingredient_cost - self.fees
    }
}

struct Candidate<'a> {
    recipe: &'a Recipe,
    /// Focus of one craft with the mastery levels.
    focus: f64,
    row: Row<'a>,
    /// Items this recipe buys or sells; recipes that share one affect each other's prices.
    keys: Vec<PriceKey>,
    active: bool,
    /// Whether this recipe has ever been profitable to start; used to report the recipe limit.
    was_profitable: bool,
    /// The next step of this recipe and its extra profit per focus, until something it depends on changes.
    next: Option<(Chunk, f64)>,
}

/// What has been bought of an item so far.
#[derive(Default, Clone, Copy)]
struct Bought {
    /// Crafts' worth of the item that is needed, a fraction because of the returns.
    need: f64,
    /// Whole units that have to be bought for that.
    units: u64,
    /// Price of those units.
    cost: f64,
    /// The same price for the fractional need, used to compare recipes (see `State::smooth_cost`).
    smooth: f64,
}

/// What has been sold of an item so far.
#[derive(Default, Clone, Copy)]
struct Sold {
    units: u64,
    revenue: f64,
}

/// The effect of making a few more crafts of one recipe.
struct Chunk {
    crafts: u64,
    bought: Vec<(PriceKey, Bought)>,
    sold: (PriceKey, Sold),
    ingredients: f64,
    revenue: f64,
    fees: f64,
    /// Profit with the ingredients priced for the fractional need, for choosing between recipes.
    decision_profit: f64,
}

struct State<'q> {
    quotes: &'q dyn Quotes,
    settings: &'q Settings,
    return_rate: f64,
    after_tax: f64,
    bought: HashMap<PriceKey, Bought>,
    sold: HashMap<PriceKey, Sold>,
}

fn ingredients(recipe: &Recipe) -> Vec<(PriceKey, u32)> {
    let mut items = vec![(PriceKey::raw(recipe.res, recipe.tier, recipe.raw_enchant), recipe.raw_count)];
    if recipe.prev_count > 0 {
        items.push((PriceKey::refined(recipe.res, recipe.tier - 1, recipe.prev_enchant), recipe.prev_count));
    }
    items
}

fn product(recipe: &Recipe) -> PriceKey {
    PriceKey::refined(recipe.res, recipe.tier, recipe.enchant)
}

impl State<'_> {
    /// The price of a fractional number of units, interpolated between the two whole numbers
    /// around it. The real purchase has to be whole units, which makes the cost of a single
    /// small step jump up and down; comparing recipes on those jumps would be misleading.
    fn smooth_cost(&self, key: &PriceKey, need: f64) -> Result<f64, Shortage> {
        let whole = need.floor();
        let cost_of = |units: f64| -> Result<f64, Shortage> {
            if units < 1.0 {
                Ok(0.0)
            } else {
                Ok(self.quotes.buy(key, units as u64)?.average * units)
            }
        };
        let low = cost_of(whole)?;
        let fraction = need - whole;
        if fraction < 1e-9 {
            return Ok(low);
        }
        Ok(low + fraction * (cost_of(whole + 1.0)? - low))
    }

    /// What `crafts` more crafts of the recipe would cost and earn, on top of what has been
    /// bought and sold already.
    fn chunk(&self, recipe: &Recipe, crafts: u64) -> Result<Chunk, Shortage> {
        let mut bought = Vec::new();
        let mut ingredient_cost = 0.0;
        let mut smooth_delta = 0.0;
        for (key, count) in ingredients(recipe) {
            let before = self.bought.get(&key).copied().unwrap_or_default();
            let need = before.need + net_units(crafts, count, self.return_rate);
            // A tiny tolerance keeps rounding errors from asking for one unit too many.
            let units = ((need - 1e-9).ceil() as u64).max(before.units).max(1);
            let cost = if units > before.units {
                self.quotes.buy(&key, units)?.average * units as f64
            } else {
                before.cost
            };
            let smooth = self.smooth_cost(&key, need)?;
            ingredient_cost += cost - before.cost;
            smooth_delta += smooth - before.smooth;
            bought.push((key, Bought { need, units, cost, smooth }));
        }

        let key = product(recipe);
        let before = self.sold.get(&key).copied().unwrap_or_default();
        let units = before.units + crafts * recipe.amount as u64;
        let revenue = self.quotes.sell(&key, units)?.average * units as f64;
        let revenue_delta = (revenue - before.revenue) * self.after_tax;
        let fees = craft_fee(recipe, self.settings) * crafts as f64;

        Ok(Chunk {
            crafts,
            bought,
            sold: (key, Sold { units, revenue }),
            ingredients: ingredient_cost,
            revenue: revenue_delta,
            fees,
            decision_profit: revenue_delta - smooth_delta - fees,
        })
    }
}

/// A recipe that adds less than this share of the plan's profit is left out of the plan.
const MIN_SHARE: f64 = 0.005;

/// Splits `budget` focus over at most `max_recipes` of the recipes so that the total profit is
/// as high as possible.
pub fn plan<'a>(
    recipes: &[&'a Recipe],
    quotes: &dyn Quotes,
    settings: &Settings,
    budget: f64,
    max_recipes: usize,
) -> Plan<'a> {
    let mut recipes: Vec<&'a Recipe> = recipes.to_vec();
    loop {
        let result = allocate(&recipes, quotes, settings, budget, max_recipes);
        // The ingredients are whole units, so a recipe that only gets a few crafts can end up
        // losing silver once the rounding is counted. Recipes that earn next to nothing are
        // not worth the extra shopping either. Both are left out and the rest is planned again.
        let total = result.profit();
        let dropped: Vec<&Recipe> = result
            .rows
            .iter()
            .filter(|r| r.profit() <= 0.0 || (result.rows.len() > 1 && r.profit() < total * MIN_SHARE))
            .map(|r| r.recipe)
            .collect();
        if dropped.is_empty() {
            return result;
        }
        recipes.retain(|r| !dropped.iter().any(|l| std::ptr::eq(*l, *r)));
    }
}

fn allocate<'a>(
    recipes: &[&'a Recipe],
    quotes: &dyn Quotes,
    settings: &Settings,
    budget: f64,
    max_recipes: usize,
) -> Plan<'a> {
    let mut state = State {
        quotes,
        settings,
        return_rate: focus_return_rate(settings),
        after_tax: 1.0 - settings.market_tax / 100.0,
        bought: HashMap::new(),
        sold: HashMap::new(),
    };
    let mut candidates: Vec<Candidate> = recipes
        .iter()
        .map(|&recipe| {
            let mut keys: Vec<PriceKey> = ingredients(recipe).into_iter().map(|(key, _)| key).collect();
            keys.push(product(recipe));
            Candidate {
                recipe,
                focus: focus_cost(recipe.focus, recipe.tier, &settings.mastery),
                row: Row { recipe, crafts: 0, focus: 0.0, ingredients: 0.0, revenue: 0.0, fees: 0.0 },
                keys,
                active: true,
                was_profitable: false,
                next: None,
            }
        })
        .collect();

    let step = budget / STEPS;
    let mut remaining = budget;
    let mut opened = 0usize;
    let mut unused = Unused::default();

    // Every round hands out one step: the recipe with the best extra profit per focus gets it.
    for _ in 0..100_000 {
        for candidate in candidates.iter_mut().filter(|c| c.active) {
            if candidate.next.as_ref().is_some_and(|(chunk, _)| chunk.crafts as f64 * candidate.focus > remaining) {
                candidate.next = None;
            }
            if candidate.next.is_some() {
                continue;
            }
            let started = candidate.row.crafts > 0;
            let affordable = (remaining / candidate.focus).floor() as u64;
            if affordable == 0 {
                candidate.active = false;
                continue;
            }
            let wanted = ((step / candidate.focus).floor() as u64).clamp(1, affordable);
            let chunk = match state.chunk(candidate.recipe, wanted) {
                Ok(chunk) => Some(chunk),
                // Not enough for a whole step; a single craft may still fit.
                Err(Shortage::NotEnough) if wanted > 1 => state.chunk(candidate.recipe, 1).ok(),
                Err(_) => None,
            };
            let Some(chunk) = chunk else {
                // Whether the market is too small or there is no price at all.
                unused.market_too_small |= started;
                candidate.active = false;
                continue;
            };
            // More crafts only get less profitable, so a recipe that loses silver is done.
            if chunk.decision_profit <= 0.0 {
                unused.unprofitable |= started;
                candidate.active = false;
                continue;
            }
            let per_focus = chunk.decision_profit / (chunk.crafts as f64 * candidate.focus);
            candidate.was_profitable = true;
            candidate.next = Some((chunk, per_focus));
        }

        let mut best: Option<usize> = None;
        for (i, candidate) in candidates.iter().enumerate().filter(|(_, c)| c.active) {
            let Some((_, per_focus)) = &candidate.next else { continue };
            if candidate.row.crafts == 0 && opened >= max_recipes {
                unused.recipe_limit = true;
                continue;
            }
            if best.is_none_or(|b| candidates[b].next.as_ref().is_some_and(|(_, other)| per_focus > other)) {
                best = Some(i);
            }
        }
        let Some(i) = best else { break };

        let (chunk, _) = candidates[i].next.take().expect("the best candidate has a next step");
        let focus = chunk.crafts as f64 * candidates[i].focus;
        if candidates[i].row.crafts == 0 {
            opened += 1;
        }
        let row = &mut candidates[i].row;
        row.crafts += chunk.crafts;
        row.focus += focus;
        row.ingredients += chunk.ingredients;
        row.revenue += chunk.revenue;
        row.fees += chunk.fees;
        remaining -= focus;
        state.bought.extend(chunk.bought);
        state.sold.insert(chunk.sold.0, chunk.sold.1);

        // Recipes that share an item with this one have to work out their next step again.
        let committed = candidates[i].keys.clone();
        for (j, other) in candidates.iter_mut().enumerate() {
            if j != i && other.keys.iter().any(|key| committed.contains(key)) {
                other.next = None;
            }
        }
        // Once the limit is reached no other recipe is started any more.
        if opened >= max_recipes {
            for other in candidates.iter_mut().filter(|c| c.row.crafts == 0 && c.active) {
                unused.recipe_limit |= other.was_profitable;
                other.active = false;
            }
        }
    }

    let cheapest = candidates.iter().map(|c| c.focus).fold(f64::INFINITY, f64::min);
    unused.focus = remaining;
    if remaining < cheapest {
        unused = Unused { focus: remaining, ..Unused::default() };
    }

    let mut purchases: Vec<Purchase> = Vec::new();
    let mut sales: Vec<Sale> = Vec::new();
    for candidate in candidates.iter().filter(|c| c.row.crafts > 0) {
        for (key, _) in ingredients(candidate.recipe) {
            let name = crate::calc::recipe_label(candidate.recipe);
            if let Some(purchase) = purchases.iter_mut().find(|p| p.key == key) {
                purchase.used_by.push(name);
            } else if let Some(bought) = state.bought.get(&key) {
                if let Ok(quote) = quotes.buy(&key, bought.units) {
                    purchases.push(Purchase { key, units: bought.units, quote, used_by: vec![name] });
                }
            }
        }
        let key = product(candidate.recipe);
        if !sales.iter().any(|s| s.key == key) {
            if let Some(sold) = state.sold.get(&key) {
                if let Ok(quote) = quotes.sell(&key, sold.units) {
                    sales.push(Sale { key, units: sold.units, quote });
                }
            }
        }
    }

    let mut rows: Vec<Row> = candidates.into_iter().map(|c| c.row).filter(|row| row.crafts > 0).collect();
    rows.sort_by(|a, b| b.profit().total_cmp(&a.profit()));
    Plan {
        focus_used: rows.iter().map(|r| r.focus).sum(),
        ingredient_cost: state.bought.values().map(|b| b.cost).sum(),
        revenue: state.sold.values().map(|s| s.revenue).sum::<f64>() * state.after_tax,
        fees: rows.iter().map(|r| r.fees).sum(),
        rows,
        purchases,
        sales,
        unused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calc::Resource;

    const ANY: usize = usize::MAX;

    fn recipe(res: Resource, tier: u8, focus: f64, raw_count: u32) -> &'static Recipe {
        Box::leak(Box::new(Recipe {
            res,
            tier,
            enchant: 0,
            amount: 1,
            focus,
            item_value: 16,
            raw_enchant: 0,
            raw_count,
            prev_enchant: 0,
            prev_count: 0,
        }))
    }

    /// An order book that gets more expensive with every unit bought: the average price of `q`
    /// units is `base + slope * q / 2`. Products sell at a flat price.
    struct Curve {
        /// (base price, slope) of the ingredient of each tier.
        ingredient: fn(u8) -> (f64, f64),
        product_price: fn(&PriceKey) -> f64,
        /// Most units that can be bought of one ingredient.
        depth: u64,
    }

    impl Quotes for Curve {
        fn buy(&self, key: &PriceKey, qty: u64) -> Result<Quote, Shortage> {
            if qty > self.depth {
                return Err(Shortage::NotEnough);
            }
            let (base, slope) = (self.ingredient)(key.tier);
            Ok(Quote::flat(base + slope * qty as f64 / 2.0))
        }

        fn sell(&self, key: &PriceKey, _qty: u64) -> Result<Quote, Shortage> {
            Ok(Quote::flat((self.product_price)(key)))
        }
    }

    fn curve(depth: u64) -> Curve {
        Curve {
            ingredient: |tier| if tier == 4 { (100.0, 0.5) } else { (150.0, 0.2) },
            product_price: |key| match (key.tier, key.enchant) {
                (4, 0) => 700.0,
                (4, _) => 1200.0,
                _ => 900.0,
            },
            depth,
        }
    }

    /// The profit of `ca` crafts of `a` and `cb` crafts of `b`, worked out directly.
    fn profit_of(curve: &Curve, settings: &Settings, a: (&Recipe, u64), b: (&Recipe, u64)) -> f64 {
        let rate = focus_return_rate(settings);
        let mut total = 0.0;
        for (recipe, crafts) in [a, b] {
            if crafts == 0 {
                continue;
            }
            let units = (net_units(crafts, recipe.raw_count, rate) - 1e-9).ceil().max(1.0);
            let ingredient = curve.buy(&PriceKey::raw(recipe.res, recipe.tier, 0), units as u64).unwrap();
            let price = curve.sell(&product(recipe), crafts).unwrap().average;
            total += price * crafts as f64 * (1.0 - settings.market_tax / 100.0) - ingredient.average * units;
        }
        total
    }

    #[test]
    fn the_split_is_as_good_as_the_best_possible_split() {
        let (a, b) = (recipe(Resource::Metal, 4, 50.0, 2), recipe(Resource::Plank, 5, 80.0, 3));
        let (curve, settings) = (curve(100_000), Settings::default());
        let budget = 20_000.0;

        let plan = plan(&[a, b], &curve, &settings, budget, ANY);
        assert!(plan.rows.len() == 2, "both recipes should be used");
        assert!(plan.focus_used <= budget);

        // Try every number of crafts of `a`, and spend the rest of the focus on `b`.
        let best = (0..=(budget / 50.0) as u64)
            .map(|ca| {
                let cb = ((budget - ca as f64 * 50.0) / 80.0).floor() as u64;
                profit_of(&curve, &settings, (a, ca), (b, cb))
            })
            .fold(f64::MIN, f64::max);
        assert!(plan.profit() > best * 0.995, "plan {} vs best {}", plan.profit(), best);
        assert!(plan.profit() <= best * 1.0001, "plan {} cannot beat the best {}", plan.profit(), best);
    }

    #[test]
    fn splitting_beats_putting_everything_into_one_recipe() {
        let (a, b) = (recipe(Resource::Metal, 4, 50.0, 2), recipe(Resource::Plank, 5, 80.0, 3));
        let (curve, settings) = (curve(100_000), Settings::default());
        let split = plan(&[a, b], &curve, &settings, 20_000.0, ANY);
        let only_a = plan(&[a], &curve, &settings, 20_000.0, ANY);
        let only_b = plan(&[b], &curve, &settings, 20_000.0, ANY);
        assert!(split.profit() > only_a.profit());
        assert!(split.profit() > only_b.profit());
    }

    #[test]
    fn the_totals_add_up() {
        let (a, b) = (recipe(Resource::Metal, 4, 50.0, 2), recipe(Resource::Plank, 5, 80.0, 3));
        let plan = plan(&[a, b], &curve(100_000), &Settings::default(), 20_000.0, ANY);
        let rows: f64 = plan.rows.iter().map(Row::profit).sum();
        assert!((plan.profit() - rows).abs() < 1e-6 * plan.profit().abs());
        // One purchase and one sale per recipe here, and the units match the crafts made.
        assert_eq!((plan.purchases.len(), plan.sales.len()), (2, 2));
        for row in &plan.rows {
            let sale = plan.sales.iter().find(|s| s.key == product(row.recipe)).unwrap();
            assert_eq!(sale.units, row.crafts);
        }
    }

    #[test]
    fn uses_at_most_the_given_number_of_recipes() {
        let recipes = [
            recipe(Resource::Metal, 4, 50.0, 2),
            recipe(Resource::Plank, 5, 80.0, 3),
            recipe(Resource::Cloth, 5, 70.0, 3),
        ];
        // Each ingredient has only 150 units, so a recipe can take 6'000 to 7'000 focus at most.
        let (curve, settings) = (curve(150), Settings::default());
        let two = plan(&recipes, &curve, &settings, 30_000.0, 2);
        let three = plan(&recipes, &curve, &settings, 30_000.0, 3);
        assert_eq!(two.rows.len(), 2);
        assert_eq!(three.rows.len(), 3);
        // More recipes to choose from cannot be worse, and the limit is reported when it matters.
        assert!(three.profit() > two.profit());
        assert!(two.unused.recipe_limit && two.unused.market_too_small);
        assert!(!three.unused.recipe_limit && three.unused.market_too_small);
        assert!(three.unused.focus > 0.0 && three.unused.focus < two.unused.focus);
    }

    #[test]
    fn recipes_that_share_an_ingredient_use_up_its_order_book_together() {
        // Two products from the same ore. The second one earns more per craft but needs more focus,
        // so it takes over from the first one once the shared ore gets expensive.
        let a = recipe(Resource::Metal, 4, 50.0, 2);
        let b: &'static Recipe = Box::leak(Box::new(Recipe { enchant: 1, focus: 100.0, ..copy_of(a) }));
        let plan = plan(&[a, b], &curve(100_000), &Settings::default(), 20_000.0, ANY);
        assert_eq!(plan.rows.len(), 2, "both recipes are used");
        assert_eq!(plan.purchases.len(), 1, "one shared ingredient list");
        assert_eq!(plan.purchases[0].used_by.len(), 2);
        // What the plan paid is exactly the price of that many units from the one order book.
        let units = plan.purchases[0].units as f64;
        let expected = (100.0 + 0.5 * units / 2.0) * units;
        assert!((plan.ingredient_cost - expected).abs() < 1e-6, "{} vs {}", plan.ingredient_cost, expected);
    }

    fn copy_of(r: &Recipe) -> Recipe {
        Recipe {
            res: r.res,
            tier: r.tier,
            enchant: r.enchant,
            amount: r.amount,
            focus: r.focus,
            item_value: r.item_value,
            raw_enchant: r.raw_enchant,
            raw_count: r.raw_count,
            prev_enchant: r.prev_enchant,
            prev_count: r.prev_count,
        }
    }

    #[test]
    fn stops_when_more_crafts_would_lose_silver() {
        // Ingredients get so expensive that only a few crafts pay off.
        let steep = Curve { ingredient: |_| (100.0, 20.0), product_price: |_| 700.0, depth: 100_000 };
        let a = recipe(Resource::Metal, 4, 50.0, 2);
        let plan = plan(&[a], &steep, &Settings::default(), 20_000.0, ANY);
        assert!(plan.focus_used > 0.0 && plan.focus_used < 20_000.0);
        assert!(plan.unused.unprofitable && !plan.unused.market_too_small);
        assert!(plan.profit() > 0.0);
    }

    #[test]
    fn stops_when_the_market_runs_out() {
        let a = recipe(Resource::Metal, 4, 50.0, 2);
        // At most 100 units can be bought: about 88 crafts (1.13 units each after the returns),
        // far less than the 400 crafts the focus allows.
        let plan = plan(&[a], &curve(100), &Settings::default(), 20_000.0, ANY);
        assert!(plan.unused.market_too_small && !plan.unused.unprofitable);
        assert!(plan.purchases[0].units <= 100);
        assert!((85..=89).contains(&plan.rows[0].crafts), "{} crafts", plan.rows[0].crafts);
    }

    #[test]
    fn a_recipe_that_earns_next_to_nothing_is_left_out() {
        let a = recipe(Resource::Metal, 4, 50.0, 2);
        // 8 ingredients per craft: barely profitable, and it only gets the 30 focus that the
        // first recipe cannot use any more (400 crafts of 50 focus fit into 20'030).
        let b = recipe(Resource::Plank, 5, 30.0, 8);
        let crumbs = plan(&[a, b], &curve(100_000), &Settings::default(), 20_030.0, ANY);
        // Without the clean-up the second recipe would get its crumb of focus.
        let raw = allocate(&[a, b], &curve(100_000), &Settings::default(), 20_030.0, ANY);
        assert_eq!(raw.rows.len(), 2);
        assert_eq!(crumbs.rows.len(), 1, "{:?}", crumbs.rows.iter().map(|r| r.crafts).collect::<Vec<_>>());
        assert!(std::ptr::eq(crumbs.rows[0].recipe, a));
        // A single recipe is kept however little it earns.
        let alone = plan(&[b], &curve(100_000), &Settings::default(), 20_030.0, ANY);
        assert_eq!(alone.rows.len(), 1);
    }

    #[test]
    fn a_recipe_that_only_loses_silver_after_rounding_is_dropped() {
        // One craft needs 1.13 units, so 2 whole units at 450 each are bought: 900 for 654.5 of revenue.
        let flat = Curve { ingredient: |_| (450.0, 0.0), product_price: |_| 700.0, depth: 100_000 };
        let a = recipe(Resource::Metal, 4, 50.0, 2);
        let small = plan(&[a], &flat, &Settings::default(), 50.0, ANY);
        assert!(small.rows.is_empty());
        assert_eq!(small.focus_used, 0.0);
        // With enough focus the same recipe is fine: the units are used up completely.
        let large = plan(&[a], &flat, &Settings::default(), 5_000.0, ANY);
        assert_eq!(large.rows.len(), 1);
        assert!(large.profit() > 0.0);
    }

    #[test]
    fn spends_the_whole_budget_when_it_pays() {
        let a = recipe(Resource::Metal, 4, 50.0, 2);
        let plan = plan(&[a], &curve(100_000), &Settings::default(), 1_000.0, ANY);
        assert_eq!(plan.unused.focus, 0.0);
        assert!(!plan.unused.unprofitable && !plan.unused.market_too_small && !plan.unused.recipe_limit);
        assert_eq!(plan.rows[0].crafts, 20);
        assert_eq!(plan.focus_used, 1_000.0);
    }
}
