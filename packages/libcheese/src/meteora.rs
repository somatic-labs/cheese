use crate::common::{de_string_to_f64, CHEESE_MINT};
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use solana_sdk::pubkey::Pubkey;

// -----------------------------------
// Networking
// -----------------------------------
pub async fn fetch_meteora_cheese_pools(client: &Client) -> Result<Vec<MeteoraPool>> {
    let base_url = "https://amm-v2.meteora.ag";
    let search_url = format!("{}/pools/search", base_url);

    let mut all_pools = Vec::new();
    let mut page = 0;
    let size = 50;

    loop {
        println!("Requesting page {page} from {search_url}");
        let resp = client
            .get(&search_url)
            .query(&[
                ("page".to_string(), page.to_string()),
                ("size".to_string(), size.to_string()),
                ("include_token_mints".to_string(), CHEESE_MINT.to_string()),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("Meteora request failed: {}", resp.status()));
        }

        let text = resp.text().await?;
        println!("Raw response: {}", text);

        let parsed: PaginatedPoolSearchResponse = serde_json::from_str(&text)?;

        println!(
            "Got {} pools on page {}, total_count={}",
            parsed.data.len(),
            parsed.page,
            parsed.total_count
        );

        all_pools.extend(parsed.data);

        let fetched_so_far = (page + 1) * size;
        if fetched_so_far >= parsed.total_count {
            break;
        }
        page += 1;
    }

    println!(
        "\nFetched a total of {} Cheese pools from Meteora.\n",
        all_pools.len()
    );

    Ok(all_pools)
}

#[derive(Debug, Deserialize)]
pub struct MeteoraPool {
    pub pool_address: String,
    pub pool_name: String,
    pub pool_token_mints: Vec<String>,
    pub pool_token_amounts: Vec<String>,
    pub pool_version: u8,
    pub vaults: Vec<String>,

    #[serde(default)]
    pub pool_token_decimals: Vec<u8>,
    #[serde(default)]
    pub pool_token_prices: Vec<String>,

    #[serde(deserialize_with = "de_string_to_f64")]
    pub pool_tvl: f64,

    #[serde(alias = "trading_volume")]
    pub daily_volume: f64,

    #[serde(default)]
    pub derived: bool,
    #[serde(default)]
    pub unknown: bool,
    #[serde(default)]
    pub permissioned: bool,

    #[serde(default)]
    pub fee_volume: f64,

    #[serde(rename = "fee_pct", default = "default_fee_pct")]
    pub total_fee_pct: String,

    // Add vault-related fields
    pub vault_a: String,
    pub vault_b: String,
    pub token_vault_a: String,
    pub token_vault_b: String,
    pub vault_lp_mint_a: String,
    pub vault_lp_mint_b: String,
    pub vault_lp_token_a: String,
    pub vault_lp_token_b: String,
    pub protocol_fee_token_a: String,
    pub protocol_fee_token_b: String,
}

// Add this function to provide a default fee percentage
fn default_fee_pct() -> String {
    "0.3".to_string()
}

// Add helper methods to MeteoraPool
impl MeteoraPool {
    pub fn get_vault_program(&self) -> Result<Pubkey> {
        // This is Meteora's vault program ID
        Ok("24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHDS2SG3LYwBpyTi".parse()?)
    }

    pub fn get_token_program(&self) -> Pubkey {
        spl_token::ID
    }

    // Add this method to initialize vault fields from the vaults array
    pub fn init_vault_fields(&mut self) -> Result<()> {
        if self.vaults.len() < 2 {
            return Err(anyhow!("Pool must have at least 2 vaults"));
        }

        self.vault_a = self.vaults[0].clone();
        self.vault_b = self.vaults[1].clone();

        // For now, use placeholder values for other vault fields
        self.token_vault_a = "placeholder".to_string();
        self.token_vault_b = "placeholder".to_string();
        self.vault_lp_mint_a = "placeholder".to_string();
        self.vault_lp_mint_b = "placeholder".to_string();
        self.vault_lp_token_a = "placeholder".to_string();
        self.vault_lp_token_b = "placeholder".to_string();
        self.protocol_fee_token_a = "placeholder".to_string();
        self.protocol_fee_token_b = "placeholder".to_string();

        Ok(())
    }

    // Update get_swap_accounts to use new field names
    pub async fn get_swap_accounts(
        &self,
        user_source_token: Pubkey,
        user_dest_token: Pubkey,
    ) -> Result<MeteoraSwapAccounts> {
        Ok(MeteoraSwapAccounts {
            pool: self.pool_address.parse()?,
            user_source_token,
            user_destination_token: user_dest_token,
            a_vault: self.vault_a.parse()?,
            b_vault: self.vault_b.parse()?,
            a_token_vault: self.token_vault_a.parse()?,
            b_token_vault: self.token_vault_b.parse()?,
            a_vault_lp_mint: self.vault_lp_mint_a.parse()?,
            b_vault_lp_mint: self.vault_lp_mint_b.parse()?,
            a_vault_lp: self.vault_lp_token_a.parse()?,
            b_vault_lp: self.vault_lp_token_b.parse()?,
            protocol_token_fee: self.protocol_fee_token_a.parse()?,
        })
    }
}

// -----------------------------------
// Part 1: Data Models
// -----------------------------------
#[derive(Debug)]
pub struct MeteoraSwapAccounts {
    pub pool: Pubkey,
    pub user_source_token: Pubkey,
    pub user_destination_token: Pubkey,
    pub a_vault: Pubkey,
    pub b_vault: Pubkey,
    pub a_token_vault: Pubkey,
    pub b_token_vault: Pubkey,
    pub a_vault_lp_mint: Pubkey,
    pub b_vault_lp_mint: Pubkey,
    pub a_vault_lp: Pubkey,
    pub b_vault_lp: Pubkey,
    pub protocol_token_fee: Pubkey,
}

