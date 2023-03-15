use std::net::SocketAddr;

use error_stack::{IntoReport, Report, ResultExt};
use server_proxy::{create_app, create_default_state, App};

use clap::Parser;

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    // TODO: how do I make this not need the crate name?
    /// tracing/log crate logging directives
    #[arg(long, env, default_value = "server_proxy=info")]
    log: String,

    /// Port to run the app on
    #[arg(short, long, env)]
    port: u16,
}

#[derive(Debug, thiserror::Error)]
#[error("program arguments error")]
struct ArgsError;

#[derive(Debug, thiserror::Error)]
#[error("web server error")]
struct ServerError;

#[derive(Debug, thiserror::Error)]
#[error("app error")]
struct AppError;

#[tracing::instrument(skip(app))]
async fn serve(addr: &SocketAddr, app: App) -> Result<(), Report<ServerError>> {
    let binding = axum::Server::try_bind(addr)
        .into_report()
        .change_context_lazy(|| ServerError)?;

    tracing::info!("serving");

    binding
        .serve(app)
        .await
        .into_report()
        .change_context_lazy(|| ServerError)
}

#[tokio::main]
async fn main() -> Result<(), Report<AppError>> {
    let args = Args::try_parse()
        .into_report()
        .change_context_lazy(|| ArgsError)
        .change_context_lazy(|| AppError)?;

    let log = args.log;

    let env_filter = tracing_subscriber::EnvFilter::builder()
        .parse(log)
        .into_report()
        .change_context_lazy(|| ArgsError)
        .change_context_lazy(|| AppError)?;

    use tracing_subscriber::fmt::format::FmtSpan;

    tracing_subscriber::fmt()
        .pretty()
        .with_env_filter(env_filter)
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .init();

    let app_state = create_default_state();
    let app = create_app(app_state);

    let port = args.port;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    serve(&addr, app).await.change_context_lazy(|| AppError)
}
