use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};
use std::{str::FromStr, time::Duration};
use tokio::time::sleep;

use crate::meteora::{self, MeteoraPool};

const MAX_RETRIES: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(1);

pub struct TradeExecutor {
    rpc_client: RpcClient,
    wallet: Keypair,
    http_client: Client,
}

impl TradeExecutor {
    pub fn new(rpc_url: &str, wallet_keypair: Keypair) -> Self {
        let rpc_client =
            RpcClient::new_with_commitment(rpc_url.to_string(), CommitmentConfig::confirmed());
        let http_client = Client::new();
        Self {
            rpc_client,
            wallet: wallet_keypair,
            http_client,
        }
    }

    /// Execute a trade on Meteora
    pub async fn execute_trade(
        &self,
        pool: &MeteoraPool,
        input_mint: &str,
        output_mint: &str,
        amount_in: u64,
        slippage_bps: u64,
    ) -> Result<Signature> {
        // Check balance before trading
        self.check_token_balance(input_mint, amount_in).await?;

        for retry in 0..MAX_RETRIES {
            if retry > 0 {
                println!(
                    "Retrying trade execution (attempt {}/{})",
                    retry + 1,
                    MAX_RETRIES
                );
                sleep(RETRY_DELAY).await;
            }

            match self
                .execute_trade_internal(pool, input_mint, output_mint, amount_in)
                .await
            {
                Ok(sig) => {
                    println!("Trade executed successfully! Signature: {}", sig);
                    println!("View transaction: https://solscan.io/tx/{}", sig);
                    return Ok(sig);
                }
                Err(e) if retry < MAX_RETRIES - 1 => {
                    println!("Trade execution failed: {}. Retrying...", e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(anyhow!("Max retries exceeded"))
    }

    async fn execute_trade_internal(
        &self,
        pool: &MeteoraPool,
        input_mint: &str,
        output_mint: &str,
        amount_in: u64,
    ) -> Result<Signature> {
        // 1. Get quote from Meteora
        let quote =
            meteora::get_meteora_quote(&self.http_client, pool, input_mint, output_mint, amount_in)
                .await?;

        println!(
            "Got quote: {} -> {} ({} -> {})",
            input_mint, output_mint, quote.in_amount, quote.out_amount
        );

        // 2. Build swap transaction
        let tx = meteora::get_meteora_swap_transaction(
            &self.http_client,
            &quote,
            &self.wallet.pubkey().to_string(),
            &self.rpc_client,
            &self.wallet,
        )
        .await?;

        // 3. Simulate transaction with detailed error reporting
        match self.simulate_transaction(&tx).await {
            Ok(_) => println!("Transaction simulation successful"),
            Err(e) => {
                println!("Transaction simulation failed: {}", e);
                return Err(e);
            }
        }

        // 4. Send and confirm transaction
        self.send_and_confirm_transaction(&tx).await
    }

    /// Check if the wallet has sufficient balance for the trade
    async fn check_token_balance(&self, mint: &str, amount: u64) -> Result<()> {
        let token_account = self.find_token_account(mint)?;
        let balance = self.rpc_client.get_token_account_balance(&token_account)?;

        if balance.ui_amount.unwrap_or(0.0) * 10f64.powi(balance.decimals as i32) < amount as f64 {
            return Err(anyhow!(
                "Insufficient balance: have {} {}, need {}",
                balance.ui_amount.unwrap_or(0.0),
                mint,
                amount
            ));
        }

        Ok(())
    }

    /// Find the associated token account for a given mint
    fn find_token_account(&self, mint: &str) -> Result<Pubkey> {
        let mint_pubkey = Pubkey::from_str(mint)?;
        let owner = self.wallet.pubkey();

        Ok(spl_associated_token_account::get_associated_token_address(
            &owner,
            &mint_pubkey,
        ))
    }

    /// Simulate a transaction before sending
    async fn simulate_transaction(&self, transaction: &Transaction) -> Result<()> {
        self.rpc_client
            .simulate_transaction(transaction)
            .map_err(|e| anyhow!("Transaction simulation failed: {}", e))?;
        Ok(())
    }

    /// Send and confirm a transaction
    async fn send_and_confirm_transaction(&self, transaction: &Transaction) -> Result<Signature> {
        let signature = self
            .rpc_client
            .send_and_confirm_transaction(transaction)
            .map_err(|e| anyhow!("Failed to send transaction: {}", e))?;
        Ok(signature)
    }

    /// Ensure a token account exists for the given mint, create if it doesn't
    pub async fn ensure_token_account(&self, mint: &str) -> Result<()> {
        let token_account = self.find_token_account(mint)?;
        if self.rpc_client.get_account(&token_account).is_err() {
            println!("Creating token account for mint: {}", mint);
            // Create ATA instruction
            let create_ata_ix = spl_associated_token_account::create_associated_token_account(
                &self.wallet.pubkey(),
                &self.wallet.pubkey(),
                &Pubkey::from_str(mint)?,
            );
            // Build and send transaction
            let recent_blockhash = self.rpc_client.get_latest_blockhash()?;
            let tx = Transaction::new_signed_with_payer(
                &[create_ata_ix],
                Some(&self.wallet.pubkey()),
                &[&self.wallet],
                recent_blockhash,
            );
            self.rpc_client.send_and_confirm_transaction(&tx)?;
            println!("Created token account for mint: {}", mint);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_trade_executor() {
        // TODO: Add tests
    }
}
