use anyhow::{anyhow, Result};
use clap::Parser;
use libcheese::common::USDC_MINT;
use libcheese::common::{is_blacklisted, parse_other_token_name, CHEESE_MINT};
use libcheese::jupiter::fetch_jupiter_prices;
use libcheese::meteora::{fetch_meteora_cheese_pools, MeteoraPool};
use libcheese::raydium::{fetch_raydium_cheese_pools, fetch_raydium_mint_ids};
use libcheese::solana::TradeExecutor;
use reqwest::Client;
use solana_sdk::signer::keypair::read_keypair_file;
use solana_sdk::signer::Signer;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time;

const SOL_PER_TX: f64 = 0.000005; // Approximate SOL cost per transaction
const LOOP_INTERVAL: Duration = Duration::from_secs(30);
const MIN_PROFIT_USD: f64 = 1.0; // Minimum profit in USD to execute trade
const MAX_USDC_INPUT: f64 = 10.0; // Maximum USDC input for any trade

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Run mode (hot/cold)
    mode: String,

    /// RPC URL (optional, defaults to mainnet)
    #[arg(long)]
    rpc_url: Option<String>,

    /// Keypair file path (required for hot mode)
    #[arg(long)]
    keypair: Option<String>,
}

/// A row describing one pool
#[derive(Debug)]
struct DisplayPool {
    source: String,
    other_mint: String,
    other_symbol: String,
    cheese_qty: String,
    other_qty: String,
    pool_version: String,
    tvl: String,
    volume_usd: String,
    fee: String,
    pool_address: String,
    cheese_price: String,
}

#[derive(Debug, Default)]
struct CheeseAggregates {
    total_liquidity_usd: f64,
    number_of_pools: u64,
    total_cheese_qty: f64,
    total_volume_24h: f64,
}

#[derive(Debug, Clone)]
struct PoolEdge {
    pool_address: String,
    source: String, // "Meteora" or "Raydium"
    token_a: String,
    token_b: String,
    fee: f64,
    tvl: f64,
    reserves_a: f64,
    reserves_b: f64,
}

#[derive(Debug)]
struct ArbitrageCycle {
    steps: Vec<TradeStep>,
    initial_cheese: f64,
    final_cheese: f64,
    total_fees_sol: f64,
    pool_fees_paid: Vec<f64>,
    initial_usdc_value: f64, // Value of initial CHEESE in USDC
    final_usdc_value: f64,   // Value of final CHEESE in USDC
    fees_usdc_value: f64,    // Value of all fees in USDC
}

#[derive(Debug, Clone)]
struct TradeStep {
    pool_address: String,
    source: String,
    sell_token: String,
    buy_token: String,
    amount_in: f64,
    expected_out: f64,
    fee_percent: f64,
}

#[derive(Debug)]
struct ArbitrageOpportunity {
    pool_address: String,
    symbol: String,
    cheese_qty: f64,
    other_qty: f64,
    implied_price: f64,
    usdc_price: f64,
    max_trade_size: f64,
    net_profit_usd: f64,
    is_sell: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // If hot mode, validate keypair
    let executor = if args.mode == "hot" {
        if args.keypair.is_none() {
            eprintln!("Error: --keypair is required in hot mode");
            std::process::exit(1);
        }

        let keypair_path = args.keypair.unwrap();
        let keypair = read_keypair_file(&keypair_path)
            .map_err(|e| anyhow!("Failed to read keypair file: {}", e))?;

        println!("\n=== Wallet Information ===");
        println!("Address: {}", keypair.pubkey());

        let rpc_url = args
            .rpc_url
            .unwrap_or_else(|| "https://api.mainnet-beta.solana.com".to_string());

        let executor = TradeExecutor::new(&rpc_url, keypair);

        // Get and display SOL balance
        let sol_balance = executor
            .rpc_client
            .get_balance(&executor.wallet.pubkey())
            .map_err(|e| anyhow!("Failed to get SOL balance: {}", e))?;
        println!("SOL balance: {} SOL", sol_balance as f64 / 1_000_000_000.0);

        // Get and display USDC balance
        let usdc_balance = match executor.get_token_balance(&USDC_MINT.parse()?).await {
            Ok(balance) => balance,
            Err(_) => 0,
        };
        println!("USDC balance: {} USDC", usdc_balance as f64 / 1_000_000.0);

        // Get and display CHEESE balance
        let cheese_balance = match executor.get_token_balance(&CHEESE_MINT.parse()?).await {
            Ok(balance) => balance,
            Err(_) => 0,
        };
        println!(
            "CHEESE balance: {} CHEESE",
            cheese_balance as f64 / 1_000_000.0
        );
        println!("=====================\n");

        Some(executor)
    } else {
        None
    };

