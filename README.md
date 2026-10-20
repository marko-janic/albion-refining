# Albion refining profit calculator

Type in your prices and refining specialization, then list the most profitable refining recipes.

```
cargo run --release            # interactive prompt, with the market listener running in the background
cargo run --release -- top 10  # run a single command (no listener)
```

## Live market data

The interactive prompt starts a listener on `http://localhost:8080` (`ingest_port` in `config.json`).
Start the Albion Data client with `albiondata-client.exe -i http://localhost:8080` and browse the market in
the game. Every page you open updates the prices. In the prompt:

- `live` prints the incoming market data as it arrives (press Enter to go back to the prompt).
- `purge <city>` deletes everything captured for one city after you type `yes` to confirm. `purge` on its own
  lists the cities that have data. Use the prompt of the running program for this: a second copy started with
  `cargo run -- purge ...` can't change the data of a copy that is running, which would write it back.
- `top 15` ranks the recipes with the latest prices.
- `market_live.txt` is rewritten after every update; keep it open in an editor to watch it.

Captured orders are stored in `market_data.json`.

Quick start:

```
> prices metal               # step through prices (Enter keeps the current value)
> top 15                     # best products by profit per focus point (the default)
> top 15 focus               # best by total profit with focus per craft
> top 15 nofocus             # best by profit per craft without focus
```

All settings live in `config.json`, created next to the program on the first run. It is read again
before every command and every market update, so edits apply right away. Anything you leave out
gets the default shown here:

```json
{
  "mastery": { "T4": 60, "T5": 55, "T6": 40, "T7": 20, "T8": 0 },
  "refining_city": "royal",
  "daily_bonus": 0,
  "usage_fee": 300,
  "buy_cities": [],
  "sell_cities": [],
  "sell_mode": "offer",
  "sell_average": "off",
  "focus_volume": 2000,
  "ingest_port": 8080
}
```

- `mastery`: refining specialization level (0-100) per tier.
- `refining_city`: `"royal"` (production bonus 18) or `"bonus"` (58, the city with the bonus for the resource).
- `daily_bonus`: extra production bonus points from the daily bonus.
- `usage_fee`: the station fee in silver per 100 nutrition, as shown at the refining station.
- Sales tax (market tax): not a setting. It follows `sell_mode`, assuming premium: 6.5% for `"offer"`
  (4% sales tax plus a 2.5% listing fee) and 4% for `"request"` (no listing fee when selling to a buy order).
- `buy_cities` / `sell_cities`: white lists of cities, e.g. `["Lymhurst", "Thetford"]`. Ingredients are only
  bought in the cities of `buy_cities` and products only sold in the cities of `sell_cities`, in the ranking
  and in the plan alike. An empty list `[]` (or `["any"]`) allows every city you have data for. Names are
  not case sensitive and are checked: Thetford, Lymhurst, Bridgewatch, Martlock, Fort Sterling, Caerleon,
  Brecilien, Black Market. The old single `"buy_city": "Martlock"` / `"sell_city"` settings still work.
- `sell_mode`: `"offer"` prices products at the lowest sell order, `"request"` at the highest buy order.
- `sell_average`: `"off"`, `"24h"`, `"1w"` or `"4w"`. Takes the selling price of the products from the average of the
  price history of that time window instead of the current orders. It never affects the ingredients you buy.
  See "Selling at the history average" below.
- `focus_volume`: how much focus worth of refining you plan to do per product; `0` turns it off. See below.
- `ingest_port`: port of the local listener, read at startup. Use the same port in the client's `-i` option.

A typo in a field name or an invalid value prints an error, and the defaults are used until it is fixed.

### Focus volume

With `focus_volume` set (for example 2000), the prices are not just the single best order. For every
recipe the program works out how many crafts that much focus pays for (`focus_volume / focus per craft`).
These are all the crafts you make, including the ones made from resources that come back. With focus every
craft returns part of its ingredients (the return rate `r`, for example 43.5% in a royal city), and you
craft again with what came back. Buying `x` crafts' worth of ingredients therefore gives
`x + x*r + x*r² + ... = x / (1 - r)` crafts in total, so for a total of `C` crafts only `C * (1 - r)` crafts'
worth has to be bought. The program uses that, rounded up to whole units, as the quantity to walk through
the captured orders with, cheapest first. The price used is the average over those
orders. Recipes whose ingredients the market does not have in that quantity are left out of the ranking
(the ranking says how many). The `Crafts` column shows how many crafts the focus volume pays for.
`Ingredients` is the total cost of all the ingredients for those crafts at the average prices (whole units,
after the return rate with focus) and `Exp. profit` the total profit of all the crafts: revenue after market
tax minus ingredients and station fee. `Profit+foc` and `Profit/foc` are per craft.

- Ingredients (raw resources and the lower tier product) are bought from the sell offers of the `buy_cities`.
  The offers of all of those cities are put together and taken cheapest first, so one ingredient can be bought
  in several cities; the ranking lists them as "buy in <city> up to <price>".
- With `sell_mode: "request"` the product is sold to the highest buy orders of one of the `sell_cities` (the
  one that pays best and has enough buy orders), walking down until all of the products are sold. With
  `"offer"` it is listed at the lowest sell offer of the city where that price is highest.
