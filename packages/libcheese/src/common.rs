use serde::de::{self, Deserializer};
use serde::Deserialize;

pub const CHEESE_MINT: &str = "A3hzGcTxZNSc7744CWB2LR5Tt9VTtEaQYpP6nwripump";
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub const USDC_DECIMALS: u8 = 6;
pub const OSHO_MINT: &str = "27VkFr6b6DHoR6hSYZjUDbwJsV6MPSFqPavXLg8nduHW";
pub const HARA_MINT: &str = "7HW7JWmXKPf5GUgfP1vsXUjPBy7WJtA1YQMLFg62pump";
pub const EMPIRE_MINT: &str = "3G5t554LYng7f4xtKKecHbppvctm8qbkoRiTtpqQEAWy";
pub const BLACKLISTED_TOKENS: &[&str] = &[OSHO_MINT, HARA_MINT, EMPIRE_MINT];

pub fn de_string_to_f64<'de, D>(deserializer: D) -> std::result::Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    s.parse::<f64>().map_err(de::Error::custom)
}

// -----------------------------------
// Helper Functions
// -----------------------------------
pub fn parse_other_token_name(pool_name: &str) -> String {
    let parts: Vec<&str> = pool_name.split('-').collect();
    if parts.len() == 2 {
        let left = parts[0].trim();
        let right = parts[1].trim();
        if left.contains("🧀") || left.to_lowercase().contains("cheese") {
            return right.to_string();
        }
        if right.contains("🧀") || right.to_lowercase().contains("cheese") {
            return left.to_string();
        }
        return right.to_string();
    }
    pool_name.to_string()
}

pub fn get_token_amount_from_ui(ui_amount: f64, decimals: u8) -> u64 {
    if decimals == USDC_DECIMALS {
        (ui_amount * 10f64.powi(USDC_DECIMALS as i32)) as u64
    } else {
        (ui_amount * 10f64.powi(decimals as i32)) as u64
    }
}

pub fn get_usdc_amount_from_ui(ui_amount: f64) -> u64 {
    get_token_amount_from_ui(ui_amount, USDC_DECIMALS)
}

pub fn is_token_blacklisted(mint: &str) -> bool {
    BLACKLISTED_TOKENS.contains(&mint)
}
