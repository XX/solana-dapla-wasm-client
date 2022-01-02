use std::ops::{Deref, DerefMut};

use solana_client_api::{client_error::Result as ClientResult, rpc_client::RpcClient, rpc_request::RpcError};
use solana_sdk::{
    commitment_config::CommitmentConfig,
    signature::Signature,
    transaction::{uses_durable_nonce, Transaction},
};

pub struct WasmRpcClient(RpcClient);

impl WasmRpcClient {
    pub fn new(client: RpcClient) -> Self {
        Self(client)
    }

    pub fn send_and_confirm_transaction(&self, transaction: &Transaction) -> ClientResult<Signature> {
        const SEND_RETRIES: usize = 1;
        const GET_STATUS_RETRIES: usize = usize::MAX;

        'sending: for _ in 0..SEND_RETRIES {
            let signature = self.send_transaction(transaction)?;

            let recent_blockhash = if uses_durable_nonce(transaction).is_some() {
                let (recent_blockhash, ..) = self.get_latest_blockhash_with_commitment(CommitmentConfig::processed())?;
                recent_blockhash
            } else {
                transaction.message.recent_blockhash
            };

            for status_retry in 0..GET_STATUS_RETRIES {
                match self.get_signature_status(&signature)? {
                    Some(Ok(_)) => return Ok(signature),
                    Some(Err(e)) => return Err(e.into()),
                    None => {
                        if !self.is_blockhash_valid(&recent_blockhash, CommitmentConfig::processed())? {
                            // Block hash is not found by some reason
                            break 'sending;
                        } else if cfg!(not(test))
                            // Ignore sleep at last step.
                            && status_retry < GET_STATUS_RETRIES
                        {
                            // Retry twice a second
                            let delay_millis = 500;

                            #[cfg(feature = "laplace_sleep")]
                            laplace_wasm::sleep::invoke(delay_millis);

                            #[cfg(not(feature = "laplace_sleep"))]
                            std::thread::sleep(std::time::Duration::from_millis(delay_millis));

                            continue;
                        }
                    },
                }
            }
        }

        Err(RpcError::ForUser(
            "unable to confirm transaction. \
             This can happen in situations such as transaction expiration \
             and insufficient fee-payer funds"
                .to_string(),
        )
        .into())
    }
}

impl Deref for WasmRpcClient {
    type Target = RpcClient;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for WasmRpcClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
