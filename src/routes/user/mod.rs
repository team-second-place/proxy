use axum::{
    extract::{State, WebSocketUpgrade},
    response::Response,
    routing::get,
    Router,
};
use futures::{Sink, SinkExt, Stream, StreamExt};
use nanoid::nanoid;
use tokio::join;
use tracing_unwrap::ResultExt;

use messages::*;

use crate::AppState;

use messages::MicrocontrollerId;

static ROOT: &str = "/";

async fn handle_get(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(|socket| {
        let (write, read) = socket.split();
        handle_ws(write, read, state)
    })
}

#[tracing::instrument(skip(writer, reader, state))]
async fn handle_ws<W, R>(mut writer: W, mut reader: R, state: AppState)
where
    W: Sink<axum::extract::ws::Message> + Unpin,
    R: Stream<Item = Result<axum::extract::ws::Message, axum::Error>> + Unpin,
{
    let (writer_sender, mut writer_receiver) = futures::channel::mpsc::channel(32);
    let (mut reader_sender, reader_receiver) = futures::channel::mpsc::channel(32);

    join!(
        async move {
            while let Some(message) = reader.next().await {
                if let Err(e) = reader_sender.send(message).await {
                    tracing::error!(?e);
                    break;
                }
            }
        },
        async move {
            while let Some(thing) = writer_receiver.next().await {
                if let Err(_e) = writer.send(thing).await {
                    tracing::error!("encountered unloggable error");
                    break;
                }
            }
        },
        handle_ws_inner(reader_receiver, writer_sender, state),
    );
}

#[tracing::instrument(skip(writer, reader, state), fields(user_id))]
async fn handle_ws_inner(
    mut reader: futures::channel::mpsc::Receiver<Result<axum::extract::ws::Message, axum::Error>>,
    mut writer: futures::channel::mpsc::Sender<axum::extract::ws::Message>,
    state: AppState,
) {
    let user_id = nanoid!();
    tracing::Span::current().record("user_id", &user_id);

    state.users.insert(user_id.clone(), writer.clone());

    while let Some(message) = reader.next().await {
        match message {
            Ok(message) => match message {
                axum::extract::ws::Message::Binary(bin) => {
                    let message = decode_from_user(&bin);

                    match message {
                        Ok(message) => {
                            match message {
                                FromUser::Command(command) => {
                                    let authentications = state.authentications.read().await;
                                    let authenticated_for_microcontroller =
                                        authentications.get_from_value(&user_id);
                                    match authenticated_for_microcontroller {
                                        Some(microcontroller_id) => {
                                            match state.microcontrollers.get_mut(microcontroller_id)
                                            {
                                                Some(mut matching_microcontroller) => {
                                                    tracing::info!(
                                                        ?command,
                                                        microcontroller_id,
                                                        "shall be passed along to microcontroller"
                                                    );

                                                    let message = ToMicrocontroller::Command(
                                                        command,
                                                        user_id.clone(),
                                                    );
                                                    let message =
                                                        encode_to_microcontroller(&message)
                                                            .unwrap_or_log();
                                                    let message =
                                                        axum::extract::ws::Message::Binary(message);
                                                    matching_microcontroller
                                                        .send(message)
                                                        .await
                                                        .unwrap_or_log();
                                                }
                                                None => {
                                                    tracing::info!(?command, "couldn't be passed along because the microcontroller is offline");

                                                    let response = ToUser::MicrocontrollerIsOffline;
                                                    let response =
                                                        encode_to_user(&response).unwrap_or_log();
                                                    let response =
                                                        axum::extract::ws::Message::Binary(
                                                            response,
                                                        );
                                                    writer.send(response).await.unwrap_or_log();
                                                }
                                            }
                                        }
                                        None => {
                                            tracing::info!(?command, "rejected due to not being authenticated with a particular microcontroller");

                                            let response = ToUser::UsageError; // TODO: maybe unauthenticated rejection
                                            let response =
                                                encode_to_user(&response).unwrap_or_log();
                                            let response =
                                                axum::extract::ws::Message::Binary(response);
                                            writer.send(response).await.unwrap_or_log();
                                        }
                                    }
                                }
                                FromUser::Authenticate(authenticate) => {
                                    match state
                                        .microcontrollers
                                        .get_mut(&authenticate.microcontroller_id)
                                    {
                                        Some(mut matching_microcontroller) => {
                                            // In the real world, this sensitive info should not be logged!
                                            tracing::info!(?authenticate, "shall be passed along");

                                            let message = ToMicrocontroller::LoginRequest(
                                                authenticate.login_info,
                                                user_id.clone(),
                                            );
                                            let message =
                                                encode_to_microcontroller(&message).unwrap_or_log();
                                            let message =
                                                axum::extract::ws::Message::Binary(message);
                                            matching_microcontroller
                                                .send(message)
                                                .await
                                                .unwrap_or_log();
                                        }

                                        None => {
                                            // In the real world, this sensitive info should not be logged!
                                            tracing::info!(?authenticate, "couldn't be passed along because the microcontroller is offline");

                                            let response = ToUser::MicrocontrollerIsOffline;
                                            let response =
                                                encode_to_user(&response).unwrap_or_log();
                                            let response =
                                                axum::extract::ws::Message::Binary(response);
                                            writer.send(response).await.unwrap_or_log();
                                        }
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!(?error);

                            let response = ToUser::UsageError;
                            let response = encode_to_user(&response).unwrap_or_log();
                            let response = axum::extract::ws::Message::Binary(response);
                            writer.send(response).await.unwrap_or_log();
                        }
                    }
                }
                axum::extract::ws::Message::Text(text) => {
                    tracing::warn!(text, "was supposed to be a binary frame but is text");

                    let response = ToUser::UsageError;
                    let response = encode_to_user(&response).unwrap_or_log();
                    let response = axum::extract::ws::Message::Binary(response);
                    writer.send(response).await.unwrap_or_log();
                }
                axum::extract::ws::Message::Ping(_) => {}
                axum::extract::ws::Message::Pong(_) => {}
                axum::extract::ws::Message::Close(_) => break,
            },
            Err(error) => {
                tracing::error!(?error, "while reading from user websocket");
                break;
            }
        }
    }

    let mut authentications = state.authentications.write().await;
    authentications.remove_value(&user_id);
    drop(authentications);

    state.users.remove(&user_id);
}

pub fn create_router() -> Router<AppState> {
    Router::new().route(ROOT, get(handle_get))
}

#[cfg(test)]
mod tests {
    // TODO: write unit tests following https://github.com/tokio-rs/axum/blob/978ae6335862b264b4368e01a52177d98c42b2d9/examples/testing-websockets/src/main.rs#L128-L148
    #[tokio::test]
    async fn first_unit_test() {
        todo!();
    }

    #[tokio::test]
    async fn authenticate() {
        todo!();
    }
}
