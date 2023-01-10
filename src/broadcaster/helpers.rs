use std::thread;

use cosmos_sdk_proto::cosmos::tx::v1beta1::TxRaw;

use cosmrs::crypto::{secp256k1::SigningKey, PublicKey};
use cosmrs::tx::{BodyBuilder, Fee, Raw, SignDoc, SignerInfo};
use cosmrs::Coin;
use error_stack::{IntoReportCompat, Report, Result, ResultExt};
use tendermint::chain::Id;

use crate::tm_client::TmClient;

use tendermint::Hash;

use crate::broadcaster::{BroadcasterError, BroadcasterError::*};

pub fn generate_sim_tx<M>(msgs: M, sequence: u64, pub_key: &PublicKey) -> Result<Vec<u8>, BroadcasterError>
where
    M: IntoIterator<Item = cosmrs::Any>,
{
    let body = BodyBuilder::new().msgs(msgs).finish();
    let fee = Fee::from_amount_and_gas(Coin::new(0u8.into(), "").unwrap(), 0u64);
    let auth_info = SignerInfo::single_direct(Some(pub_key.clone()), sequence).auth_info(fee);
    let body_bytes = body
        .clone()
        .into_bytes()
        .into_report()
        .change_context(TxMarshalingFailed)?;
    let auth_info_bytes = auth_info
        .clone()
        .into_bytes()
        .into_report()
        .change_context(TxMarshalingFailed)?;

    let raw: Raw = TxRaw {
        body_bytes,
        auth_info_bytes,
        signatures: vec![[].to_vec()],
    }
    .into();

    raw.to_bytes().into_report().change_context(TxMarshalingFailed)
}

pub fn generate_tx<M>(
    msgs: M,
    priv_key: &SigningKey,
    account_number: u64,
    sequence: u64,
    fee: Fee,
    chain_id: &Id,
) -> Result<Vec<u8>, BroadcasterError>
where
    M: IntoIterator<Item = cosmrs::Any>,
{
    let pub_key = priv_key.public_key();
    let auth_info = SignerInfo::single_direct(Some(pub_key), sequence).auth_info(fee);
    let body = BodyBuilder::new().msgs(msgs).finish();

    SignDoc::new(&body, &auth_info, &chain_id, account_number)
        .and_then(|sign_doc| sign_doc.sign(&priv_key))
        .and_then(|tx| tx.to_bytes())
        .into_report()
        .change_context(TxMarshalingFailed)
}

pub async fn wait_for_block_inclusion<C>(
    tm_client: &C,
    tx_hash: Hash,
    fetch_interval: std::time::Duration,
    max_retries: u32,
) -> Result<(), BroadcasterError>
where
    C: TmClient,
{
    let mut last_error = Report::new(BlockInclusionTimeout);

    for _ in 0..max_retries {
        thread::sleep(fetch_interval);
        match tm_client.get_tx_height(tx_hash, true).await {
            Ok(_) => return Ok(()),
            Err(err) => {
                last_error = err.change_context(BlockInclusionTimeout);
                continue;
            }
        }
    }

    Err(last_error)
}

#[cfg(test)]
mod tests {

    use tendermint::chain::Id;

    use cosmrs::bank::MsgSend;
    use cosmrs::crypto::secp256k1::SigningKey;
    use cosmrs::tx::AccountNumber;
    use cosmrs::tx::{Fee, Msg};
    use cosmrs::Coin;
    use std::iter;
    use std::str;

    use super::{generate_sim_tx, generate_tx};

    const CHAIN_ID: &str = "axelar-unit-test";
    const ACC_NUMBER: AccountNumber = 1;
    const ACC_PREFIX: &str = "axelar";
    const DENOM: &str = "uaxl";

    #[test]
    fn marshal_sim_tx_success() {
        let priv_key = SigningKey::random();
        let account_id = priv_key.public_key().account_id(ACC_PREFIX).unwrap();

        let recipient_private_key = SigningKey::random();
        let recipient_account_id = recipient_private_key.public_key().account_id(ACC_PREFIX).unwrap();

        let amount = Coin {
            amount: 1u8.into(),
            denom: DENOM.parse().unwrap(),
        };

        let msg_send = MsgSend {
            from_address: account_id.clone(),
            to_address: recipient_account_id,
            amount: vec![amount.clone()],
        }
        .to_any()
        .unwrap();

        let res = generate_sim_tx(iter::once(msg_send), 0, &priv_key.public_key());

        assert!(res.is_ok());
    }

    #[test]
    fn marshal_tx_success() {
        let priv_key = SigningKey::random();
        let account_id = priv_key.public_key().account_id(ACC_PREFIX).unwrap();

        let recipient_private_key = SigningKey::random();
        let recipient_account_id = recipient_private_key.public_key().account_id(ACC_PREFIX).unwrap();

        let amount = Coin {
            amount: 1u8.into(),
            denom: DENOM.parse().unwrap(),
        };

        let msg_send = MsgSend {
            from_address: account_id.clone(),
            to_address: recipient_account_id,
            amount: vec![amount.clone()],
        }
        .to_any()
        .unwrap();

        let chain_id: Id = CHAIN_ID.parse().unwrap();
        let seq_number = 0;
        let gas = 100_000u64;
        let fee = Fee::from_amount_and_gas(amount, gas);

        let res = generate_tx(iter::once(msg_send), &priv_key, ACC_NUMBER, seq_number, fee, &chain_id);

        assert!(res.is_ok());
    }
}
