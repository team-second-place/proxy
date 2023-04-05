use axum::{
    extract::{State, WebSocketUpgrade},
    response::Response,
    routing::get,
    Router,
};
use futures::{Sink, SinkExt, Stream, StreamExt};
use messages::{
    decode_from_microcontroller, encode_to_microcontroller, encode_to_user, Authenticated, Data,
    FromMicrocontroller, MicrocontrollerId, Register, ToMicrocontroller, ToUser, UserId,
};
use tokio::{join, task::JoinError};
use tracing_unwrap::{OptionExt, ResultExt};

use crate::{AppState, WebsocketReceiver, WebsocketSender};

async fn try_join_all<F, T>(futures: Vec<F>) -> Vec<Result<T, JoinError>>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let mut handles = Vec::with_capacity(futures.len());

    for future in futures {
        let handle = tokio::spawn(future);

        handles.push(handle);
    }

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        results.push(handle.await);
    }

    results
}

async fn join_all<F, T>(futures: Vec<F>) -> Vec<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let result: Result<Vec<T>, JoinError> = try_join_all(futures).await.into_iter().collect();
    result.unwrap()
}

async fn pass_along_data<I>(data: Data, user_senders: I)
where
    I: Iterator<Item = crate::UserSender>,
{
    let message = ToUser::Data(data);
    let message = encode_to_user(&message).unwrap_or_log();
    let message = axum::extract::ws::Message::Binary(message);

    join_all(
        user_senders
            .map(|mut user_sender| {
                let message = message.clone();
                async move {
                    user_sender.send(message).await.unwrap_or_log();
                    // Doesn't wait for it to be flushed to the user
                }
            })
            .collect(),
    )
    .await;
}

#[tracing::instrument]
async fn on_microcontroller_register(
    register: Register,
    writer: &mut WebsocketSender,
    registered_id: &mut Option<MicrocontrollerId>,
    state: &AppState,
    span: &tracing::Span,
) {
    let microcontroller_id = register.microcontroller_id;
    span.record("microcontroller_id", microcontroller_id.clone());

    registered_id.replace(microcontroller_id.clone());

    state
        .microcontrollers
        .insert(microcontroller_id.clone(), writer.clone());

    // Because users cannot connect to the microcontroller without it already being registered,
    // UsersAreOffline is the denoted response to a registration
    let response = ToMicrocontroller::UsersAreOffline;
    let response = encode_to_microcontroller(&response).unwrap_or_log();
    let response = axum::extract::ws::Message::Binary(response);
    writer.send(response).await.unwrap_or_log();
}

async fn on_microcontroller_user_specific_data(
    data: Data,
    user_ids: Vec<UserId>,
    writer: &mut WebsocketSender,
    registered_id: &Option<MicrocontrollerId>,
    state: &AppState,
) {
    // TODO: registered_id will be a MicrocontrollerId rather than an Option of one once extracted
    match registered_id {
        Some(microcontroller_id) => {
            let authentications = state.authentications.read().await;
            let connected_users = authentications.get_from_key(microcontroller_id);

            if let Some(connected_users) = connected_users {
                let filtered_user_ids: Vec<UserId> = user_ids
                    .into_iter()
                    .filter(|user_id| connected_users.contains(user_id))
                    .collect();

                if filtered_user_ids.is_empty() {
                    tracing::info!(
                        ?data,
                        "couldn't be passed along because no users are online"
                    );

                    let response = ToMicrocontroller::UsersAreOffline;
                    let response = encode_to_microcontroller(&response).unwrap_or_log();
                    let response = axum::extract::ws::Message::Binary(response);
                    writer.send(response).await.unwrap_or_log();
                } else {
                    tracing::info!(
                        ?data,
                        ?filtered_user_ids,
                        registered_id,
                        "shall be passed along"
                    );

                    let user_senders = filtered_user_ids.iter().filter_map(|user_id| {
                        let sender = state.users.get(user_id);
                        sender.map(|sender| sender.clone())
                    });

                    pass_along_data(data, user_senders).await;
                }
            }
        }
        None => {
            tracing::info!(?data, "rejected due to not being registered");

            let response = ToMicrocontroller::UsageError; // TODO: maybe unauthenticated rejection
            let response = encode_to_microcontroller(&response).unwrap_or_log();
            let response = axum::extract::ws::Message::Binary(response);
            writer.send(response).await.unwrap_or_log();
        }
    }
}

