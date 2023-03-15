use axum::{routing::get, Router};

use crate::AppState;

mod microcontroller;
mod user;

pub static ROOT: &str = "/";
pub static MICROCONTROLLER: &str = "/microcontroller";
pub static USER: &str = "/user";

const CRATE: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

async fn handle_get() -> String {
    format!("{CRATE} {VERSION}")
}

pub fn create_router() -> Router<AppState> {
    Router::new()
        .route(ROOT, get(handle_get))
        .nest(MICROCONTROLLER, microcontroller::create_router())
        .nest(USER, user::create_router())
}