#[derive(Debug, Deserialize)]
pub struct PaginatedPoolSearchResponse {
    data: Vec<MeteoraPool>,
    page: i32,
    total_count: i32,
}

#[derive(Debug, Deserialize, Clone, Serialize)]
pub struct MeteoraQuoteResponse {
    pub pool_address: String,
    pub input_mint: String,
    pub output_mint: String,
    pub in_amount: String,
    pub out_amount: String,
    pub fee_amount: String,
    pub price_impact: String,
}

#[derive(Debug, Serialize, Clone)]
struct MeteoraSwapRequest {
    user_public_key: String,
    quote_response: MeteoraQuoteResponse,
}

#[derive(Debug, Deserialize)]
struct MeteoraSwapResponse {
    transaction: String,
}

// -----------------------------------
// Trading
// -----------------------------------
pub async fn get_meteora_quote(
    client: &Client,
    pool_address: &str,
    input_mint: &str,
    output_mint: &str,
    amount_in: u64,
) -> Result<MeteoraQuoteResponse> {
    // Get current pool state
    let pool = fetch_pool_state(client, pool_address).await?;

    // Find the indices for input and output tokens
    let (in_idx, out_idx) = if pool.pool_token_mints[0] == input_mint {
        (0, 1)
    } else {
        (1, 0)
    };

    // Parse pool amounts
    let in_amount_pool: f64 = pool.pool_token_amounts[in_idx].parse()?;
    let out_amount_pool: f64 = pool.pool_token_amounts[out_idx].parse()?;

    // Default fee if not specified (0.3%)
    let fee_pct = pool
        .total_fee_pct
        .trim_end_matches('%')
        .parse::<f64>()
        .unwrap_or(0.3)
        / 100.0;

    let amount_in_after_fee = amount_in as f64 * (1.0 - fee_pct);

    // Calculate out amount using constant product formula: (x + Δx)(y - Δy) = xy
    let amount_out =
        (out_amount_pool * amount_in_after_fee) / (in_amount_pool + amount_in_after_fee);
    let fee_amount = (amount_in as f64 * fee_pct) as u64;

    // Calculate price impact
    let price_before = out_amount_pool / in_amount_pool;
    let price_after = (out_amount_pool - amount_out) / (in_amount_pool + amount_in as f64);
    let price_impact = ((price_before - price_after) / price_before * 100.0).to_string();

    Ok(MeteoraQuoteResponse {
        pool_address: pool_address.to_string(),
        input_mint: input_mint.to_string(),
        output_mint: output_mint.to_string(),
        in_amount: amount_in.to_string(),
        out_amount: amount_out.to_string(),
        fee_amount: fee_amount.to_string(),
        price_impact,
    })
}

async fn fetch_pool_state(client: &Client, pool_address: &str) -> Result<MeteoraPool> {
    let base_url = "https://amm-v2.meteora.ag";
    let pools_url = format!("{}/pools", base_url);

    let resp = client
        .get(&pools_url)
        .query(&[("address", &[pool_address.to_string()])])
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(anyhow!("Failed to fetch pool state: {}", resp.status()));
    }

    let pools: Vec<MeteoraPool> = resp.json().await?;
    let mut pool = pools
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("Pool not found: {}", pool_address))?;

    pool.init_vault_fields()?;
    Ok(pool)
}

// Update get_meteora_swap_transaction to use proper accounts
pub async fn get_meteora_swap_transaction(
    client: &Client,
    quote: &MeteoraQuoteResponse,
    user_pubkey: &str,
    swap_accounts: &MeteoraSwapAccounts,
) -> Result<String> {
    let swap_url = "https://amm-v2.meteora.ag/swap";

    let swap_request = json!({
        "user_public_key": user_pubkey,
        "quote_response": quote,
        "accounts": {
            "pool": swap_accounts.pool.to_string(),
            "userSourceToken": swap_accounts.user_source_token.to_string(),
            "userDestinationToken": swap_accounts.user_destination_token.to_string(),
            "aVault": swap_accounts.a_vault.to_string(),
            "bVault": swap_accounts.b_vault.to_string(),
            "aTokenVault": swap_accounts.a_token_vault.to_string(),
            "bTokenVault": swap_accounts.b_token_vault.to_string(),
            "aVaultLpMint": swap_accounts.a_vault_lp_mint.to_string(),
            "bVaultLpMint": swap_accounts.b_vault_lp_mint.to_string(),
            "aVaultLp": swap_accounts.a_vault_lp.to_string(),
            "bVaultLp": swap_accounts.b_vault_lp.to_string(),
            "protocolTokenFee": swap_accounts.protocol_token_fee.to_string(),
            "user": user_pubkey,
            "vaultProgram": "24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHDS2SG3LYwBpyTi",
            "tokenProgram": spl_token::ID.to_string()
        }
    });

    println!(
        "Sending swap request: {}",
        serde_json::to_string_pretty(&swap_request)?
    );

    let resp = client.post(swap_url).json(&swap_request).send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let error_text = resp.text().await?;
        return Err(anyhow!(
            "Meteora swap request failed: {} - {}",
            status,
            error_text
        ));
    }

    let swap: MeteoraSwapResponse = resp.json().await?;
    Ok(swap.transaction)
}
