use std::any::Any;
use std::{collections::HashMap, path::PathBuf};

use color_eyre::eyre::Result;
use lsp_server::{Connection, RequestId};

use crate::language_server_transport::LanguageServerTransport;
use crate::source_mapping::FiascoSourceMapping;
use crate::websocket_logger::Logger;

#[derive(Clone, Copy)]
pub enum Direction {
    ToServer,
    FromServer,
}

impl Direction {
    pub fn reverse(&self) -> Direction {
        match self {
            Direction::ToServer => Direction::FromServer,
            Direction::FromServer => Direction::ToServer,
        }
    }
}

pub struct ReqContext {
    method: String,
    /// Request id of the client request.
    req_id: RequestId,
    value: Option<Box<dyn Any>>,
}

impl ReqContext {
    pub fn new(method: String, req_id: RequestId) -> Self {
        Self { method, req_id, value: None }
    }

    pub fn method(&self) -> &str {
        self.method.as_ref()
    }

    pub fn req_id(&self) -> &RequestId {
        &self.req_id
    }

    pub fn set_value<T: Any>(&mut self, value: T) {
        self.value.replace(Box::new(value));
    }

    pub fn take_value<T: Any>(&mut self) -> Option<T> {
        self.value.take().map(|value| *value.downcast().unwrap())
    }
}

type RequestRegistry = HashMap<RequestId, ReqContext>;

pub struct ReqContextAlloc {
    pub req_method: String,
    pub req_id: RequestId,
}

impl ReqContextAlloc {
    pub fn alloc(&self) -> ReqContext {
        ReqContext::new(self.req_method.clone(), self.req_id.clone())
    }
}

pub struct GlobalState {
    pub client: Connection,
    pub server: LanguageServerTransport,
    logger: Logger,
    pub source_mapping: FiascoSourceMapping,
    pub open_files: HashMap<PathBuf, u32>,
    pub client_reqs: RequestRegistry,
    pub server_reqs: RequestRegistry,
    pub next_req_id: u32,
}

impl GlobalState {
    pub fn new(
        client: Connection,
        server: LanguageServerTransport,
        logger: Logger,
        source_mapping: FiascoSourceMapping,
    ) -> GlobalState {
        GlobalState {
            client,
            server,
            logger,
            source_mapping,
            open_files: HashMap::new(),
            client_reqs: RequestRegistry::new(),
            server_reqs: RequestRegistry::new(),
            next_req_id: 0,
        }
    }

    pub fn log_from_server(&mut self, msg: &lsp_server::Message) -> Result<()> {
        self.logger.send(Direction::FromServer, msg)?;
        Ok(())
    }

    pub fn send_to_server<M>(&mut self, m: M) -> Result<()>
    where
        M: Into<lsp_server::Message>,
    {
        let msg = m.into();
        self.logger.send(Direction::ToServer, &msg)?;
        self.server.to_lang_server.sender().send(msg)?;
        Ok(())
    }

    pub fn send_to_client<M>(&mut self, m: M) -> Result<()>
    where
        M: Into<lsp_server::Message>,
    {
        self.client.sender.send(m.into())?;
        Ok(())
    }

    pub fn send<M>(&mut self, direction: Direction, m: M) -> Result<()>
    where
        M: Into<lsp_server::Message>,
    {
        match direction {
            Direction::ToServer => self.send_to_server(m),
            Direction::FromServer => self.send_to_client(m),
        }
    }

    pub fn reqs(&mut self, direction: Direction) -> &mut RequestRegistry {
        match direction {
            Direction::ToServer => &mut self.client_reqs,
            Direction::FromServer => &mut self.server_reqs,
        }
    }

    pub fn alloc_req_id(&mut self) -> u32 {
        let req_id = self.next_req_id;
        self.next_req_id += 1;
        req_id
    }
}
