//! Derived from: https://github.com/rust-lang/rust-analyzer/blob/a2a3ea86eaafdc3bb6287e836a42deadcd02637b/crates/rust-analyzer/src/dispatch.rs

use std::{fmt, mem};

use lsp_server::RequestId;
use serde::de::DeserializeOwned;

use crate::global_state::{Direction, GlobalState, ReqContext, ReqContextAlloc};
use crate::util::{build_notif, build_req, build_res, cast_notif, cast_req, cast_res};

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Direction::ToServer => write!(f, "client"),
            Direction::FromServer => write!(f, "server"),
        }
    }
}

/// A visitor for routing a raw JSON request to an appropriate handler function.
pub struct RequestDispatcher<'a> {
    pub direction: Direction,
    pub req: Option<lsp_server::Request>,
    pub state: &'a mut GlobalState,
}

// TODO: Instead of ReqContextAlloc we could just return a generic value...

impl RequestDispatcher<'_> {
    /// Dispatches the request.
    pub fn on<R>(
        &mut self,
        f: fn(&mut GlobalState, &mut ReqContext, R::Params) -> R::Params,
    ) -> &mut Self
    where
        R: lsp_types::request::Request,
        R::Params: DeserializeOwned,
    {
        let req = match &self.req {
            Some(req) if req.method == R::METHOD => self.req.take().unwrap(),
            _ => return self,
        };

        match cast_req::<R>(req) {
            Ok((mut id, params)) => {
                let mut req_context = self.prepare_req_id(R::METHOD, &mut id);
                // Translate request.
                let mapped = f(self.state, &mut req_context, params);
                self.send_req(req_context, build_req::<R>(id, mapped));
            }
            Err((id, err)) => {
                warn!("Received malformed request from {}: {}", self.direction, err);
                self.state
                    .send(
                        self.direction.reverse(),
                        lsp_server::Response::new_err(
                            id,
                            lsp_server::ErrorCode::InvalidParams as i32,
                            "malformed params".to_string(),
                        ),
                    )
                    .expect(&format!("Lost connection to {}.", self.direction.reverse()));
            }
        };

        self
    }

    pub fn on_many<R>(
        &mut self,
        f: fn(&mut GlobalState, &ReqContextAlloc, R::Params) -> Vec<(R::Params, ReqContext)>,
    ) -> &mut Self
    where
        R: lsp_types::request::Request,
        R::Params: DeserializeOwned,
    {
        let req = match &self.req {
            Some(req) if req.method == R::METHOD => self.req.take().unwrap(),
            _ => return self,
        };

        match cast_req::<R>(req) {
            Ok((id, params)) => {
                // let mut req_context = self.prepare_req_id(&R::METHOD, &mut id);
                let req_context_alloc =
                    ReqContextAlloc { req_method: R::METHOD.to_owned(), req_id: id.clone() };
                // Translate request.
                for (mapped, req_context) in f(self.state, &req_context_alloc, params) {
                    let req_id = RequestId::from(self.state.alloc_req_id() as i32);
                    self.send_req(req_context, build_req::<R>(req_id, mapped));
                }
            }
            Err((id, err)) => {
                warn!("Received malformed request from {}: {}", self.direction, err);
                self.state
                    .send(
                        self.direction.reverse(),
                        lsp_server::Response::new_err(
                            id,
                            lsp_server::ErrorCode::InvalidParams as i32,
                            "malformed params".to_string(),
                        ),
                    )
                    .expect(&format!("Lost connection to {}.", self.direction.reverse()));
            }
        };

        self
    }

    pub fn forward<R>(&mut self) -> &mut Self
    where
        R: lsp_types::request::Request,
    {
        let mut req = match &self.req {
            Some(req) if req.method == R::METHOD => self.req.take().unwrap(),
            _ => return self,
        };

        let req_context = self.prepare_req(&mut req);
        self.send_req(req_context, req);

        self
    }

    pub fn finish(&mut self) {
        if let Some(mut req) = self.req.take() {
            warn!("Unhandled request: {:?}", req);
            let req_context = self.prepare_req(&mut req);
            self.send_req(req_context, req);
        }
    }

    fn prepare_req_id(&mut self, req_method: &str, req_id: &mut RequestId) -> ReqContext {
        let client_req_id = mem::replace(req_id, RequestId::from(self.state.alloc_req_id() as i32));

        ReqContext::new(req_method.to_owned(), client_req_id)
    }

    fn prepare_req(&mut self, req: &mut lsp_server::Request) -> ReqContext {
        self.prepare_req_id(&req.method, &mut req.id)
    }

    fn send_req(&mut self, req_context: ReqContext, req: lsp_server::Request) {
        // Register request as pending.
        self.state.reqs(self.direction).insert(req.id.clone(), req_context);
        // Send request.
        self.state
            .send(self.direction, req)
            .expect(&format!("Lost connection to {}.", self.direction));
    }
}

