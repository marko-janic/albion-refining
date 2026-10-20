use crate::calc::Settings;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;

pub const CONFIG_FILE: &str = "config.json";

const TEMPLATE: &str = r#"{
  "mastery": {
    "T4": 0,
    "T5": 0,
    "T6": 0,
    "T7": 0,
    "T8": 0
  },
  "refining_city": "royal",
  "daily_bonus": 0,
  "usage_fee": 0,
  "buy_city": "any",
  "sell_city": "any",
  "sell_mode": "offer",
  "focus_volume": 0,
  "ingest_port": 8080
}
"#;

/// Everything that is read from `config.json`.
pub struct Config {
    pub settings: Settings,
    pub buy_city: String,
    pub sell_city: String,
    pub sell_to_buy_orders: bool,
    pub ingest_port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            buy_city: "any".into(),
            sell_city: "any".into(),
            sell_to_buy_orders: false,
            ingest_port: DEFAULT_PORT,
        }
    }
}

const DEFAULT_PORT: u16 = 8080;

/// Sales tax with premium, charged on every sale.
const SALES_TAX_PERCENT: f64 = 4.0;
/// Listing fee of a sell order. Selling to an existing buy order does not have it.
const LISTING_FEE_PERCENT: f64 = 2.5;

/// Production bonus of a royal city and of the city that has the refining bonus for a resource.
const ROYAL_CITY_LPB: f64 = 18.0;
const BONUS_CITY_LPB: f64 = 58.0;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    mastery: BTreeMap<String, f64>,
    refining_city: String,
    daily_bonus: f64,
    usage_fee: f64,
    buy_city: String,
    sell_city: String,
    sell_mode: String,
    focus_volume: f64,
    ingest_port: u16,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            mastery: BTreeMap::new(),
            refining_city: "royal".into(),
            daily_bonus: 0.0,
            usage_fee: 0.0,
            buy_city: "any".into(),
            sell_city: "any".into(),
            sell_mode: "offer".into(),
            focus_volume: 0.0,
            ingest_port: DEFAULT_PORT,
        }
    }
}

/// Parses `config.json`. Everything that is left out gets its default value.
pub fn parse(text: &str) -> Result<Config, String> {
    let raw: RawConfig = serde_json::from_str(text).map_err(|e| e.to_string())?;

    let mut mastery = [0.0; 5];
    for (key, level) in &raw.mastery {
        let tier = key
            .trim_start_matches(['t', 'T'])
            .parse::<usize>()
            .ok()
            .filter(|t| (4..=8).contains(t))
            .ok_or_else(|| format!("mastery: '{key}' is not a tier between T4 and T8"))?;
        if !(0.0..=100.0).contains(level) {
            return Err(format!("mastery: level of {key} must be from 0 to 100"));
        }
        mastery[tier - 4] = *level;
    }

    let lpb = match raw.refining_city.to_lowercase().as_str() {
        "royal" => ROYAL_CITY_LPB,
        "bonus" => BONUS_CITY_LPB,
        other => return Err(format!("refining_city: '{other}' must be \"royal\" or \"bonus\"")),
    };
    if raw.daily_bonus < 0.0 {
        return Err("daily_bonus cannot be negative".into());
    }
    if !(0.0..=1000.0).contains(&raw.usage_fee) {
        return Err("usage_fee must be from 0 to 1000 (silver per 100 nutrition)".into());
    }
    let sell_to_buy_orders = match raw.sell_mode.to_lowercase().as_str() {
        "offer" => false,
        "request" => true,
        other => return Err(format!("sell_mode: '{other}' must be \"offer\" or \"request\"")),
    };
    let market_tax = if sell_to_buy_orders { SALES_TAX_PERCENT } else { SALES_TAX_PERCENT + LISTING_FEE_PERCENT };

    if raw.ingest_port == 0 {
        return Err("ingest_port must be between 1 and 65535".into());
    }
    if !(raw.focus_volume >= 0.0 && raw.focus_volume.is_finite()) {
        return Err("focus_volume cannot be negative (use 0 to turn it off)".into());
    }

    Ok(Config {
        settings: Settings {
            lpb,
            daily_bonus: raw.daily_bonus,
            usage_fee: raw.usage_fee,
            market_tax,
            mastery,
            focus_volume: raw.focus_volume,
        },
        buy_city: raw.buy_city.trim().to_string(),
        sell_city: raw.sell_city.trim().to_string(),
        sell_to_buy_orders,
        ingest_port: raw.ingest_port,
    })
}

