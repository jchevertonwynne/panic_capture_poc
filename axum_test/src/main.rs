use std::net::SocketAddr;

use axum::{extract::Query, Json, Router};
use futures::FutureExt;
use pin_project::pin_project;
use serde::{Deserialize, Serialize};
use tracing::{error, info, Level};
use tracing_subscriber::{prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt};

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

    info!("hello!");

    let router = Router::new().route("/", axum::routing::get(my_route));
    // .layer(PanicCatchLayer::default());

    let addr = SocketAddr::from(([0, 0, 0, 0], 25565));

    info!("serving on {addr}");

    let shutdown = tokio::signal::ctrl_c().map(|_| ());
    axum::Server::from_tcp(std::net::TcpListener::bind(addr)?)?
        .serve(router.into_make_service())
        .with_graceful_shutdown(shutdown)
        .await?;

    info!("goodbye!");

    panic!("oh no :(");
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

impl<F> std::future::Future for PanicWatchFuture<F>
where
    F: std::future::Future,
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