/// A visitor for routing a raw JSON request to an appropriate handler function.
pub struct ResponseDispatcher<'a> {
    direction: Direction,
    res: Option<lsp_server::Response>,
    req_context: Option<ReqContext>,
    state: &'a mut GlobalState,
}

impl<'a> ResponseDispatcher<'a> {
    pub fn new(
        direction: Direction,
        res: lsp_server::Response,
        state: &'a mut GlobalState,
    ) -> Self {
        // Lookup and remove request type for id.
        let req_context = state.reqs(direction.reverse()).remove(&res.id);
        Self { state, direction, res: Some(res), req_context }
    }

    /// Dispatches the response.
    pub fn on<R>(
        &mut self,
        f: fn(&mut GlobalState, &mut ReqContext, R::Result) -> R::Result,
    ) -> &mut Self
    where
        R: lsp_types::request::Request,
        R::Result: DeserializeOwned,
    {
        if self.req_context.is_none() {
            // Unexpected response (no corresponding request registered), we
            // cannot figure out the request method.
            return self;
        }

        let req_context = self.req_context.as_mut().unwrap();
        let res = match &self.res {
            Some(_) if req_context.method() == R::METHOD => self.res.take().unwrap(),
            _ => return self,
        };

        // Forward errors
        if res.error.is_some() {
            self.send_res(res);
            return self;
        }

        match cast_res::<R>(res) {
            Ok((id, params)) => {
                // Translate response.
                let mapped = f(self.state, req_context, params);
                self.send_res(build_res(id, mapped));
            }
            Err(err) => {
                warn!("Received malformed response from {}: {}", self.direction, err);
                // TODO: Can / have we to report something to the sender?
            }
        };

        self
    }

    pub fn on_collect<R>(
        &mut self,
        f: fn(&mut GlobalState, &mut ReqContext, R::Result) -> Option<R::Result>,
    ) -> &mut Self
    where
        R: lsp_types::request::Request,
        R::Result: DeserializeOwned,
    {
        if self.req_context.is_none() {
            // Unexpected response (no corresponding request registered), we
            // cannot figure out the request method.
            return self;
        }

        let req_context = self.req_context.as_mut().unwrap();
        let res = match &self.res {
            Some(_) if req_context.method() == R::METHOD => self.res.take().unwrap(),
            _ => return self,
        };

        // TODO: Ignore / collect / forward errors? Maybe use a trait instead?
        let orig_req_id = req_context.req_id().clone();
        if res.error.is_some() {
            //panic!("Error on collect!");
            self.send_res(lsp_server::Response { id: orig_req_id, result: None, error: res.error });
            return self;
        }

        match cast_res::<R>(res) {
            Ok((_id, params)) => {
                // Translate response.
                let mapped_opt = f(self.state, req_context, params);
                if let Some(mapped) = mapped_opt {
                    self.send_res(build_res(orig_req_id, mapped));
                }
            }
            Err(err) => {
                panic!("Received malformed response from {}: {}", self.direction, err);
                // TODO: Can / have we to report something to the sender?
                //       We have to count down Rc<> reference counter!
            }
        };

        self
    }

    // There are many requests that take a document (+ optional range) as parameter and returns a vector of result objects.
    // Because on <non-preprocessed>.cpp is mapped to multiple files, for all this requests we need to do split and merge!
    // A generic abstraction in dispatch for that is therefore justified!

    // on_many:
    //  - need to ignore/remember/join errors
    //  - only send response once last response came in

