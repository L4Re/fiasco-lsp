//! Derived from: https://github.com/kak-lsp/kak-lsp/blob/master/src/language_server_transport.rs
use std::io::{self, BufRead, BufReader, BufWriter, Error, ErrorKind, Read, Result, Write};
use std::process::{Command, Stdio};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use lsp_server::Message;

use crate::thread_worker::Worker;

pub enum Void {}

pub struct LanguageServerTransport {
    // The field order is important as it defines the order of drop.
    // We want to exit a writer loop first (after sending exit notification),
    // then close all pipes and wait until child process is finished.
    // That helps to ensure that reader loop is not stuck trying to read from the language server.
    pub to_lang_server: Worker<Message, Void>,
    pub from_lang_server: Worker<Void, Message>,
    pub errors: Worker<Void, Void>,
}

pub fn start(cmd: &str, args: &[&str]) -> Result<LanguageServerTransport> {
    info!("Starting Language server `{} {}`", cmd, args.join(" "));
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let writer = BufWriter::new(child.stdin.take().expect("Failed to open stdin"));
    let reader = BufReader::new(child.stdout.take().expect("Failed to open stdout"));

    // NOTE 1024 is arbitrary
    let channel_capacity = 1024;

    // XXX temporary way of tracing language server errors
    let mut stderr = BufReader::new(child.stderr.take().expect("Failed to open stderr"));
    let errors =
        Worker::spawn("Language server errors", channel_capacity, move |receiver, _| loop {
            if let Err(TryRecvError::Disconnected) = receiver.try_recv() {
                return;
            }
            let mut buf = String::new();
            match stderr.read_to_string(&mut buf) {
                Ok(_) => {
                    if buf.is_empty() {
                        return;
                    }
                    error!("Language server error: {}", buf);
                }
                Err(e) => {
                    error!("Failed to read from language server stderr: {}", e);
                    return;
                }
            }
        });

    let from_lang_server = Worker::spawn(
        "Messages from language server",
        channel_capacity,
        move |receiver, sender| {
            if let Err(msg) = reader_loop(reader, receiver, &sender) {
                error!("{}", msg);
            }
        },
    );

    let to_lang_server =
        Worker::spawn("Messages to language server", channel_capacity, move |receiver, _| {
            if writer_loop(writer, &receiver).is_err() {
                error!("Failed to write message to language server");
            }
            // NOTE prevent zombie
            debug!("Waiting for language server process end");
            drop(child.stdin.take());
            drop(child.stdout.take());
            drop(child.stderr.take());
            std::thread::sleep(std::time::Duration::from_secs(1));
            match child.try_wait() {
                Ok(None) => {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    if let Ok(None) = child.try_wait() {
                        // Okay, we asked politely enough and waited long enough.
                        child.kill().unwrap();
                    }
                }
                Err(_) => {
                    error!("Language server wasn't running was it?!");
                }
                _ => {}
            }
        });

    Ok(LanguageServerTransport { to_lang_server, from_lang_server, errors })
}

fn reader_loop(
    mut reader: impl BufRead,
    receiver: Receiver<Void>,
    sender: &Sender<Message>,
) -> io::Result<()> {
    loop {
        if let Err(TryRecvError::Disconnected) = receiver.try_recv() {
            return Ok(());
        }
        while let Some(msg) = Message::read(&mut reader)? {
            debug!("From server: {:?}", msg);
            let is_exit = match &msg {
                Message::Notification(n) => n.method == "exit",
                _ => false,
            };

            if sender.send(msg).is_err() {
                return Err(Error::new(ErrorKind::Other, "Failed to send response"));
            }

            if is_exit {
                break;
            }
        }
    }
}

fn writer_loop(mut writer: impl Write, receiver: &Receiver<Message>) -> io::Result<()> {
    for request in receiver {
        debug!("To server: {:?}", request);
        request.write(&mut writer)?;
        writer.flush()?;
    }
    // NOTE we rely on the assumption that language server will exit when its stdin is closed
    // without need to kill child process
    debug!("Received signal to stop language server, closing pipe");
    Ok(())
}
