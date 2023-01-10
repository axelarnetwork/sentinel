use async_trait::async_trait;
use error_stack::{Report, Result};
use futures::TryFutureExt;

use tendermint::block::Height;
use tendermint::Hash;
use tendermint_rpc::{Client, Subscription, SubscriptionClient, WebSocketClient};

use tokio_stream::Stream;

pub type BlockResponse = tendermint_rpc::endpoint::block_results::Response;
pub type BroadcastResponse = tendermint_rpc::endpoint::broadcast::tx_sync::Response;

pub type TmClientError = tendermint_rpc::Error;
pub type Query = tendermint_rpc::query::Query;
pub type Event = tendermint_rpc::event::Event;
pub type EventData = tendermint_rpc::event::EventData;
pub type EventType = tendermint_rpc::query::EventType;

#[async_trait]
pub trait TmClient {
    type Sub: Stream<Item = core::result::Result<Event, TmClientError>> + Unpin;

    async fn subscribe(&self, query: Query) -> Result<Self::Sub, TmClientError>;
    async fn block_results(&self, block_height: Height) -> Result<BlockResponse, TmClientError>;
    async fn broadcast(&self, tx_raw: Vec<u8>) -> Result<BroadcastResponse, TmClientError>;
    async fn get_tx_height(&self, tx_hash: Hash, prove: bool) -> Result<Height, TmClientError>;
    fn close(self) -> Result<(), TmClientError>;
}

#[async_trait]
impl TmClient for WebSocketClient {
    type Sub = Subscription;

    async fn subscribe(&self, query: Query) -> Result<Self::Sub, TmClientError> {
        SubscriptionClient::subscribe(self, query).map_err(Report::new).await
    }

    async fn block_results(&self, block_height: Height) -> Result<BlockResponse, TmClientError> {
        Client::block_results(self, block_height).map_err(Report::new).await
    }
    async fn broadcast(&self, tx_raw: Vec<u8>) -> Result<BroadcastResponse, TmClientError> {
        Client::broadcast_tx_sync(self, tx_raw).map_err(Report::new).await
    }
    fn close(self) -> Result<(), TmClientError> {
        SubscriptionClient::close(self).map_err(Report::new)
    }
    async fn get_tx_height(&self, tx_hash: Hash, prove: bool) -> Result<Height, TmClientError> {
        Ok(Client::tx(self, tx_hash, prove).map_err(Report::new).await?.height)
    }
}
