
use std::time::{Duration};

use error_stack::{Report, Result, ResultExt};
use thiserror::Error;

use cosmrs::tx::Fee;
use cosmrs::crypto::secp256k1::SigningKey;
use cosmrs::Denom;
use tendermint::chain::Id;

use crate::broadcaster::BroadcasterError::*;
use crate::tm_client::{TmClient, BroadcastResponse};

pub mod account_client;
mod helpers;

#[derive(Error, Debug)]
pub enum BroadcasterError {
    #[error("failed to connect to node")]
    ConnectionFailed,
    #[error("tx marshaling failed")]
    TxMarshalingFailed,
    #[error("tx simulation failed")]
    TxSimulationFailed,
    #[error("unable to resolve account sequence mismatch during simulation")]
    UnresolvedAccountSequenceMismatch,
    #[error("timeout for tx inclusion in block")]
    BlockInclusionTimeout,
    #[error("failed to update local context")]
    ContextUpdateFailed,
    #[error("failed to estimate gas")]
    GasEstimationFailed,
}

#[derive(Clone)]
pub struct BroadcastOptions {
    pub tx_fetch_interval: Duration,
    pub tx_fetch_max_retries: u32,
    pub sim_sequence_mismatch_retries: u32,
    pub gas_adjustment: f32,
    pub gas_price: (f64, Denom),
}

pub struct Broadcaster<T: TmClient + Clone, A: account_client::AccountClient> {
    tm_client: T,
    account_client: A,
    priv_key: SigningKey,
    options: BroadcastOptions,
    chain_id: Option<Id>,
}

impl<T: TmClient + Clone, C: account_client::AccountClient> Broadcaster<T,C> {
    pub fn new(tm_client: T, account_client: C, options: BroadcastOptions, priv_key: SigningKey) -> Self {
        Broadcaster { tm_client, account_client, options, priv_key, chain_id: None}
    }

    pub async fn broadcast<M>(&mut self, msgs: M) -> Result<BroadcastResponse,BroadcasterError>
    where M: IntoIterator<Item = cosmrs::Any> + Clone,
    {
        if self.account_client.sequence().is_none() || self.account_client.account_number().is_none() {
            self.account_client.update().await.change_context(ContextUpdateFailed)?;
        }

        if self.chain_id.is_none() {
            self.chain_id = Some(self.tm_client.clone().get_status().await.change_context(ContextUpdateFailed)?.node_info.network);
        }
        
        let account_number = self.account_client.account_number().ok_or(ContextUpdateFailed)?;
        let chain_id = self.chain_id.clone().ok_or(ContextUpdateFailed)?;
        let gas_adjustment = self.options.gas_adjustment;

        let mut sequence = 0;
        let mut estimated_gas = 0;
        let mut last_error = Report::new(UnresolvedAccountSequenceMismatch);

        // retry tx simulation in case there are sequence mismatches
        for _ in 0..self.options.sim_sequence_mismatch_retries {
            sequence = self.account_client.sequence().ok_or(ContextUpdateFailed)?;
            let tx_bytes = helpers::generate_sim_tx(msgs.clone(), sequence, &self.priv_key.public_key())?;

            match self.account_client.estimate_gas(tx_bytes.clone()).await {
                Ok(gas) => {
                    estimated_gas = gas;
                    break;
                }
                Err(err) => {
                    match err.current_context() {
                        account_client::AccountClientError::AccountSequenceMismatch => {
                            last_error = err.change_context(UnresolvedAccountSequenceMismatch);
                            self.account_client.update().await.change_context(ContextUpdateFailed)?;
                            continue;
                        }
                        _ => return Err(err).change_context(GasEstimationFailed),
                    }
                }
            }
        }

        if estimated_gas == 0 {
            return Err(last_error).change_context(GasEstimationFailed);
        }

        let mut gas_limit = estimated_gas;
        if self.options.gas_adjustment > 0.0 {
            gas_limit = (gas_limit as f64 * gas_adjustment as f64) as u64;
        }

        let (value,denom) = self.options.gas_price.clone();
        let amount = cosmrs::Coin{
            amount:  (gas_limit as f64 * value).ceil() as u128,
            denom: denom,
        };

        let fee = Fee::from_amount_and_gas(amount, gas_limit);
        let tx_bytes= helpers::generate_tx(
            msgs.clone(),
            &self.priv_key,
            account_number,
            sequence,
            fee,
            chain_id
        )?;
        let response = self.tm_client.broadcast(tx_bytes).await.change_context(ConnectionFailed)?;

        helpers::wait_for_block_inclusion(
            self.tm_client.clone(),
            response.hash,
            self.options.tx_fetch_interval,
            self.options.tx_fetch_max_retries,
        ).await?;

        //update sequence number
        self.account_client.update().await.change_context(ContextUpdateFailed)?;

        Ok(response)

    }
}
