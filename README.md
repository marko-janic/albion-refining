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
  "buy_city": "any",
  "sell_city": "any",
  "sell_mode": "offer",
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
- `buy_city` / `sell_city`: where raw resources are bought and refined products sold; `"any"` picks the
  cheapest (buying) or best paying (selling) city.
- `sell_mode`: `"offer"` prices products at the lowest sell order, `"request"` at the highest buy order.
- `focus_volume`: how much focus worth of refining you plan to do per product; `0` turns it off. See below.
- `ingest_port`: port of the local listener, read at startup. Use the same port in the client's `-i` option.

A typo in a field name or an invalid value prints an error, and the defaults are used until it is fixed.

### Focus volume

With `focus_volume` set (for example 2000), the prices are not just the single best order. For every
recipe the program works out how many crafts that much focus pays for (`focus_volume / focus per craft`),
how many raw resources and lower tier items those crafts use up after the return rate, and walks through
the captured orders to buy exactly that many, cheapest first. The price used is the average over those
orders. Recipes whose ingredients the market does not have in that quantity are left out of the ranking
(the ranking says how many). The `Crafts` column shows how many crafts the focus volume pays for.
`Ingredients` is the total cost of all the ingredients for those crafts at the average prices (whole units,
after the return rate with focus) and `Exp. profit` the total profit of all the crafts: revenue after market
tax minus ingredients and station fee. `Profit+foc` and `Profit/foc` are per craft.

- Ingredients (raw resources and the lower tier product) are bought from the sell offers of `buy_city`.
  With `"any"` the offers of all cities you have data for are put together and taken cheapest first, so one
  ingredient can be bought in several cities; the ranking lists them as "buy in <city> up to <price>".
- With `sell_mode: "request"` the product is sold to the highest buy orders of `sell_city`, walking down
  until all of the products are sold. With `"offer"` it is listed at the lowest sell offer.
- Only orders that were loaded in the game count, so browse the market pages of the items first.
  Items with typed-in prices only are treated as available in any quantity.

### Where to sell

Below the ingredients of every product in `top`, a line says where and for how much to sell the products.
With `sell_city: "any"` it is the best paying city among the ones you have data for, marked "(best paying
city)"; with a city set in the config it is that city.

- `sell_mode: "request"`: `sell 111 units in Martlock (best paying city) to buy orders down to 7700 (average 7776 each)`.
  Sell to the buy orders from the highest one down to the given price. The city needs enough buy orders for
  all the products, otherwise it is skipped.
- `sell_mode: "offer"`: `sell 111 units in Martlock (best paying city): list at 7776 each (lowest sell offer there)`.
  The product is listed at the current lowest sell offer of the city with the highest such price.

Buy orders are only captured when you open the buy orders tab of an item in the game.

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
