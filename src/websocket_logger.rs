use std::net::TcpListener;
use std::thread::spawn;

use color_eyre::eyre::{Context, Result};
use crossbeam_channel::{bounded, select, Receiver, Sender, TrySendError};
use lsp_server::Message;
use serde_json::json;
use tungstenite::accept;
use tungstenite::Message::Text;

use crate::global_state::Direction;

pub struct Logger {
    sender: Sender<String>,
    receiver: Receiver<String>,
}

impl Direction {
    fn to_lsp_log(&self) -> u32 {
        match self {
            Direction::ToServer => 1,
            Direction::FromServer => 2,
        }
    }
}

impl Logger {
    pub fn spawn() -> Logger {
        let (sender, receiver) = bounded(1024);
        info!("Spawn logger!");
        spawn({
            let receiver = receiver.clone();
            move || Self::log_socket_handler(receiver)
        });
        Logger { sender, receiver }
    }

    fn log_socket_handler(receiver: Receiver<String>) {
        let server = TcpListener::bind("127.0.0.1:9981").unwrap();
        loop {
            let do_accept = |reconnect| {
                if reconnect {
                    warn!("Lost connection to logger websocket, accept new connection.");
                }
                let client = accept(server.accept().unwrap().0).unwrap();
                info!("Connected to logger websocket.");
                client
            };

            let mut websocket = do_accept(false);
            loop {
                select! {
                    recv(receiver) -> r => {
                        if !websocket.can_write() {
                            websocket = do_accept(true);
                        }
                        let msg = r.unwrap();
                        trace!("Sending message to logger: {}", msg);
                        if let Err(err) = websocket.send(Text(msg.into())) {
                            warn!("Error while sending message to logger: {err}");
                            websocket = do_accept(true);
                        }
                    }
                }
            }
        }
    }

    pub fn send(&mut self, direction: Direction, msg: &Message) -> Result<()> {
        let log_msg = match msg {
            Message::Request(req) => {
                json!({
                    "id": req.id,
                    "method": req.method,
                    "params": req.params,
                    "direction": direction.to_lsp_log(),
                })
            }
            Message::Response(res) => {
                json!({
                    "id": res.id,
                    "params": res.result,
                    "direction": direction.to_lsp_log(),
                    "isError": res.error.is_some(),
                })
            }
            Message::Notification(not) => {
                json!({
                    "method": not.method,
                    "params": not.params,
                    "direction": direction.to_lsp_log(),
                })
            }
        }
        .to_string();

        match self.sender.try_send(log_msg) {
            // Channel is full, try to remove the oldest entry.
            Err(TrySendError::Full(log_msg)) => {
                if self.receiver.try_recv().is_ok() {
                    debug!("Websocket logger queue overfull, dropped oldest entry.");
                }

                // Retry the send, channel should have a free entry again.
                self.sender.try_send(log_msg)
            }
            r => r,
        }
        .wrap_err("Failed to log message to websocket logger.")
    }
}
