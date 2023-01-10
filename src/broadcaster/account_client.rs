use async_trait::async_trait;

use cosmos_sdk_proto::cosmos::auth::v1beta1::query_client::QueryClient as AuthQueryClient;
use cosmos_sdk_proto::cosmos::auth::v1beta1::{BaseAccount, QueryAccountRequest};
use cosmos_sdk_proto::traits::Message;
use error_stack::{IntoReport, Result, ResultExt};
use thiserror::Error;

use cosmos_sdk_proto::cosmos::tx::v1beta1::{service_client::ServiceClient, SimulateRequest};

const SEQ_MISMATCH_CODE: i32 = 32;

pub trait AccountInfo {
    fn address(&self) -> String;
    fn sequence(&self) -> u64;
    fn account_number(&self) -> u64;
}

#[derive(Error, Debug)]
pub enum GasEstimatorError {
    #[error("failed to connect to node")]
    ConnectionFailed,
    #[error("remote call failed")]
    RemoteCallFailed,
    #[error("account sequence mismatch during simulation")]
    AccountSequenceMismatch,
    #[error("tx simulation failed")]
    TxSimulationFailed,
}

#[async_trait]
pub trait GasEstimator {
    async fn estimate_gas(&self, tx_bytes: Vec<u8>) -> Result<u64, GasEstimatorError>;
}

#[derive(Error, Debug)]
pub enum AccountClientError {
    #[error("failed to connect to node")]
    ConnectionFailed,
    #[error("remote call failed")]
    RemoteCallFailed,
    #[error("failed to unmarshal protobuf")]
    UnmarshalingFailed,
}

#[async_trait]
pub trait AccountClient: AccountInfo + GasEstimator {
    async fn synch(&mut self) -> Result<(), AccountClientError>;
}

#[derive(Debug, Clone)]
pub struct GrpcAccountClient {
    grpc_url: String,
    address: String,
    account_info: Option<BaseAccount>,
}

impl GrpcAccountClient {
    pub fn new(address: String, grpc_url: String) -> Self {
        GrpcAccountClient {
            grpc_url: grpc_url,
            address: address,
            account_info: None,
        }
    }
}

impl AccountInfo for GrpcAccountClient {
    fn address(&self) -> String {
        self.address.clone()
    }

    fn sequence(&self) -> u64 {
        self.account_info.as_ref().map_or(0, |info| info.sequence)
    }

    fn account_number(&self) -> u64 {
        self.account_info.as_ref().map_or(0, |info| info.account_number)
    }
}

#[async_trait]
impl GasEstimator for GrpcAccountClient {
    async fn estimate_gas(&self, tx_bytes: Vec<u8>) -> Result<u64, GasEstimatorError> {
        let request = SimulateRequest { tx: None, tx_bytes };

        let mut client = ServiceClient::connect(self.grpc_url.clone())
            .await
            .into_report()
            .change_context(GasEstimatorError::ConnectionFailed)?;

        match client.simulate(request).await {
            Ok(response) => response
                .into_inner()
                .gas_info
                .ok_or(GasEstimatorError::TxSimulationFailed)
                .map(|info| info.gas_used)
                .into_report(),
            Err(err) => {
                if err.code() == SEQ_MISMATCH_CODE.into() {
                    return Err(err)
                        .into_report()
                        .change_context(GasEstimatorError::AccountSequenceMismatch);
                }
                Err(err)
                    .into_report()
                    .change_context(GasEstimatorError::TxSimulationFailed)
            }
        }
    }
}
#[async_trait]
impl AccountClient for GrpcAccountClient {
    async fn synch(&mut self) -> Result<(), AccountClientError> {
        if self.account_info.is_some() {
            return Ok(());
        }

        let request = QueryAccountRequest {
            address: self.address.clone(),
        };

        let mut client = AuthQueryClient::connect(self.grpc_url.clone())
            .await
            .into_report()
            .change_context(AccountClientError::ConnectionFailed)?;

        let response = client
            .account(request)
            .await
            .into_report()
            .change_context(AccountClientError::RemoteCallFailed)?;

        let account = response
            .into_inner()
            .account
            .ok_or(AccountClientError::UnmarshalingFailed)
            .into_report()
            .and_then(|account| {
                BaseAccount::decode(&account.value[..])
                    .into_report()
                    .change_context(AccountClientError::UnmarshalingFailed)
            })?;

        self.account_info = Some(account);
        Ok(())
    }
}
