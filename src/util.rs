use std::collections::HashSet;
use std::path::Path;

use lsp_server::{Notification, Request, RequestId, Response};
use lsp_types::{Location, Position, Range, Url};
use serde::{de::DeserializeOwned, Serialize};

use crate::source_mapping::{FiascoSourceMapping, MapDirection};

#[derive(Debug)]
pub enum CastError<T> {
    /// The extracted message was of a different method than expected.
    MethodMismatch(T),
    /// Failed to deserialize the message.
    JsonError { method: String, error: serde_json::Error },
}

pub fn cast_req<R>(req: Request) -> Result<(RequestId, R::Params), (RequestId, String)>
where
    R: lsp_types::request::Request,
    R::Params: DeserializeOwned,
{
    let id = req.id.clone();
    req.extract(R::METHOD).map_err(|e| (id, format!("{}", e)))
}

pub fn cast_res<R>(res: Response) -> Result<(RequestId, R::Result), String>
where
    R: lsp_types::request::Request,
    R::Params: DeserializeOwned,
{
    match res.result {
        None => Err("No result present.".to_owned()),
        Some(r) => match serde_json::from_value(r) {
            Ok(result) => Ok((res.id, result)),
            Err(error) => Err(format!("{}", error)),
        },
    }
}

pub fn cast_notif<N>(not: Notification) -> Result<N::Params, String>
where
    N: lsp_types::notification::Notification,
    N::Params: DeserializeOwned,
{
    not.extract(N::METHOD).map_err(|e| format!("{}", e))
}

pub fn build_req<R>(id: RequestId, params: R::Params) -> Request
where
    R: lsp_types::request::Request,
    R::Params: Serialize,
{
    Request::new(id, R::METHOD.to_owned(), params)
}

pub fn build_res<R: Serialize>(id: RequestId, result: R) -> Response {
    Response::new_ok(id, result)
}

pub fn build_notif<N>(params: N::Params) -> Notification
where
    N: lsp_types::notification::Notification,
    N::Params: Serialize,
{
    Notification::new(N::METHOD.to_owned(), params)
}

impl FiascoSourceMapping {
    pub fn map_position(
        &self,
        direction: MapDirection,
        path: &mut String,
        position: &mut Position,
    ) {
        let mapped = self.map(direction, path, position.line, position.character);

        *path = mapped.path.to_str().unwrap().to_owned();
        position.line = mapped.line;
        position.character = mapped.character;
    }

    pub fn map_position_uri(
        &self,
        direction: MapDirection,
        uri: &mut Url,
        position: &mut Position,
    ) {
        assert_eq!(uri.scheme(), "file");
        let mut path = uri.path().to_owned();
        self.map_position(direction, &mut path, position);
        *uri = Url::from_file_path(path).unwrap();
    }

    pub fn map_range(
        &self,
        direction: MapDirection,
        path: &mut String,
        range: &mut Range,
    ) -> Result<(), ()> {
        let mapped_start = self.map(direction, path, range.start.line, range.start.character);

        let mapped_end = self.map(direction, path, range.end.line, range.end.character);

        if mapped_start.path != mapped_end.path {
            debug!("Range mapping across source files: {:?} vs. {:?}", &mapped_start, &mapped_end);
            return Err(());
        }

        *path = mapped_start.path.to_str().unwrap().to_owned();
        range.start.line = mapped_start.line;
        range.start.character = mapped_start.character;
        range.end.line = mapped_end.line;
        range.end.character = mapped_end.character;
        Ok(())
    }

    pub fn map_range_uri(
        &self,
        direction: MapDirection,
        uri: &mut Url,
        range: &mut Range,
    ) -> Result<(), ()> {
        assert_eq!(uri.scheme(), "file");
        let mut path = uri.path().to_owned();
        self.map_range(direction, &mut path, range)?;
        *uri = Url::from_file_path(path).unwrap();
        Ok(())
    }

    pub fn map_location(&self, direction: MapDirection, location: &mut Location) -> Result<(), ()> {
        self.map_range_uri(direction, &mut location.uri, &mut location.range)
    }

    pub fn map_file_range(
        &self,
        direction: MapDirection,
        path: &str,
        range: &Range,
    ) -> HashSet<&Path> {
        self.map_files_with_range(direction, path, range.start.line, range.end.line)
    }

    pub fn map_file_range_uri(
        &self,
        direction: MapDirection,
        uri: &Url,
        range: &Range,
    ) -> HashSet<&Path> {
        assert_eq!(uri.scheme(), "file");
        self.map_file_range(direction, uri.path(), range)
    }
}