    loop {
        if let Err(e) = run_iteration(&executor).await {
            eprintln!("Error in iteration: {}", e);
        }

        if args.mode != "hot" {
            break;
        }

        time::sleep(LOOP_INTERVAL).await;
    }

    Ok(())
}

async fn run_iteration(executor: &Option<TradeExecutor>) -> Result<()> {
    let client = Client::new();

    // 1) fetch from Meteora
    let meteora_pools = fetch_meteora_cheese_pools(&client).await?;

    // 2) fetch from Raydium
    let raydium_pools = fetch_raydium_cheese_pools(&client).await?;

    // gather unique mints
    let mut set = HashSet::new();
    set.insert(CHEESE_MINT.to_string());
    for pool in &meteora_pools {
        for m in &pool.pool_token_mints {
            set.insert(m.clone());
        }
    }
    let mut all_mints_vec: Vec<String> = set.into_iter().collect();
    all_mints_vec.sort();

    // fetch minted data from Raydium
    let minted_data = fetch_raydium_mint_ids(&client, &all_mints_vec).await?;
    let mut mint_to_symbol = HashMap::new();
    for maybe_item in &minted_data {
        if let Some(item) = maybe_item {
            mint_to_symbol.insert(item.address.clone(), item.symbol.clone());
        }
    }

    // fetch Jupiter prices
    let jup_prices = fetch_jupiter_prices(&client, &all_mints_vec).await?;

    // Find the USDC/CHEESE price from the specific pool
    let usdc_pool = meteora_pools
        .iter()
        .find(|p| p.pool_address == "2rkTh46zo8wUvPJvACPTJ16RNUHEM9EZ1nLYkUxZEHkw")
        .unwrap();
    let (cheese_ix, usdc_ix) = if usdc_pool.pool_token_mints[0] == CHEESE_MINT {
        (0, 1)
    } else {
        (1, 0)
    };
    let cheese_amt: f64 = usdc_pool.pool_token_amounts[cheese_ix]
        .parse()
        .unwrap_or(0.0);
    let usdc_amt: f64 = usdc_pool.pool_token_amounts[usdc_ix].parse().unwrap_or(0.0);
    let cheese_usdc_price = if cheese_amt > 0.0 {
        usdc_amt / cheese_amt
    } else {
        0.0
    };

    // Print table header
    println!("\n| Source   | Other Mint                                   | Other Name | Version    | CHEESE Qty | Other Qty | Liquidity($) | Volume($) |   Fee | CHEESE Price | Pool Address                                 |");
    println!("|----------|----------------------------------------------|------------|------------|------------|-----------|--------------|-----------|-------|--------------|----------------------------------------------|");

    // Prepare display pools
    let mut display_pools = Vec::new();
    let mut aggregates = CheeseAggregates::default();

    // Add Meteora pools
    for pool in &meteora_pools {
        let (cheese_ix, other_ix) = if pool.pool_token_mints[0] == CHEESE_MINT {
            (0, 1)
        } else {
            (1, 0)
        };

        let other_mint = pool.pool_token_mints[other_ix].clone();

        // Skip blacklisted tokens
        if is_blacklisted(&other_mint) {
            continue;
        }

        let other_symbol = mint_to_symbol
            .get(&other_mint)
            .cloned()
            .unwrap_or_else(|| parse_other_token_name(&pool.pool_name));

        let cheese_qty: f64 = pool.pool_token_amounts[cheese_ix].parse().unwrap_or(0.0);
        let other_qty: f64 = pool.pool_token_amounts[other_ix].parse().unwrap_or(0.0);

        // Update aggregates
        aggregates.number_of_pools += 1;
        aggregates.total_cheese_qty += cheese_qty;
        aggregates.total_liquidity_usd += pool.pool_tvl;
        aggregates.total_volume_24h += pool.daily_volume;

        display_pools.push(DisplayPool {
            source: "Meteora".to_string(),
            other_mint: other_mint.to_string(),
            other_symbol: other_symbol.to_string(),
            cheese_qty: format!("{:.2}", cheese_qty),
            other_qty: format!("{:.2}", other_qty),
            pool_version: pool.pool_version.to_string(),
            tvl: format!("{:.2}", pool.pool_tvl),
            volume_usd: format!("{:.2}", pool.daily_volume),
            fee: format!("{:.2}%", pool.total_fee_pct.trim_end_matches('%')),
            pool_address: pool.pool_address.clone(),
            cheese_price: format!("${:.6}", cheese_usdc_price),
        });
    }

    // Add Raydium pools
    for pool in &raydium_pools {
        let (cheese_qty, other_qty, other_mint, other_symbol) = if pool.mintA.address == CHEESE_MINT
        {
            (
                pool.mint_amount_a,
                pool.mint_amount_b,
                pool.mintB.address.clone(),
                pool.mintB.symbol.clone(),
            )
        } else {
            (
                pool.mint_amount_b,
                pool.mint_amount_a,
                pool.mintA.address.clone(),
                pool.mintA.symbol.clone(),
            )
        };

        // Update aggregates
        aggregates.number_of_pools += 1;
        aggregates.total_cheese_qty += cheese_qty;
        aggregates.total_liquidity_usd += pool.tvl;
        aggregates.total_volume_24h += pool.day.volume;

        display_pools.push(DisplayPool {
            source: "Raydium".to_string(),
            other_mint,
            other_symbol,
            cheese_qty: format!("{:.2}", cheese_qty),
            other_qty: format!("{:.2}", other_qty),
            pool_version: pool.r#type.clone(),
            tvl: format!("{:.2}", pool.tvl),
            volume_usd: format!("{:.2}", pool.day.volume),
            fee: format!("{:.2}%", pool.feeRate * 100.0),
            pool_address: pool.pool_id.clone(),
            cheese_price: format!("${:.6}", cheese_usdc_price),
        });
    }

    // Sort by TVL
    display_pools.sort_by(|a, b| {
        b.tvl
            .parse::<f64>()
            .unwrap_or(0.0)
            .partial_cmp(&a.tvl.parse::<f64>().unwrap_or(0.0))
            .unwrap()
    });

    // Print pools
    for pool in &display_pools {
        // Calculate derived price if available
        let derived_price = if let Some(price) = jup_prices.get(&pool.other_mint) {
            let other_qty = pool.other_qty.parse::<f64>().unwrap_or(0.0);
            let cheese_qty = pool.cheese_qty.parse::<f64>().unwrap_or(0.0);
            if cheese_qty > 0.0 {
                (other_qty * price) / cheese_qty
            } else {
                0.0
            }
        } else {
            0.0
        };

        // Use derived price for TVL if available
        let tvl = if derived_price > 0.0 {
            let cheese_qty = pool.cheese_qty.parse::<f64>().unwrap_or(0.0);
            cheese_qty * derived_price * 2.0 // multiply by 2 since it's both sides of the pool
        } else {
            pool.tvl.parse::<f64>().unwrap_or(0.0)
        };

        println!(
            "| {:8} | {:44} | {:10} | {:10} | {:10} | {:9} | {:12} | {:9} | {:5} | {:12} | {:44} |",
            pool.source,
            pool.other_mint,
            pool.other_symbol,
            pool.pool_version,
            pool.cheese_qty,
            pool.other_qty,
            format!("{:.2}", tvl),
            pool.volume_usd,
            pool.fee,
            if derived_price > 0.0 {
                format!("${:.6}", derived_price)
            } else {
                "N/A".to_string()
            },
            pool.pool_address,
        );
    }

    // Print summary
    println!("\n===== 🧀 Aggregates =====");
    println!(
        "Total Liquidity (USD):   ${:.2}",
        aggregates.total_liquidity_usd
    );
    println!(
        "Total 24H Volume (USD): ${:.2}",
        aggregates.total_volume_24h
    );
    println!("Number of pools:        {}", aggregates.number_of_pools);
    println!("Total 🧀 in pools:      {:.2}", aggregates.total_cheese_qty);
    println!("===========================\n");

    // Process pools and find opportunities
    let opportunities = find_arbitrage_opportunities(&meteora_pools, cheese_usdc_price)?;

    // Print opportunities
    for opp in opportunities
        .iter()
        .filter(|o| o.net_profit_usd >= MIN_PROFIT_USD)
    {
        // Get the pool for this opportunity
        let pool = meteora_pools
            .iter()
            .find(|p| p.pool_address == opp.pool_address)
            .unwrap();
        let fee_percent = pool.total_fee_pct.trim_end_matches('%').parse::<f64>()? / 100.0;

        println!("\nPool: {} ({})", opp.pool_address, opp.symbol);
        println!("├─ Implied CHEESE price: ${:.10}", opp.implied_price);
        println!(
            "├─ Price difference: {:.2}%",
            ((opp.implied_price - opp.usdc_price) / opp.usdc_price) * 100.0
        );
        println!("├─ Pool liquidity:");
        println!("│  ├─ 🧀: {:.2}", opp.cheese_qty);
        println!("│  └─ {}: {:.2}", opp.symbol, opp.other_qty);
        println!("├─ Fees:");
        println!("│  ├─ USDC->CHEESE fee: 0.25%");
        println!("│  ├─ CHEESE->Target fee: {:.2}%", fee_percent * 100.0);
        println!("│  ├─ Target->CHEESE fee: {:.2}%", fee_percent * 100.0);
        println!("│  ├─ CHEESE->USDC fee: 0.25%");
        println!(
            "│  └─ Transaction cost: ${:.4} (4 transactions)",
            SOL_PER_TX * 4.0
        );
        println!("├─ Trade path:");
        println!(
            "│  1. Buy {:.2} USDC worth of CHEESE at ${:.10}",
            opp.max_trade_size * opp.usdc_price,
            opp.usdc_price
        );
        println!(
            "│  2. Trade CHEESE for {} at ${:.10}",
            opp.symbol, opp.implied_price
        );
        println!("│  3. Trade {} back to CHEESE", opp.symbol);
        println!("│  4. Sell CHEESE for USDC at ${:.10}", opp.usdc_price);
        println!("└─ Profitability:");
        let gross_profit = opp.max_trade_size * (opp.implied_price - opp.usdc_price).abs();
        let total_fees = (opp.max_trade_size * opp.usdc_price * 0.0025) + // First USDC->CHEESE 0.25%
                        (opp.max_trade_size * opp.implied_price * fee_percent) +   // CHEESE->Target fee
                        (opp.max_trade_size * opp.implied_price * fee_percent) +   // Target->CHEESE fee
                        (opp.max_trade_size * opp.usdc_price * 0.0025) +   // Final CHEESE->USDC 0.25%
                        (SOL_PER_TX * 4.0); // 4 transactions total
        println!("   ├─ Gross profit: ${:.4}", gross_profit);
        println!("   ├─ Total fees: ${:.4}", total_fees);
        println!("   └─ Net profit: ${:.4}", opp.net_profit_usd);

        // Execute trade if in hot mode
        if let Some(executor) = executor {
            println!("\n=== Starting Trade Execution ===");
            println!("Trade details:");
            println!("- Is sell: {}", opp.is_sell);
            println!("- Max trade size: {}", opp.max_trade_size);
            println!("- USDC price: {}", opp.usdc_price);
            println!("- Implied price: {}", opp.implied_price);
            println!("- Net profit USD: {}", opp.net_profit_usd);
            println!("- Pool address: {}", opp.pool_address);
            println!("- Symbol: {}", opp.symbol);

            // Get the other token's index
            let (_, other_ix) = if pool.pool_token_mints[0] == CHEESE_MINT {
                (0, 1)
            } else {
                (1, 0)
            };
            println!("\nPool details:");
            println!("- Pool token mints: {:?}", pool.pool_token_mints);
            println!("- Pool token amounts: {:?}", pool.pool_token_amounts);
            println!("- Other token index: {}", other_ix);

            // Ensure all necessary token accounts exist before trading
            println!("\nEnsuring token accounts exist...");
            executor.ensure_token_account(USDC_MINT).await?;
            executor.ensure_token_account(CHEESE_MINT).await?;
            executor
                .ensure_token_account(&pool.pool_token_mints[other_ix])
                .await?;

            if opp.is_sell {
                println!("\nExecuting sell path: USDC -> CHEESE -> Target -> CHEESE -> USDC");

                // 1. USDC -> CHEESE on Meteora
                let amount_in_usdc = ((opp.max_trade_size * opp.usdc_price) as u64) * 1_000_000; // Convert to USDC lamports (6 decimals)
                println!("\nStep 1: USDC -> CHEESE");
                println!("Amount in USDC: {}", amount_in_usdc as f64 / 1_000_000.0); // Display in human-readable USDC
                let sig1 = executor
                    .execute_trade(
                        usdc_pool,
                        USDC_MINT,
                        CHEESE_MINT,
                        amount_in_usdc,
                        50, // 0.5% slippage
                    )
                    .await?;
                println!("1. USDC -> CHEESE: {}", sig1);

                // 2. CHEESE -> Target token
                let amount_in_cheese = (opp.max_trade_size * 1_000_000_000.0) as u64;
                println!("\nStep 2: CHEESE -> {}", opp.symbol);
                println!("Amount in CHEESE: {}", amount_in_cheese);
                let sig2 = executor
                    .execute_trade(
                        pool,
                        CHEESE_MINT,
                        &pool.pool_token_mints[other_ix],
                        amount_in_cheese,
                        50,
                    )
                    .await?;
                println!("2. CHEESE -> {}: {}", opp.symbol, sig2);

                // 3. Target -> CHEESE
                let amount_in_target = (opp.other_qty * 0.1 * 1_000_000_000.0) as u64; // 10% of target token liquidity
                println!("\nStep 3: {} -> CHEESE", opp.symbol);
                println!("Amount in {}: {}", opp.symbol, amount_in_target);
                let sig3 = executor
                    .execute_trade(
                        pool,
                        &pool.pool_token_mints[other_ix],
                        CHEESE_MINT,
                        amount_in_target,
                        50,
                    )
                    .await?;
                println!("3. {} -> CHEESE: {}", opp.symbol, sig3);

                // 4. CHEESE -> USDC
                println!("\nStep 4: CHEESE -> USDC");
                println!("Amount in CHEESE: {}", amount_in_cheese);
                let sig4 = executor
                    .execute_trade(usdc_pool, CHEESE_MINT, USDC_MINT, amount_in_cheese, 50)
                    .await?;
                println!("4. CHEESE -> USDC: {}", sig4);
            } else {
                println!("\nExecuting buy path: USDC -> CHEESE -> Target -> CHEESE -> USDC");

                // 1. USDC -> CHEESE on Meteora
                let amount_in_usdc = (opp.max_trade_size * opp.usdc_price * 1_000_000.0) as u64;
                println!("\nStep 1: USDC -> CHEESE");
                println!("Amount in USDC: {}", amount_in_usdc);
                let sig1 = executor
                    .execute_trade(usdc_pool, USDC_MINT, CHEESE_MINT, amount_in_usdc, 50)
                    .await?;
                println!("1. USDC -> CHEESE: {}", sig1);

                // 2. CHEESE -> Target token
                let amount_in_cheese = (opp.max_trade_size * 1_000_000_000.0) as u64;
                println!("\nStep 2: CHEESE -> {}", opp.symbol);
                println!("Amount in CHEESE: {}", amount_in_cheese);
                let sig2 = executor
                    .execute_trade(
                        pool,
                        CHEESE_MINT,
                        &pool.pool_token_mints[other_ix],
                        amount_in_cheese,
                        50,
                    )
                    .await?;
                println!("2. CHEESE -> {}: {}", opp.symbol, sig2);

                // 3. Target -> CHEESE
                let amount_in_target = (opp.other_qty * 0.1 * 1_000_000_000.0) as u64; // 10% of target token liquidity
                println!("\nStep 3: {} -> CHEESE", opp.symbol);
                println!("Amount in {}: {}", opp.symbol, amount_in_target);
                let sig3 = executor
                    .execute_trade(
                        pool,
                        &pool.pool_token_mints[other_ix],
                        CHEESE_MINT,
                        amount_in_target,
                        50,
                    )
                    .await?;
                println!("3. {} -> CHEESE: {}", opp.symbol, sig3);

                // 4. CHEESE -> USDC
                println!("\nStep 4: CHEESE -> USDC");
                println!("Amount in CHEESE: {}", amount_in_cheese);
                let sig4 = executor
                    .execute_trade(usdc_pool, CHEESE_MINT, USDC_MINT, amount_in_cheese, 50)
                    .await?;
                println!("4. CHEESE -> USDC: {}", sig4);
            }
        }
    }

    Ok(())
}