    /// Dispatches the response.
    pub fn forward<R>(&mut self) -> &mut Self
    where
        R: lsp_types::request::Request,
    {
        if self.req_context.is_none() {
            // Unexpected response (no corresponding request registered), we
            // cannot figure out the request method.
            return self;
        }

        let req_context = self.req_context.as_ref().unwrap();
        let res = match &self.res {
            Some(_) if req_context.method() == R::METHOD => self.res.take().unwrap(),
            _ => return self,
        };

        // Forward
        self.send_res(res);

        self
    }

    pub fn finish(&mut self) {
        if self.req_context.is_none() {
            if self.res.is_some() {
                warn!(
                    "Received unexpected response from {} {:#?}.",
                    self.direction,
                    self.res.take()
                );
            }

            return;
        }

        if let Some(res) = self.res.take() {
            warn!("Unhandled response: {:?}", res);
            self.send_res(res);
        }
    }

    fn send_res(&mut self, mut res: lsp_server::Response) {
        // Restore original request id.
        res.id = self.req_context.as_ref().unwrap().req_id().clone();
        // Send response.
        self.state
            .send(self.direction, res)
            .expect(&format!("Lost connection to {}.", self.direction));
    }
}

/// A visitor for routing a raw JSON request to an appropriate handler function.
pub struct NotificationDispatcher<'a> {
    pub direction: Direction,
    pub not: Option<lsp_server::Notification>,
    pub state: &'a mut GlobalState,
}

impl NotificationDispatcher<'_> {
    fn _on<N, S>(
        &mut self,
        f_send: fn(&mut GlobalState, Direction, S),
        f: fn(&mut GlobalState, N::Params) -> S,
    ) -> &mut Self
    where
        N: lsp_types::notification::Notification,
        N::Params: DeserializeOwned,
    {
        let not = match &self.not {
            Some(not) if not.method == N::METHOD => self.not.take().unwrap(),
            _ => return self,
        };

        match cast_notif::<N>(not) {
            Ok(params) => {
                let mapped = f(self.state, params);
                f_send(self.state, self.direction, mapped)
            }
            Err(err) => {
                warn!("Received malformed notification from {}: {}", self.direction, err);
            }
        };

        self
    }

    /// Dispatches the request.
    pub fn on<N>(&mut self, f: fn(&mut GlobalState, N::Params) -> N::Params) -> &mut Self
    where
        N: lsp_types::notification::Notification,
        N::Params: DeserializeOwned,
    {
        self._on::<N, N::Params>(Self::send::<N>, f)
    }

    pub fn on_many<N>(&mut self, f: fn(&mut GlobalState, N::Params) -> Vec<N::Params>) -> &mut Self
    where
        N: lsp_types::notification::Notification,
        N::Params: DeserializeOwned,
    {
        self._on::<N, Vec<N::Params>>(Self::send_many::<N>, f)
    }

    /// Dispatches the request.
    pub fn forward<N>(&mut self) -> &mut Self
    where
        N: lsp_types::notification::Notification,
        N::Params: DeserializeOwned,
    {
        let not = match &self.not {
            Some(not) if not.method == N::METHOD => self.not.take().unwrap(),
            _ => return self,
        };

        // Forward
        self.state
            .send(self.direction, not)
            .expect(&format!("Lost connection to {}.", self.direction));

        self
    }

    pub fn finish(&mut self) {
        if let Some(not) = self.not.take() {
            warn!("Unhandled notification: {:?}", not);
            self.state
                .send(self.direction, not)
                .expect(&format!("Lost connection to {}.", self.direction));
        }
    }

    fn send<N>(state: &mut GlobalState, direction: Direction, params: N::Params)
    where
        N: lsp_types::notification::Notification,
        N::Params: DeserializeOwned,
    {
        state
            .send(direction, build_notif::<N>(params))
            .expect(&format!("Lost connection to {}.", direction))
    }

    fn send_many<N>(state: &mut GlobalState, direction: Direction, params: Vec<N::Params>)
    where
        N: lsp_types::notification::Notification,
        N::Params: DeserializeOwned,
    {
        for p in params {
            Self::send::<N>(state, direction, p)
        }
    }
}
