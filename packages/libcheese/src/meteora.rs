use crate::common::{de_string_to_f64, CHEESE_MINT};
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

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

        let parsed: PaginatedPoolSearchResponse = resp.json().await?;
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
    pub pool_type: String,
    pub total_fee_pct: String,

    // For demonstration, we won't read these
    #[allow(dead_code)]
    unknown: bool,
    #[allow(dead_code)]
    permissioned: bool,

    #[serde(deserialize_with = "de_string_to_f64")]
    pub pool_tvl: f64,

    #[serde(alias = "trading_volume")]
    pub daily_volume: f64,

    pub pool_token_amounts: Vec<String>,

    #[serde(default)]
    pub derived: bool,
}

// -----------------------------------
// Part 1: Data Models
// -----------------------------------
#[derive(Debug, Deserialize)]
pub struct PaginatedPoolSearchResponse {
    data: Vec<MeteoraPool>,
    page: i32,
    total_count: i32,
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
    println!("Fetching pool state for {}", pool_address);
    let pool = fetch_pool_state(client, pool_address).await?;

    // Find the indices for input and output tokens
    let (in_idx, out_idx) = if pool.pool_token_mints[0] == input_mint {
        (0, 1)
    } else {
        (1, 0)
    };

    println!(
        "Pool state: in_idx={}, out_idx={}, amounts={:?}",
        in_idx, out_idx, pool.pool_token_amounts
    );

    // Parse pool amounts
    let in_amount_pool: f64 = pool.pool_token_amounts[in_idx].parse()?;
    let out_amount_pool: f64 = pool.pool_token_amounts[out_idx].parse()?;

    // Calculate fee
    let fee_pct: f64 = pool.total_fee_pct.trim_end_matches('%').parse::<f64>()? / 100.0;
    let amount_in_after_fee = amount_in as f64 * (1.0 - fee_pct);

    // Calculate out amount using constant product formula: (x + Δx)(y - Δy) = xy
    let amount_out =
        (out_amount_pool * amount_in_after_fee) / (in_amount_pool + amount_in_after_fee);
    let fee_amount = (amount_in as f64 * fee_pct) as u64;

    // Calculate price impact
    let price_before = out_amount_pool / in_amount_pool;
    let price_after = (out_amount_pool - amount_out) / (in_amount_pool + amount_in as f64);
    let price_impact = ((price_before - price_after) / price_before * 100.0).to_string();

    let quote = MeteoraQuoteResponse {
        pool_address: pool_address.to_string(),
        input_mint: input_mint.to_string(),
        output_mint: output_mint.to_string(),
        in_amount: amount_in.to_string(),
        out_amount: amount_out.to_string(),
        fee_amount: fee_amount.to_string(),
        price_impact,
    };

    println!("Generated quote: {:?}", quote);
    Ok(quote)
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
    pools
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("Pool not found: {}", pool_address))
}

pub async fn get_meteora_swap_transaction(
    client: &Client,
    quote: &MeteoraQuoteResponse,
    user_pubkey: &str,
) -> Result<String> {
    let base_url = "https://amm-v2.meteora.ag";
    let swap_url = format!("{}/swap", base_url);

    let swap_request = MeteoraSwapRequest {
        user_public_key: user_pubkey.to_string(),
        quote_response: quote.clone(),
    };

    println!("Sending swap request to {}: {:?}", swap_url, swap_request);

    let resp = client.post(&swap_url).json(&swap_request).send().await?;

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
    println!(
        "Received swap transaction (length={})",
        swap.transaction.len()
    );
    Ok(swap.transaction)
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
