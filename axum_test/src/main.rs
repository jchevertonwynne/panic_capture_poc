use std::{
    collections::HashMap,
    future::Future,
    net::SocketAddr,
    panic::PanicInfo,
    task::ready,
    time::Duration,
};

use axum::{extract::Query, Json, Router};
use futures::FutureExt;
use http::StatusCode;
use pin_project::pin_project;
use serde::{Deserialize, Serialize};
use tokio::{
    select,
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, Level};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// to make a request panic send
// curl 'localhost:25565?numerator=10&denominator=0'

// and main exits with a panic upon ctrl-c

#[panic_capture_lib::capture_panics]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::Registry::default()
        .with(tracing_subscriber::filter::LevelFilter::from_level(
            Level::INFO,
        ))
        .with(tracing_subscriber::fmt::layer())
        .try_init()?;

    let cancel_token = CancellationToken::new();

    let panic_watcher_handle = install_panic_handler({
        let cancel_token = cancel_token.clone();
        async move { cancel_token.cancelled().await }
    });

    info!("hello!");

    let router = Router::new()
        .route("/", axum::routing::get(my_route))
        .layer(StatusCounterLayer::new())
        .layer(PanicCatchLayer::default());

    let addr = SocketAddr::from(([0, 0, 0, 0], 25565));

    info!("serving on {addr}");

    axum::Server::from_tcp(std::net::TcpListener::bind(addr)?)?
        .serve(router.into_make_service())
        .with_graceful_shutdown(tokio::signal::ctrl_c().map(|_| ()))
        .await?;

    info!("starting shutdown");

    cancel_token.cancel();

    panic_watcher_handle.await?;

    info!("goodbye!");

    panic!("oh no :(");
}

fn install_panic_handler(cancel: impl Future<Output = ()> + Send + 'static) -> JoinHandle<()> {
    let (tx, mut rx) = unbounded_channel();

    let default_hook = std::panic::take_hook();
    let handle = tokio::spawn(async move {
        let mut traces = vec![];
        let mut cancel = std::pin::pin!(cancel);
        loop {
            select! {
                Some(bt) = rx.recv() => {
                    traces.push(bt);
                    info!("there are now {} backtraces collected", traces.len());
                }
                _ = cancel.as_mut() => {
                    info!("shutting down panic handler");
                    std::panic::set_hook(default_hook);
                    break
                }
            }
        }
    });

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info: &PanicInfo| {
        let bt = backtrace::Backtrace::new();
        tx.send(bt).expect("backtrace receiver was shut down");
        default_hook(info)
    }));

    handle
}

#[derive(Debug, Serialize, Deserialize)]
struct Params {
    numerator: isize,
    denominator: isize,
}

#[derive(Debug, Serialize, Deserialize)]
struct Response {
    result: isize,
}

async fn my_route(
    Query(Params {
        numerator,
        denominator,
    }): Query<Params>,
) -> Json<Response> {
    Json(Response {
        result: numerator / denominator,
    })
}

#[derive(Debug, Clone, Default)]
struct PanicCatchLayer {}

impl<S> tower::Layer<S> for PanicCatchLayer {
    type Service = PanicCatchService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PanicCatchService::new(inner)
    }
}

#[derive(Debug, Clone)]
struct PanicCatchService<S> {
    inner: S,
}

impl<S> PanicCatchService<S> {
    fn new(inner: S) -> PanicCatchService<S> {
        PanicCatchService { inner }
    }
}

impl<S, R> tower::Service<R> for PanicCatchService<S>
where
    S: tower::Service<R>,
{
    type Response = S::Response;

    type Error = S::Error;

    type Future = PanicWatchFuture<S::Future>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: R) -> Self::Future {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.inner.call(req))) {
            Ok(f) => PanicWatchFuture::new(f),
            Err(err) => {
                let panics = panic_capture_lib::increment_counter();
                error!("this is from my tower layer panic capture. total panics = {panics}");
                std::panic::resume_unwind(err)
            }
        }
    }
}

#[pin_project]
struct PanicWatchFuture<F> {
    #[pin]
    fut: F,
}

impl<F> PanicWatchFuture<F> {
    fn new(fut: F) -> Self {
        Self { fut }
    }
}

impl<F> Future for PanicWatchFuture<F>
where
    F: Future,
{
    type Output = F::Output;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| this.fut.poll(cx))) {
            Ok(polled) => polled,
            Err(err) => {
                let panics = panic_capture_lib::increment_counter();
                error!("this is from my tower service panic capture. total panics = {panics}");
                std::panic::resume_unwind(err)
            }
        }
    }
}

#[derive(Debug, Clone)]
struct StatusCounterLayer {
    sender: UnboundedSender<StatusCode>,
}

impl StatusCounterLayer {
    fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        tokio::spawn(status_loop(rx));
        Self { sender: tx }
    }
}

async fn status_loop(mut rx: UnboundedReceiver<StatusCode>) {
    let mut codes = HashMap::<StatusCode, usize>::default();
    let mut interval = tokio::time::interval(Duration::from_secs(5));

    loop {
        select! {
            channel_recv = rx.recv() => {
                let Some(status_code) = channel_recv else {
                    info!("ending status loop!");
                    break;
                };
                *codes.entry(status_code).or_default() += 1;
            }
            _ = interval.tick() => {
                info!("status codes = {codes:?}");
            }
        }
    }
}

impl<S> tower::Layer<S> for StatusCounterLayer {
    type Service = StatusCounterService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        StatusCounterService::new(inner, self.sender.clone())
    }
}

#[derive(Debug, Clone)]
struct StatusCounterService<S> {
    inner: S,
    sender: UnboundedSender<StatusCode>,
}

impl<S> StatusCounterService<S> {
    fn new(inner: S, sender: UnboundedSender<StatusCode>) -> Self {
        Self { inner, sender }
    }
}

impl<S, Req, Res> tower::Service<Req> for StatusCounterService<S>
where
    S: tower::Service<Req, Response = http::Response<Res>>,
{
    type Response = S::Response;

    type Error = S::Error;

    type Future = StatusCounterFut<S::Future>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let fut = self.inner.call(req);
        let sender = self.sender.clone();
        StatusCounterFut { fut, sender }
    }
}

#[pin_project]
struct StatusCounterFut<F> {
    #[pin]
    fut: F,
    sender: UnboundedSender<StatusCode>,
}

impl<F, Res, Err> Future for StatusCounterFut<F>
where
    F: Future<Output = Result<http::Response<Res>, Err>>,
{
    type Output = F::Output;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        let rdy = ready!(this.fut.poll(cx));

        if let Ok(resp) = rdy.as_ref() {
            this.sender
                .send(resp.status())
                .expect("receiver should always be alive");
        }

        std::task::Poll::Ready(rdy)
    }
}
