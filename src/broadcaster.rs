use std::time::Duration;

use error_stack::{Result, ResultExt};
use thiserror::Error;

use cosmrs::crypto::secp256k1::SigningKey;
use cosmrs::tx::Fee;
use cosmrs::Denom;
use tendermint::chain::Id;

use crate::broadcaster::account_client::GasEstimator;
use crate::broadcaster::BroadcasterError::*;
use crate::tm_client::{BroadcastResponse, TmClient};

pub mod account_client;
mod helpers;

const AXELAR_CHAIN_ID: &str = "axelar-dojo-1";
const AXL_DENOM: &str = "uaxl";
const TX_INTERVAL_SECONDS: u64 = 0;
const TX_INTERVAL_MILISECONDS: u32 = 500;
const TX_MAX_RETRIES: u32 = 10;
const GAS_PRICE: f64 = 0.00005;
const GAS_ADJUSTMENT: f32 = 1.0;

#[derive(Error, Debug)]
pub enum BroadcasterError {
    #[error("broadcast failed")]
    BroadcastFailed,
    #[error("tx marshaling failed")]
    TxMarshalingFailed,
    #[error("timeout for tx inclusion in block")]
    BlockInclusionTimeout,
    #[error("failed to estimate gas")]
    GasEstimationFailed,
}

#[derive(Debug, Clone)]
struct BroadcastOptions {
    pub tx_fetch_interval: Duration,
    pub tx_fetch_max_retries: u32,
    pub gas_adjustment: f32,
    pub gas_price: (f64, Denom),
}

pub struct Broadcaster<T: TmClient, G: GasEstimator> {
    tm_client: T,
    gas_estimator: G,
    acc_number: u64,
    acc_sequence: u64,
    chain_id: Id,
    priv_key: SigningKey,
    options: BroadcastOptions,
}

pub struct BroadcasterBuilder<T: TmClient, G: GasEstimator> {
    tm_client: T,
    gas_estimator: G,
    acc_number: u64,
    acc_sequence: u64,
    chain_id: Id,
    priv_key: SigningKey,
    options: BroadcastOptions,
}

impl<T: TmClient, G: GasEstimator> BroadcasterBuilder<T, G> {
    pub fn new(tm_client: T, gas_estimator: G, priv_key: SigningKey) -> Self {
        BroadcasterBuilder {
            tm_client,
            gas_estimator,
            priv_key,
            chain_id: AXELAR_CHAIN_ID.parse::<Id>().unwrap(),
            acc_number: 0,
            acc_sequence: 0,
            options: BroadcastOptions {
                tx_fetch_interval: std::time::Duration::new(TX_INTERVAL_SECONDS, TX_INTERVAL_MILISECONDS),
                tx_fetch_max_retries: TX_MAX_RETRIES,
                gas_adjustment: GAS_ADJUSTMENT,
                gas_price: (GAS_PRICE, AXL_DENOM.parse().unwrap()),
            },
        }
    }

    pub fn build(self) -> Broadcaster<T, G> {
        Broadcaster {
            tm_client: self.tm_client,
            gas_estimator: self.gas_estimator,
            priv_key: self.priv_key,
            chain_id: self.chain_id,
            acc_number: self.acc_number,
            acc_sequence: self.acc_sequence,
            options: self.options,
        }
    }

    pub fn chain_id(mut self, chain_id: Id) -> BroadcasterBuilder<T, G> {
        self.chain_id = chain_id;
        self
    }
    pub fn acc_number(mut self, acc_number: u64) -> BroadcasterBuilder<T, G> {
        self.acc_number = acc_number;
        self
    }
    pub fn acc_sequence(mut self, acc_sequence: u64) -> BroadcasterBuilder<T, G> {
        self.acc_sequence = acc_sequence;
        self
    }
    pub fn tx_fetch_interval(mut self, tx_fetch_interval: Duration) -> BroadcasterBuilder<T, G> {
        self.options.tx_fetch_interval = tx_fetch_interval;
        self
    }
    pub fn tx_fetch_max_retries(mut self, tx_fetch_max_retries: u32) -> BroadcasterBuilder<T, G> {
        self.options.tx_fetch_max_retries = tx_fetch_max_retries;
        self
    }
    pub fn gas_adjustment(mut self, gas_adjustment: f32) -> BroadcasterBuilder<T, G> {
        self.options.gas_adjustment = gas_adjustment;
        self
    }
    pub fn gas_price(mut self, gas_price: (f64, Denom)) -> BroadcasterBuilder<T, G> {
        self.options.gas_price = gas_price;
        self
    }
}