#[tracing::instrument(skip(writer, state))]
async fn on_microcontroller_broadcast_data(
    data: Data,
    writer: &mut WebsocketSender,
    registered_id: &Option<MicrocontrollerId>,
    state: &AppState,
) {
    match registered_id {
        Some(microcontroller_id) => {
            let authentications = state.authentications.read().await;

            match authentications.get_from_key(microcontroller_id) {
                Some(connected_users) => {
                    let user_senders = connected_users.iter().map(|connected_user| {
                        let sender = state.users.get(connected_user).unwrap_or_log();

                        sender.clone()
                    });

                    pass_along_data(data, user_senders).await;
                }
                None => {
                    tracing::info!(
                        ?data,
                        "couldn't be passed along because no users are online"
                    );

                    let response = ToMicrocontroller::UsersAreOffline;
                    let response = encode_to_microcontroller(&response).unwrap_or_log();
                    let response = axum::extract::ws::Message::Binary(response);
                    writer.send(response).await.unwrap_or_log();
                }
            }
        }
        None => {
            tracing::info!(?data, "rejected due to not being registered");

            let response = ToMicrocontroller::UsageError; // TODO: maybe unauthenticated rejection
            let response = encode_to_microcontroller(&response).unwrap_or_log();
            let response = axum::extract::ws::Message::Binary(response);
            writer.send(response).await.unwrap_or_log();
        }
    }
}

#[tracing::instrument(skip(state))]
async fn on_microcontroller_login_response(
    authenticated: bool,
    user_id: UserId,
    writer: &mut WebsocketSender,
    registered_id: &Option<MicrocontrollerId>,
    state: &AppState,
) {
    if authenticated {
        if let Some(microcontroller_id) = registered_id {
            let mut authentications = state.authentications.write().await;
            authentications.put(microcontroller_id.clone(), user_id.clone());
            drop(authentications);

            if let Some(mut user_writer) = state.users.get_mut(&user_id) {
                let response = ToUser::Authenticated(Authenticated(true));
                let response = encode_to_user(&response).unwrap_or_log();
                let response = axum::extract::ws::Message::Binary(response);

                user_writer.send(response).await.unwrap_or_log();
            } else {
                tracing::info!(
                    ?user_id,
                    "authenticating this user was rejected due to them not being connected"
                );

                // Not really a usage error cause the user may have disconnected recently?
                let response = ToMicrocontroller::UsageError;
                let response = encode_to_microcontroller(&response).unwrap_or_log();
                let response = axum::extract::ws::Message::Binary(response);
                writer.send(response).await.unwrap_or_log();
            }
        } else {
            tracing::info!(
                ?user_id,
                "authenticating with this microcontroller was rejected due to it not being registered"
            );

            let response = ToMicrocontroller::UsageError; // TODO: maybe unauthenticated rejection (more specific than usage error)
            let response = encode_to_microcontroller(&response).unwrap_or_log();
            let response = axum::extract::ws::Message::Binary(response);
            writer.send(response).await.unwrap_or_log();
        }
    } else {
        if let Some(mut user_writer) = state.users.get_mut(&user_id) {
            let response = ToUser::Authenticated(Authenticated(false));
            let response = encode_to_user(&response).unwrap_or_log();
            let response = axum::extract::ws::Message::Binary(response);

            user_writer.send(response).await.unwrap_or_log();
        } else {
            tracing::info!(
                ?user_id,
                "refusing to authenticate this user had no effect because they aren't connected"
            );

            let response = ToMicrocontroller::UsageError;
            let response = encode_to_microcontroller(&response).unwrap_or_log();
            let response = axum::extract::ws::Message::Binary(response);
            writer.send(response).await.unwrap_or_log();
        }
    }
}

#[tracing::instrument(skip(writer))]
async fn on_message_decode_error(error: messages::DecodeError, writer: &mut WebsocketSender) {
    tracing::warn!(?error);

    let response = ToMicrocontroller::UsageError;
    let response = encode_to_microcontroller(&response).unwrap_or_log();
    let response = axum::extract::ws::Message::Binary(response);

    writer.send(response).await.unwrap_or_log();
}

#[tracing::instrument(skip(writer))]
async fn on_unexpected_text_frame(text: String, writer: &mut WebsocketSender) {
    tracing::warn!(text, "was supposed to be a binary frame but is text");

    let response = ToMicrocontroller::UsageError;
    let response = encode_to_microcontroller(&response).unwrap_or_log();
    let response = axum::extract::ws::Message::Binary(response);
    writer.send(response).await.unwrap_or_log();
}

static ROOT: &str = "/";

#[tracing::instrument(skip(state))]
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