/// Reads `config.json`, creating it with default values when it does not exist.
pub fn load() -> Config {
    let text = match fs::read_to_string(CONFIG_FILE) {
        Ok(text) => text,
        Err(_) => {
            match fs::write(CONFIG_FILE, TEMPLATE) {
                Ok(()) => println!("Created {CONFIG_FILE}: put your refining levels and fees in there."),
                Err(e) => eprintln!("Could not create {CONFIG_FILE}: {e}"),
            }
            return Config::default();
        }
    };
    parse(&text).unwrap_or_else(|e| {
        eprintln!("{CONFIG_FILE}: {e}. Using the default settings until it is fixed.");
        Config::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_matches_the_defaults() {
        let config = parse(TEMPLATE).unwrap();
        let default = Config::default();
        assert_eq!(config.settings.mastery, default.settings.mastery);
        assert_eq!(config.settings.lpb, default.settings.lpb);
        assert_eq!(config.settings.market_tax, default.settings.market_tax);
        assert_eq!(config.settings.usage_fee, default.settings.usage_fee);
        assert_eq!(config.buy_city, "any");
        assert!(!config.sell_to_buy_orders);
        assert_eq!(config.ingest_port, default.ingest_port);
    }

    #[test]
    fn reads_all_fields() {
        let config = parse(
            r#"{"mastery": {"T4": 60, "t6": 12.5, "8": 100}, "refining_city": "bonus", "daily_bonus": 10,
                "usage_fee": 250, "buy_city": "Martlock", "sell_city": "Fort Sterling",
                "sell_mode": "request", "focus_volume": 2000, "ingest_port": 9000}"#,
        )
        .unwrap();
        assert_eq!(config.settings.focus_volume, 2000.0);
        assert_eq!(config.ingest_port, 9000);
        assert_eq!(config.settings.mastery, [60.0, 0.0, 12.5, 0.0, 100.0]);
        assert_eq!(config.settings.lpb, 58.0);
        assert_eq!(config.settings.daily_bonus, 10.0);
        assert_eq!(config.settings.usage_fee, 250.0);
        assert_eq!(config.settings.market_tax, 4.0);
        assert_eq!(config.buy_city, "Martlock");
        assert_eq!(config.sell_city, "Fort Sterling");
        assert!(config.sell_to_buy_orders);
    }

    #[test]
    fn a_file_with_only_mastery_still_works() {
        let config = parse(r#"{"mastery": {"T5": 40}}"#).unwrap();
        assert_eq!(config.settings.mastery[1], 40.0);
        assert_eq!(config.settings.usage_fee, 0.0);
        assert_eq!(config.settings.market_tax, 6.5);
    }

    #[test]
    fn the_market_tax_follows_the_sell_mode() {
        // Sell order: 4% sales tax + 2.5% listing fee. Buy order: only the 4% sales tax.
        assert_eq!(parse(r#"{"sell_mode": "offer"}"#).unwrap().settings.market_tax, 6.5);
        assert_eq!(parse(r#"{"sell_mode": "request"}"#).unwrap().settings.market_tax, 4.0);
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            "not json",
            r#"{"mastery": {"T3": 5}}"#,
            r#"{"mastery": {"T4": 101}}"#,
            r#"{"mastery": {"T4": "high"}}"#,
            r#"{"refining_city": "mars"}"#,
            r#"{"usage_fee": -1}"#,
            r#"{"market_tax": 6.5}"#,
            r#"{"sell_mode": "auction"}"#,
            r#"{"ingest_port": 0}"#,
            r#"{"focus_volume": -5}"#,
            r#"{"ingest_port": 70000}"#,
            r#"{"usagefee": 5}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad} should be rejected");
        }
    }
}