impl<T: TmClient, G: GasEstimator> Broadcaster<T, G> {
    pub async fn broadcast<M>(&mut self, msgs: M) -> Result<BroadcastResponse, BroadcasterError>
    where
        M: IntoIterator<Item = cosmrs::Any> + Clone,
    {
        let tx_bytes = helpers::generate_sim_tx(msgs.clone(), self.acc_sequence, &self.priv_key.public_key())?;
        let estimated_gas = self
            .gas_estimator
            .estimate_gas(tx_bytes)
            .await
            .change_context(GasEstimationFailed)?;
        let mut gas_limit = estimated_gas;
        if self.options.gas_adjustment > 0.0 {
            gas_limit = (gas_limit as f64 * self.options.gas_adjustment as f64) as u64;
        }

        let (value, denom) = self.options.gas_price.clone();
        let amount = cosmrs::Coin {
            amount: (gas_limit as f64 * value).ceil() as u128,
            denom: denom,
        };

        let fee = Fee::from_amount_and_gas(amount, gas_limit);
        let tx_bytes = helpers::generate_tx(
            msgs,
            &self.priv_key,
            self.acc_number,
            self.acc_sequence,
            fee,
            &self.chain_id,
        )?;
        let response = self
            .tm_client
            .broadcast(tx_bytes)
            .await
            .change_context(BroadcastFailed)?;

        helpers::wait_for_block_inclusion(
            &self.tm_client,
            response.hash,
            self.options.tx_fetch_interval,
            self.options.tx_fetch_max_retries,
        )
        .await?;

        self.acc_sequence += 1;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use async_trait::async_trait;
    use error_stack::{Report, Result};
    use mockall::mock;

    use tendermint::abci::Code;
    use tendermint::block::Height;
    use tendermint::Hash;

    use crate::broadcaster::account_client::{GasEstimator, GasEstimatorError};

    use futures::Stream;

    use crate::broadcaster::{BroadcasterBuilder, BroadcasterError};
    use crate::tm_client::*;

    use cosmrs::bank::MsgSend;
    use cosmrs::crypto::secp256k1::SigningKey;
    use cosmrs::tx::Msg;
    use cosmrs::Coin;

    use tokio::test;

    #[test]
    async fn broadcast_successful() {
        let mut mock_tm_client = MockWebsocketClient::new();
        mock_tm_client.expect_broadcast().returning(|_| {
            Ok(BroadcastResponse {
                code: Code::Ok,
                log: String::from(""),
                data: "".into(),
                hash: Hash::None,
            })
        });

        mock_tm_client
            .expect_get_tx_height()
            .returning(|_, _| Ok(Height::from(1000000_u32)));

        let mut mock_gas_estimator = MockGasEstimator::new();

        mock_gas_estimator.expect_estimate_gas().returning(|_| Ok(1000));

        let priv_key = SigningKey::random();
        let account_id = priv_key.public_key().account_id("axelar").unwrap();

        let recipient_private_key = SigningKey::random();
        let recipient_account_id = recipient_private_key.public_key().account_id("axelar").unwrap();

        let amount = Coin {
            amount: 1u8.into(),
            denom: "uaxl".parse().unwrap(),
        };

        let msg_send = MsgSend {
            from_address: account_id,
            to_address: recipient_account_id,
            amount: vec![amount],
        }
        .to_any()
        .unwrap();

        let mut broadcaster = BroadcasterBuilder::new(mock_tm_client, mock_gas_estimator, priv_key).build();

        let response = broadcaster.broadcast(std::iter::once(msg_send)).await;

        assert!(response.is_ok())
    }

    #[test]
    async fn broadcast_failed() {
        let mut mock_tm_client = MockWebsocketClient::new();
        let mut mock_gas_estimator = MockGasEstimator::new();

        mock_tm_client
            .expect_broadcast()
            .returning(|_| Err(Report::new(TmClientError::client_internal("internal failure".into()))));

        mock_gas_estimator.expect_estimate_gas().returning(|_| Ok(1000));

        let priv_key = SigningKey::random();
        let account_id = priv_key.public_key().account_id("axelar").unwrap();

        let recipient_private_key = SigningKey::random();
        let recipient_account_id = recipient_private_key.public_key().account_id("axelar").unwrap();

        let amount = Coin {
            amount: 1u8.into(),
            denom: "uaxl".parse().unwrap(),
        };

        let msg_send = MsgSend {
            from_address: account_id,
            to_address: recipient_account_id,
            amount: vec![amount],
        }
        .to_any()
        .unwrap();

        let mut broadcaster = BroadcasterBuilder::new(mock_tm_client, mock_gas_estimator, priv_key).build();
        let response = broadcaster.broadcast(std::iter::once(msg_send)).await;

        assert!(matches!(
            response.unwrap_err().current_context(),
            BroadcasterError::BroadcastFailed
        ));
    }

    #[test]
    async fn broadcast_failed_seq_mismatch() {
        let mock_tm_client = MockWebsocketClient::new();
        let mut mock_gas_estimator = MockGasEstimator::new();

        mock_gas_estimator
            .expect_estimate_gas()
            .returning(|_| Err(Report::new(GasEstimatorError::AccountSequenceMismatch)));

        let priv_key = SigningKey::random();
        let account_id = priv_key.public_key().account_id("axelar").unwrap();

        let recipient_private_key = SigningKey::random();
        let recipient_account_id = recipient_private_key.public_key().account_id("axelar").unwrap();

        let amount = Coin {
            amount: 1u8.into(),
            denom: "uaxl".parse().unwrap(),
        };

        let msg_send = MsgSend {
            from_address: account_id,
            to_address: recipient_account_id,
            amount: vec![amount],
        }
        .to_any()
        .unwrap();

        let mut broadcaster = BroadcasterBuilder::new(mock_tm_client, mock_gas_estimator, priv_key).build();
        let response = broadcaster.broadcast(std::iter::once(msg_send)).await;

        assert!(matches!(
            response.unwrap_err().current_context(),
            BroadcasterError::GasEstimationFailed
        ));
    }

    #[test]
    async fn broadcast_failed_block_inclusion() {
        let mut mock_tm_client = MockWebsocketClient::new();
        mock_tm_client.expect_broadcast().returning(|_| {
            Ok(BroadcastResponse {
                code: Code::Ok,
                log: String::from(""),
                data: "".into(),
                hash: Hash::None,
            })
        });

        mock_tm_client
            .expect_get_tx_height()
            .returning(|_, _| Err(Report::new(TmClientError::client_internal("tx not found".into()))));

        let mut mock_gas_estimator = MockGasEstimator::new();

        mock_gas_estimator.expect_estimate_gas().returning(|_| Ok(1000));

        let priv_key = SigningKey::random();
        let account_id = priv_key.public_key().account_id("axelar").unwrap();

        let recipient_private_key = SigningKey::random();
        let recipient_account_id = recipient_private_key.public_key().account_id("axelar").unwrap();

        let amount = Coin {
            amount: 1u8.into(),
            denom: "uaxl".parse().unwrap(),
        };

        let msg_send = MsgSend {
            from_address: account_id,
            to_address: recipient_account_id,
            amount: vec![amount],
        }
        .to_any()
        .unwrap();

        let mut broadcaster = BroadcasterBuilder::new(mock_tm_client, mock_gas_estimator, priv_key).build();
        let response = broadcaster.broadcast(std::iter::once(msg_send)).await;

        assert!(matches!(
            response.unwrap_err().current_context(),
            BroadcasterError::BlockInclusionTimeout
        ));
    }

    mock! {
        Subscription{}

        impl Stream for Subscription {
            type Item = core::result::Result<Event, TmClientError>;

            fn poll_next<'a>(self: Pin<&mut Self>, cx: &mut Context<'a>) -> Poll<Option<<Self as Stream>::Item>>;
        }
    }

    mock! {
        WebsocketClient{}

        #[async_trait]
        impl TmClient for WebsocketClient{
            type Sub = MockSubscription;

            async fn subscribe(&self, query: Query) -> Result<<Self as TmClient>::Sub, TmClientError>;
            async fn block_results(&self, block_height: Height) -> Result<BlockResponse, TmClientError>;
            async fn broadcast(&self, tx_raw: Vec<u8>) -> Result<BroadcastResponse, TmClientError>;
            async fn get_tx_height(&self, tx_hash: Hash, prove: bool) -> Result<Height,TmClientError>;
            fn close(self) -> Result<(), TmClientError>;
        }
    }

    mock! {
        GasEstimator{}

        #[async_trait]
        impl GasEstimator for GasEstimator {
            async fn estimate_gas(&self, tx_bytes: Vec<u8>) -> Result<u64,GasEstimatorError>;
        }
    }
}