#[tracing::instrument(skip(writer, reader, state), fields(microcontroller_id))]
async fn handle_ws_inner(
    mut reader: WebsocketReceiver,
    mut writer: WebsocketSender,
    state: AppState,
) {
    let mut registered_id: Option<MicrocontrollerId> = None;

    while let Some(message) = reader.next().await {
        match message {
            Ok(message) => match message {
                axum::extract::ws::Message::Binary(bin) => {
                    let message = decode_from_microcontroller(&bin);

                    match message {
                        Ok(message) => match message {
                            FromMicrocontroller::BroadcastData(data) => {
                                on_microcontroller_broadcast_data(
                                    data,
                                    &mut writer,
                                    &registered_id,
                                    &state,
                                )
                                .await
                            }
                            FromMicrocontroller::UserSpecificData(data, user_ids) => {
                                on_microcontroller_user_specific_data(
                                    data,
                                    user_ids,
                                    &mut writer,
                                    &registered_id,
                                    &state,
                                )
                                .await
                            }
                            FromMicrocontroller::Register(register) => {
                                let span = tracing::Span::current();
                                // There is no registration challenge,
                                // but there would be in a production version of the system

                                on_microcontroller_register(
                                    register,
                                    &mut writer,
                                    &mut registered_id,
                                    &state,
                                    &span,
                                )
                                .await;
                            }
                            FromMicrocontroller::LoginResponse(authenticated, user_id) => {
                                on_microcontroller_login_response(
                                    authenticated,
                                    user_id,
                                    &mut writer,
                                    &registered_id,
                                    &state,
                                )
                                .await;
                            }
                        },
                        Err(error) => {
                            on_message_decode_error(error, &mut writer).await;
                        }
                    }
                }
                axum::extract::ws::Message::Text(text) => {
                    on_unexpected_text_frame(text, &mut writer).await;
                }
                axum::extract::ws::Message::Ping(_) => {}
                axum::extract::ws::Message::Pong(_) => {}
                axum::extract::ws::Message::Close(_) => break,
            },
            Err(error) => {
                tracing::error!(?error, "while reading from microcontroller websocket");
                break;
            }
        }
    }

    if let Some(microcontroller_id) = registered_id {
        state
            .authentications
            .write()
            .await
            .remove_key(&microcontroller_id);
        state.microcontrollers.remove(&microcontroller_id);
    }
}

pub fn create_router() -> Router<AppState> {
    Router::new().route(ROOT, get(handle_get))
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use futures::{SinkExt, StreamExt};
    use messages::{
        decode_to_microcontroller, encode_from_microcontroller, FromMicrocontroller, Register,
        ToMicrocontroller,
    };
    use tokio::time::timeout;

    use crate::create_default_state;

    use super::handle_ws_inner;

    // TODO: write unit tests following https://github.com/tokio-rs/axum/blob/978ae6335862b264b4368e01a52177d98c42b2d9/examples/testing-websockets/src/main.rs#L128-L148

    #[tokio::test]
    #[ntest::timeout(2000)]
    async fn register() {
        let state = create_default_state();

        let (socket_write, mut test_rx) = futures::channel::mpsc::channel(128);
        let (mut test_tx, socket_read) = futures::channel::mpsc::channel(128);

        tokio::spawn(handle_ws_inner(
            socket_read,
            socket_write,
            Arc::clone(&state),
        ));

        let microcontroller_id = String::from(
            "MFwwDQYJKoZIhvcNAQEBBQADSwAwSAJBAMgBTQIs1MJy0ZNl9wSdNzk1A7j5468g
QyQJCYHO5fF7ma3zFB4HjOo+2CmKJCOwDf/VR3pEy9kQJceYOUMrE2kCAwEAAQ==",
        );

        let registration_message = FromMicrocontroller::Register(Register { microcontroller_id });
        let registration_message = encode_from_microcontroller(&registration_message)
            .expect("failed to encode registration message");
        let registration_message = axum::extract::ws::Message::Binary(registration_message);

        test_tx
            .send(Ok(registration_message))
            .await
            .expect("failed to send registration message");

        let message_back = timeout(Duration::from_millis(1000), test_rx.next())
            .await
            .expect("shouldn't have timed out")
            .expect("a message should've been sent back after succesful registration");

        match message_back {
            axum::extract::ws::Message::Binary(message_back) => {
                let message_back = decode_to_microcontroller(&message_back)
                    .expect("failed to decode message back to microcontroller");

                match message_back {
                    ToMicrocontroller::UsersAreOffline => {}
                    other => panic!("didn't expect to get {other:?}"),
                }
            }
            other => panic!("didn't expect to get {other:?}"),
        }
    }
}