fn find_arbitrage_opportunities(
    meteora_pools: &[MeteoraPool],
    cheese_usdc_price: f64,
) -> Result<Vec<ArbitrageOpportunity>> {
    let mut opportunities = Vec::new();

    for pool in meteora_pools {
        let (cheese_ix, other_ix) = if pool.pool_token_mints[0] == CHEESE_MINT {
            (0, 1)
        } else {
            (1, 0)
        };

        let other_mint = &pool.pool_token_mints[other_ix];

        // Skip blacklisted tokens
        if is_blacklisted(other_mint) {
            continue;
        }

        let cheese_qty: f64 = pool.pool_token_amounts[cheese_ix].parse()?;
        let other_qty: f64 = pool.pool_token_amounts[other_ix].parse()?;

        // If this is USDC, use price of 1.0
        let other_price = if other_mint == USDC_MINT {
            1.0 // USDC is always worth $1
        } else {
            cheese_usdc_price
        };

        let implied_price = (other_qty * other_price) / cheese_qty;
        let price_diff_pct = ((implied_price - cheese_usdc_price) / cheese_usdc_price) * 100.0;

        let fee_percent = pool.total_fee_pct.trim_end_matches('%').parse::<f64>()? / 100.0;

        if cheese_qty <= 0.0 || other_qty <= 0.0 {
            continue;
        }

        // If price difference is significant (>1%)
        if price_diff_pct.abs() > 1.0 {
            // Calculate max trade size based on pool liquidity
            let pool_based_size = if pool.pool_token_mints.contains(&USDC_MINT.to_string()) {
                cheese_qty * 0.1
            } else {
                cheese_qty * 0.05
            };

            // Limit by MAX_USDC_INPUT
            let max_trade_size = (MAX_USDC_INPUT / cheese_usdc_price).min(pool_based_size);

            let price_diff_per_cheese = (implied_price - cheese_usdc_price).abs();
            let gross_profit = max_trade_size * price_diff_per_cheese;

            // Calculate fees for the full USDC -> CHEESE -> Target -> CHEESE -> USDC path
            let total_fees = (max_trade_size * cheese_usdc_price * 0.0025) + // First USDC->CHEESE 0.25%
                           (max_trade_size * implied_price * fee_percent) +   // CHEESE->Target fee
                           (max_trade_size * implied_price * fee_percent) +   // Target->CHEESE fee
                           (max_trade_size * cheese_usdc_price * 0.0025) +   // Final CHEESE->USDC 0.25%
                           (SOL_PER_TX * 4.0); // 4 transactions total

            let net_profit = gross_profit - total_fees;

            if net_profit >= MIN_PROFIT_USD {
                opportunities.push(ArbitrageOpportunity {
                    pool_address: pool.pool_address.clone(),
                    symbol: parse_other_token_name(&pool.pool_name),
                    cheese_qty,
                    other_qty,
                    implied_price,
                    usdc_price: cheese_usdc_price,
                    max_trade_size,
                    net_profit_usd: net_profit,
                    is_sell: implied_price > cheese_usdc_price, // If true, we buy CHEESE in USDC pool and sell in target
                });
            }
        }
    }

    opportunities.sort_by(|a, b| b.net_profit_usd.partial_cmp(&a.net_profit_usd).unwrap());
    Ok(opportunities)
}
