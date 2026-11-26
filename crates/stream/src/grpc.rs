use std::{collections::HashMap, future::Future, mem, pin::Pin, task::Poll};

use futures_util::pin_mut;
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

        // TODO: make max_tries configurable?
        let stream =
            futures_util::stream::select_all(rxs.iter().map(|rx| ReceiverStream::new(rx, 8)));

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

        // A program listed twice must still send each update only once.
        let mut seen = std::collections::HashSet::new();
        let rxs: Vec<_> = pubkeys
            .iter()
            .filter(|key| seen.insert(*key))
            .filter_map(|key| self.0.get(key))
            .flatten()
            .collect();

        // TODO: make max_tries configurable?
        let stream =
            futures_util::stream::select_all(rxs.iter().map(|rx| ReceiverStream::new(rx, 8)));

        Ok(Response::new(stream))
    }
}

type BoxedRx = Box<Receiver>;
type RecvResult = (BoxedRx, Result<Any, broadcast::error::RecvError>);
enum RecvState {
    Unpolled(BoxedRx),
    Poison,
    Polled(Pin<Box<dyn Future<Output = RecvResult> + Send>>),
}

pin_project_lite::pin_project! {
    pub struct ReceiverStream {
        recv: RecvState,
        tries: u8,
        max_tries: u8,
    }
}

impl ReceiverStream {
    fn new(rx: &Receiver, max_tries: u8) -> Self {
        Self {
            recv: RecvState::Unpolled(rx.resubscribe().into()),
            tries: max_tries,
            max_tries,
        }
    }
}

impl futures_util::Stream for ReceiverStream {
    type Item = Result<SubscribeUpdate, tonic::Status>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let me = self.project();

        loop {
            match me.recv {
                RecvState::Unpolled(_) => {
                    let RecvState::Unpolled(mut rx) = mem::replace(me.recv, RecvState::Poison)
                    else {
                        unreachable!();
                    };

                    *me.recv = RecvState::Polled(Box::pin(async move {
                        let res = rx.recv().await;
                        (rx, res)
                    }));
                },
                RecvState::Poison => panic!("RecvState was poisoned!"),
                RecvState::Polled(_) => (),
            }

            let RecvState::Polled(fut) = me.recv else {
                unreachable!()
            };

            pin_mut!(fut);
            let (rx, res) = match fut.poll(cx) {
                Poll::Pending => break Poll::Pending,
                Poll::Ready(r) => r,
            };

            *me.recv = RecvState::Unpolled(rx);

            break Poll::Ready(match res {
                Ok(m) => {
                    *me.tries = *me.max_tries;
                    Some(Ok(SubscribeUpdate { parsed: Some(m) }))
                },
                Err(broadcast::error::RecvError::Closed) => None,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    *me.tries = me
                        .tries
                        .checked_sub(1)
                        .unwrap_or_else(|| panic!("gRPC channel lagged too many times!"));

                    continue;
                },
            });
        }
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

    use super::*;

    const PROGRAM: &str = "11111111111111111111111111111111";

    #[tokio::test]
    async fn subscribe_many_sends_each_update_once_for_a_repeated_program() {
        let (tx, rx) = broadcast::channel(4);
        let channels = Channels::from([(PROGRAM.parse().unwrap(), Box::from([rx]))]);

        let stream = Service(channels)
            .subscribe_many(Request::new(SubscribeManyRequest {
                programs: vec![PROGRAM.into(), PROGRAM.into()],
            }))
            .await
            .unwrap()
            .into_inner();

        tx.send(Any {
            type_url: "test".into(),
            value: vec![1],
        })
        .unwrap();
        drop(tx);

        let updates: Vec<_> = stream.collect().await;
        assert_eq!(updates.len(), 1);
    }
}
