use crate::common::{de_string_to_f64, CHEESE_MINT};
use anchor_lang::{prelude::*, AnchorDeserialize};
use anyhow::{anyhow, Result};
use bincode;
use borsh::BorshDeserialize;
use prog_dynamic_amm::state::Pool;
use prog_dynamic_vault::state::Vault;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};
use spl_token;
use std::str::FromStr;

pub const METEORA_PROGRAM_ID: &str = "Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB";
pub const METEORA_DEPOSIT_PROGRAM_ID: &str = "24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHDS2SG3LYwBpyTi";

// Helper function to convert between Pubkey types
fn to_solana_pubkey(pubkey: &Pubkey) -> solana_sdk::pubkey::Pubkey {
    solana_sdk::pubkey::Pubkey::new_from_array(pubkey.to_bytes())
}

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
    pool: &MeteoraPool,
    input_mint: &str,
    output_mint: &str,
    amount_in: u64,
) -> Result<MeteoraQuoteResponse> {
    println!("\nCalculating quote for:");
    println!("Pool: {}", pool.pool_address);
    println!("Input mint: {}", input_mint);
    println!("Output mint: {}", output_mint);
    println!("Amount in: {}", amount_in);

    // Find the indices for input and output tokens
    let (in_idx, out_idx) = if pool.pool_token_mints[0] == input_mint {
        (0, 1)
    } else {
        (1, 0)
    };
    println!("Input token index: {}", in_idx);
    println!("Output token index: {}", out_idx);

    // Parse pool amounts
    let in_amount_pool: f64 = pool.pool_token_amounts[in_idx].parse().map_err(|e| {
        anyhow!(
            "Failed to parse input amount: {} - Value: {}",
            e,
            pool.pool_token_amounts[in_idx]
        )
    })?;
    let out_amount_pool: f64 = pool.pool_token_amounts[out_idx].parse().map_err(|e| {
        anyhow!(
            "Failed to parse output amount: {} - Value: {}",
            e,
            pool.pool_token_amounts[out_idx]
        )
    })?;
    println!("Pool input amount: {}", in_amount_pool);
    println!("Pool output amount: {}", out_amount_pool);

    // Calculate fee
    let fee_pct: f64 = pool
        .total_fee_pct
        .trim_end_matches('%')
        .parse()
        .map_err(|e| {
            anyhow!(
                "Failed to parse fee percentage: {} - Value: {}",
                e,
                pool.total_fee_pct
            )
        })?;
    let fee_pct = fee_pct / 100.0;
    let amount_in_after_fee = amount_in as f64 * (1.0 - fee_pct);
    println!("Fee percentage: {}%", fee_pct * 100.0);
    println!("Amount after fee: {}", amount_in_after_fee);

    // Calculate out amount using constant product formula: (x + Δx)(y - Δy) = xy
    let amount_out =
        (out_amount_pool * amount_in_after_fee) / (in_amount_pool + amount_in_after_fee);
    let fee_amount = (amount_in as f64 * fee_pct) as u64;
    println!("Calculated output amount: {}", amount_out);
    println!("Fee amount: {}", fee_amount);

    // Calculate price impact
    let price_before = out_amount_pool / in_amount_pool;
    let price_after = (out_amount_pool - amount_out) / (in_amount_pool + amount_in as f64);
    let price_impact = ((price_before - price_after) / price_before * 100.0).to_string();
    println!("Price before: {}", price_before);
    println!("Price after: {}", price_after);
    println!("Price impact: {}%", price_impact);

    let quote = MeteoraQuoteResponse {
        pool_address: pool.pool_address.clone(),
        input_mint: input_mint.to_string(),
        output_mint: output_mint.to_string(),
        in_amount: amount_in.to_string(),
        out_amount: amount_out.to_string(),
        fee_amount: fee_amount.to_string(),
        price_impact,
    };
    println!("\nFinal quote: {:?}", quote);

    Ok(quote)
}

