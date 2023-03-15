use axum::Router;

use crate::AppState;

mod microcontroller;
mod user;

pub static MICROCONTROLLER: &str = "/microcontroller";
pub static USER: &str = "/user";

pub fn create_router() -> Router<AppState> {
    Router::new()
        .nest(MICROCONTROLLER, microcontroller::create_router())
        .nest(USER, user::create_router())
}