- Only orders that were loaded in the game count, so browse the market pages of the items first.
  Items with typed-in prices only are treated as available in any quantity.

### Plan: splitting the focus over several recipes

`top` ranks each recipe as if you put all of `focus_volume` into it. `plan [n]` (default 5) instead splits
the focus over up to `n` recipes for the highest total profit, and prints what to buy and where to sell for
the whole plan.

Why a split can pay: the more of a recipe you craft, the more it costs per craft. You use up the cheapest
orders of its ingredients first and the next ones are more expensive (and with `sell_mode: "request"` the
best buy orders are used up as well). So the extra profit of every further craft shrinks. The plan hands out
the focus in small steps (1/400 of the budget) and gives each step to the recipe whose next crafts earn the
most extra profit per focus point at that moment. For profits that shrink like this, that is the best
possible split. The recipes that share an order book (for example the same lower tier bar as ingredient)
use it up together. Which recipes are used is decided on the way: all recipes compete, and the first `n`
that win a step are the ones in the plan.

- A recipe that would earn less than 0.5% of the plan's profit, or that loses silver once the ingredients
  are rounded up to whole units, is left out.
- If focus is left over, the plan says why: more crafts would lose silver, the market has no more
  ingredients or buyers, or the limit of `n` recipes (try a larger `n`).
- It also says how much more the split earns than the best of its recipes alone.
- With `sell_mode: "offer"` or a history average the selling price does not depend on how many you sell, so
  one recipe often takes all the focus. Only the ingredients get more expensive. With `"request"` the buy
  orders you captured limit what you can sell.
- It does not chain crafts: an ingredient is always bought from the market, even if the plan makes that item
  as a product of another recipe.

### Where to sell

Below the ingredients of every product in `top`, a line says where and for how much to sell the products.
With one city in `sell_cities` it is that city; with several (or none) it is the best paying of the allowed
cities among the ones you have data for, marked "(best paying city)".

- `sell_mode: "request"`: `sell 111 units in Martlock (best paying city) to buy orders down to 7700 (average 7776 each)`.
  Sell to the buy orders from the highest one down to the given price. The city needs enough buy orders for
  all the products, otherwise it is skipped.
- `sell_mode: "offer"`: `sell 111 units in Martlock (best paying city): list at 7776 each (lowest sell offer there)`.
  The product is listed at the current lowest sell offer of the city with the highest such price.

Buy orders are only captured when you open the buy orders tab of an item in the game.

### Selling at the history average

With `sell_average` set to `"24h"`, `"1w"` or `"4w"` the selling price of a product is the average price it
was really sold for in that time window, taken from the price history that the game sends when you look at
the price chart of an item in the market (24 hours, 7 days and 4 weeks are the three scales of the chart).
The average is weighted by the number of items sold: total silver of the sales divided by the items sold.

- Only the selling price changes. Ingredients are still bought from the current sell offers.
- The history is captured per item, city and time window, so open the chart of each product in each city
  with the window you chose. The ranking line says `at the 4-week average of 140'000 each (5 sold, captured 2s ago)`,
  so you can see how many items the average is based on and how old it is.
- With several `sell_cities` (or none) the allowed city with the highest average is used. A city where only a few items were sold can
  win this way; check the number of items sold.
- The history is about finished sales, so the market tax still follows `sell_mode`, and the buy order depth
  of `"request"` is not checked.
- A product without a history for the chosen window is priced from the current orders, and its line says so
  (`[no 4-week price history captured, using current orders]`). A price you typed in yourself still wins.
- The game's history messages contain only a numeric item id. The ids of the refining resources come from
  `ao-data/ao-bin-dumps` (`formatted/items.txt`, copied into `src/ids.rs`); a game update that renumbers items
  would need that table to be regenerated.
- `purge <city>` also deletes the price histories of that city.

Raw resources use your **buy** price, refined products your **sell** price (the refined
ingredient of the tier below is also priced at its sell price). Typed-in prices are saved to
`albion_state.txt` in the current directory; it is a plain text file you can edit.

A price you type in for a refined product (`set metal 6.2 refined 99999`) is a custom value and is always
used for selling that product instead of the market, whatever the market data says; the ranking marks it
with "(using custom value)". Typed-in raw prices are only used for items the market data knows nothing about.
Remove one with the `prices` command (enter `-`) or `clear` to remove them all.

## Model

Copied from the community calculators and the game data (`ao-data/ao-bin-dumps`):

- Return rate = `LPB / (100 + LPB)`; LPB is 18 in a royal city, 58 in the resource's bonus city, plus
  daily bonus; focus adds 59.
- Cost per craft = `(raw price * raw count + lower tier refined price * count) * (1 - return rate)`.
- Usage fee = `0.1125 * item value * fee / 100` (not charged for T2).
- Focus cost = `base * 0.5^(efficiency / 10000)`, efficiency = 30 per spec level in every tier 4-8,
  plus 250 per level in the product's own tier.
- Profit = `sell * amount * (1 - market tax) - cost - fee`.
- Includes the stone block recipes that turn enchanted rock into 2/4/8 plain blocks.