pub async fn get_meteora_swap_transaction(
    client: &Client,
    quote: &MeteoraQuoteResponse,
    user_pubkey: &str,
    rpc_client: &RpcClient,
    wallet: &Keypair,
) -> Result<Transaction> {
    println!("\nBuilding swap transaction:");
    println!("Pool: {}", quote.pool_address);
    println!("User: {}", user_pubkey);
    println!("Input mint: {}", quote.input_mint);
    println!("Output mint: {}", quote.output_mint);
    println!("Amount in: {}", quote.in_amount);

    let program_id = Pubkey::from_str(METEORA_PROGRAM_ID)
        .map_err(|e| anyhow!("Failed to parse program ID: {}", e))?;
    let pool = Pubkey::from_str(&quote.pool_address)
        .map_err(|e| anyhow!("Failed to parse pool address: {}", e))?;
    let user =
        Pubkey::from_str(user_pubkey).map_err(|e| anyhow!("Failed to parse user pubkey: {}", e))?;
    let input_mint = Pubkey::from_str(&quote.input_mint)
        .map_err(|e| anyhow!("Failed to parse input mint: {}", e))?;
    let output_mint = Pubkey::from_str(&quote.output_mint)
        .map_err(|e| anyhow!("Failed to parse output mint: {}", e))?;
    let amount_in = quote
        .in_amount
        .parse::<u64>()
        .map_err(|e| anyhow!("Failed to parse amount in: {}", e))?;

    // Get program states
    let program_amm_client =
        RpcClient::new_with_commitment(rpc_client.url(), CommitmentConfig::finalized());
    let account = program_amm_client.get_account(&to_solana_pubkey(&pool))?;
    let mut data = &account.data[..];
    let pool_state = Pool::try_deserialize_unchecked(&mut data)?;

    let program_vault_client =
        RpcClient::new_with_commitment(rpc_client.url(), CommitmentConfig::finalized());
    let account = program_vault_client.get_account(&to_solana_pubkey(&pool_state.a_vault))?;
    let mut data = &account.data[..];
    let a_vault_state = Vault::try_deserialize_unchecked(&mut data)?;
    let account = program_vault_client.get_account(&to_solana_pubkey(&pool_state.b_vault))?;
    let mut data = &account.data[..];
    let b_vault_state = Vault::try_deserialize_unchecked(&mut data)?;

    // Calculate minimum out amount with 0.5% slippage
    let min_amount_out = (quote
        .out_amount
        .parse::<f64>()
        .map_err(|e| anyhow!("Failed to parse out amount: {}", e))?
        * 0.995) as u64;

    // Build instructions vector
    let mut instructions = Vec::new();

    // Add compute budget instructions
    instructions
        .push(solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(1));
    instructions.push(
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(200_000),
    );

    // Get user token accounts
    let user_source_token =
        spl_associated_token_account::get_associated_token_address(&user, &input_mint);
    let user_destination_token =
        spl_associated_token_account::get_associated_token_address(&user, &output_mint);

    // Create token accounts if they don't exist
    if rpc_client.get_account(&user_destination_token).is_err() {
        println!("Creating destination token account...");
        instructions.push(
            spl_associated_token_account::instruction::create_associated_token_account(
                &wallet.pubkey(),
                &user,
                &output_mint,
                &spl_token::id(),
            ),
        );
    }

    // Add swap instruction
    instructions.push(Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(pool, false),                            // Pool
            AccountMeta::new(user_source_token, false),               // User source token
            AccountMeta::new(user_destination_token, false),          // User destination token
            AccountMeta::new(pool_state.a_vault_lp, false),           // A vault LP
            AccountMeta::new(pool_state.b_vault_lp, false),           // B vault LP
            AccountMeta::new(pool_state.a_vault, false),              // A vault
            AccountMeta::new(pool_state.b_vault, false),              // B vault
            AccountMeta::new_readonly(a_vault_state.lp_mint, false),  // A vault LP mint
            AccountMeta::new_readonly(b_vault_state.lp_mint, false),  // B vault LP mint
            AccountMeta::new(a_vault_state.token_vault, false),       // A token vault
            AccountMeta::new(b_vault_state.token_vault, false),       // B token vault
            AccountMeta::new(user, true),                             // User (signer)
            AccountMeta::new_readonly(prog_dynamic_vault::ID, false), // Vault program
            AccountMeta::new_readonly(spl_token::id(), false),        // Token program
            AccountMeta::new(pool_state.protocol_token_a_fee, false), // Protocol fee A
        ],
        data: {
            let mut data = vec![
                248, 198, 158, 145, 225, 117, 135, 200, // Anchor discriminator for "swap"
            ];
            data.extend_from_slice(&amount_in.to_le_bytes());
            data.extend_from_slice(&min_amount_out.to_le_bytes());
            data
        },
    });

    // Create transaction
    let recent_blockhash = rpc_client.get_latest_blockhash()?;
    let transaction =
        Transaction::new_signed_with_payer(&instructions, Some(&user), &[wallet], recent_blockhash);

    println!("\nTransaction created successfully");
    Ok(transaction)
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
