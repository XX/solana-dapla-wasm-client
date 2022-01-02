pub use solana_client_api::*;

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
    time::{Duration, Instant},
};

use laplace_wasm::http;
use serde::Deserialize;
use serde_json::Value;
use solana_client_api::{
    client_error::{ClientError, ClientErrorKind, Result},
    rpc_custom_error,
    rpc_request::{RpcError, RpcRequest, RpcResponseErrorData},
    rpc_response::RpcSimulateTransactionResult,
    rpc_sender::{RpcSender, RpcTransportStats},
};

pub mod wasm_rpc_client;

pub struct HttpSender {
    url: String,
    request_id: AtomicU64,
    stats: RwLock<RpcTransportStats>,
}

impl HttpSender {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            request_id: AtomicU64::new(0),
            stats: RwLock::new(RpcTransportStats::default()),
        }
    }
}

impl RpcSender for HttpSender {
    fn send(&self, request: RpcRequest, params: Value) -> Result<Value> {
        let mut stats_updater = StatsUpdater::new(&self.stats);

        let request_id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let request_json = request.build_request_json(request_id, params).to_string();
        let mut too_many_requests_retries = 5;

        loop {
            let http_request = http::RequestBuilder::new()
                .method(http::Method::POST)
                .uri(&self.url)
                .header(http::types::header::CONTENT_TYPE, "application/json")
                .body(request_json.clone().into_bytes())
                .map_err(|err| ClientError::new_with_request(ClientErrorKind::Custom(format!("{:?}", err)), request))?
                .into();
            let http_response = http::invoke(http_request)
                .map_err(|err| ClientError::new_with_request(ClientErrorKind::Custom(format!("{:?}", err)), request))?;

            if !http_response.status.is_success() {
                if http_response.status == http::StatusCode::TOO_MANY_REQUESTS && too_many_requests_retries > 0 {
                    let mut duration = Duration::from_millis(500);
                    if let Some(retry_after) = http_response.headers.get(http::types::header::RETRY_AFTER) {
                        if let Ok(retry_after) = retry_after.to_str() {
                            if let Ok(retry_after) = retry_after.parse::<u64>() {
                                if retry_after < 120 {
                                    duration = Duration::from_secs(retry_after);
                                }
                            }
                        }
                    }

                    too_many_requests_retries -= 1;

                    #[cfg(feature = "laplace_sleep")]
                    laplace_wasm::sleep::invoke(duration.as_millis() as u64);

                    #[cfg(not(feature = "laplace_sleep"))]
                    std::thread::sleep(duration);

                    stats_updater.add_rate_limited_time(duration);
                    continue;
                }
                return Err(ClientError::new_with_request(
                    ClientErrorKind::RpcError(RpcError::ForUser(format!("{}", http_response.status))),
                    request,
                ));
            }

            let mut json: Value = serde_json::from_slice(&http_response.body)?;
            if json["error"].is_object() {
                return match serde_json::from_value::<RpcErrorObject>(json["error"].clone()) {
                    Ok(rpc_error_object) => {
                        let data = match rpc_error_object.code {
                            rpc_custom_error::JSON_RPC_SERVER_ERROR_SEND_TRANSACTION_PREFLIGHT_FAILURE => {
                                match serde_json::from_value::<RpcSimulateTransactionResult>(
                                    json["error"]["data"].clone(),
                                ) {
                                    Ok(data) => RpcResponseErrorData::SendTransactionPreflightFailure(data),
                                    Err(_) => RpcResponseErrorData::Empty,
                                }
                            },
                            rpc_custom_error::JSON_RPC_SERVER_ERROR_NODE_UNHEALTHY => {
                                match serde_json::from_value::<rpc_custom_error::NodeUnhealthyErrorData>(
                                    json["error"]["data"].clone(),
                                ) {
                                    Ok(rpc_custom_error::NodeUnhealthyErrorData { num_slots_behind }) => {
                                        RpcResponseErrorData::NodeUnhealthy { num_slots_behind }
                                    },
                                    Err(_err) => RpcResponseErrorData::Empty,
                                }
                            },
                            _ => RpcResponseErrorData::Empty,
                        };

                        Err(RpcError::RpcResponseError {
                            code: rpc_error_object.code,
                            message: rpc_error_object.message,
                            data,
                        }
                        .into())
                    },
                    Err(err) => Err(RpcError::RpcRequestError(format!(
                        "Failed to deserialize RPC error response: {} [{}]",
                        serde_json::to_string(&json["error"]).unwrap(),
                        err
                    ))
                    .into()),
                };
            }
            return Ok(json["result"].take());
        }
    }

    fn get_transport_stats(&self) -> RpcTransportStats {
        self.stats.read().unwrap().clone()
    }
}

struct StatsUpdater<'a> {
    stats: &'a RwLock<RpcTransportStats>,
    request_start_time: Instant,
    rate_limited_time: Duration,
}

impl<'a> StatsUpdater<'a> {
    fn new(stats: &'a RwLock<RpcTransportStats>) -> Self {
        Self {
            stats,
            request_start_time: Instant::now(),
            rate_limited_time: Duration::default(),
        }
    }

    fn add_rate_limited_time(&mut self, duration: Duration) {
        self.rate_limited_time += duration;
    }
}

impl<'a> Drop for StatsUpdater<'a> {
    fn drop(&mut self) {
        let mut stats = self.stats.write().unwrap();
        stats.request_count += 1;
        stats.elapsed_time += Instant::now().duration_since(self.request_start_time);
        stats.rate_limited_time += self.rate_limited_time;
    }
}

#[derive(Deserialize, Debug)]
struct RpcErrorObject {
    code: i64,
    message: String,
}
