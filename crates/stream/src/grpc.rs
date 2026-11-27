use std::{collections::HashMap, future::Future, pin::Pin, task::Poll};

use futures_util::StreamExt;
use shipstern::{stop, Handler, HandlerResult};
use shipstern_core::Pubkey;
use shipstern_proto::{
    prost::{Message, Name},
    prost_types::Any,
    stream::{
        self,
        program_streams_server::{ProgramStreams, ProgramStreamsServer},
        SubscribeManyRequest, SubscribeRequest, SubscribeUpdate,
    },
    tonic::{self, transport, Request, Response, Status},
    tonic_reflection,
};
use tokio::{
    sync::broadcast,
    task::{JoinError, JoinHandle},
};
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};

use super::config::GrpcConfig;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("gRPC transport error")]
    Transport(#[from] transport::Error),
    #[error("Server task panicked")]
    Panic(#[from] JoinError),
}

#[derive(Debug)]
pub struct GrpcHandler(pub(super) broadcast::Sender<Any>);

impl<T: Message + Name + Sync, R: Sync> Handler<T, R> for GrpcHandler {
    async fn handle(&self, value: &T, _raw: &R) -> HandlerResult<()> {
        self.0.send(Any::from_msg(value)?).ok();
        Ok(())
    }
}

pub type Receiver = broadcast::Receiver<Any>;
pub type Channels<V = Box<[Receiver]>> = HashMap<Pubkey, V>;

pub(super) struct Service(Channels);

#[tonic::async_trait]
impl ProgramStreams for Service {
    type SubscribeManyStream = futures_util::stream::SelectAll<ReceiverStream>;
    type SubscribeStream = futures_util::stream::SelectAll<ReceiverStream>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let pubkey: Pubkey = request.into_inner().program.parse().map_err(
            |e: shipstern_core::KeyFromStrError| {
                Status::new(tonic::Code::InvalidArgument, e.to_string())
            },
        )?;

        static NO_RX: [Receiver; 0] = [];
        let rxs = self.0.get(&pubkey).map_or(NO_RX.as_slice(), AsRef::as_ref);

        let stream = futures_util::stream::select_all(rxs.iter().map(new_receiver_stream));

        Ok(Response::new(stream))
    }

    async fn subscribe_many(
        &self,
        request: Request<SubscribeManyRequest>,
    ) -> Result<Response<Self::SubscribeManyStream>, Status> {
        let pubkeys: Vec<Pubkey> = request
            .into_inner()
            .programs
            .iter()
            .map(|p| {
                p.parse().map_err(|e: shipstern_core::KeyFromStrError| {
                    Status::new(tonic::Code::InvalidArgument, e.to_string())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let rxs: Vec<_> = pubkeys
            .iter()
            .filter_map(|key| self.0.get(key))
            .flatten()
            .collect();

        let stream = futures_util::stream::select_all(rxs.iter().map(|rx| new_receiver_stream(rx)));

        Ok(Response::new(stream))
    }
}

pub type ReceiverStream =
    futures_util::stream::Map<BroadcastStream<Any>, fn(RecvItem) -> UpdateItem>;

type RecvItem = Result<Any, BroadcastStreamRecvError>;
type UpdateItem = Result<SubscribeUpdate, Status>;

fn new_receiver_stream(rx: &Receiver) -> ReceiverStream {
    BroadcastStream::new(rx.resubscribe()).map(into_update)
}

// A lagged receiver has already dropped messages. Ending the response with an
// error tells the client about the gap so it can resubscribe, instead of
// silently continuing with missing data.
fn into_update(item: RecvItem) -> UpdateItem {
    match item {
        Ok(parsed) => Ok(SubscribeUpdate {
            parsed: Some(parsed),
        }),
        Err(BroadcastStreamRecvError::Lagged(skipped)) => Err(Status::data_loss(format!(
            "client lagged behind and missed {skipped} updates"
        ))),
    }
}

#[derive(Debug)]
pub struct Server(stop::StopTx, JoinHandle<Result<(), transport::Error>>);

#[inline]
fn flatten_error<T>(res: Result<Result<T, transport::Error>, JoinError>) -> Result<T, Error> {
    Ok(res??)
}

impl Future for Server {
    type Output = Result<(), Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.1).poll(cx).map(flatten_error)
    }
}

impl Server {
    pub fn run(config: GrpcConfig, desc_sets: &[&[u8]], channels: Channels) -> Self {
        let GrpcConfig { address } = config;

        let mut reflection = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(stream::DESCRIPTOR_SET);

        for desc in desc_sets {
            reflection = reflection.register_encoded_file_descriptor_set(desc);
        }

        let reflection = reflection.build_v1alpha().unwrap();

        let (stop, rx) = stop::channel();
        Self(
            stop,
            tokio::task::spawn(async move {
                transport::Server::builder()
                    .add_service(reflection)
                    .add_service(ProgramStreamsServer::new(Service(channels)))
                    .serve_with_shutdown(address, rx.as_unit())
                    .await
            }),
        )
    }

    pub async fn stop(self) -> Result<(), Error> {
        self.0.maybe_send();
        flatten_error(self.1.await)
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;
    use shipstern_proto::prost_types::Any;

    use super::*;

    const PROGRAM: &str = "11111111111111111111111111111111";

    fn msg(n: u8) -> Any {
        Any {
            type_url: "test".into(),
            value: vec![n],
        }
    }

    #[tokio::test]
    async fn lagged_client_gets_data_loss_instead_of_panicking() {
        let (tx, rx) = broadcast::channel(2);
        let channels = Channels::from([(PROGRAM.parse().unwrap(), Box::from([rx]))]);

        let mut stream = Service(channels)
            .subscribe(Request::new(SubscribeRequest {
                program: PROGRAM.into(),
            }))
            .await
            .unwrap()
            .into_inner();

        for n in 0..5 {
            tx.send(msg(n)).unwrap();
        }

        let err = stream.next().await.unwrap().unwrap_err();
        assert_eq!(err.code(), tonic::Code::DataLoss);
    }
}
