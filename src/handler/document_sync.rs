use std::collections::HashMap;

use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    TextDocumentIdentifier, TextDocumentItem, Url, VersionedTextDocumentIdentifier,
};

use crate::global_state::GlobalState;
use crate::source_mapping::MapDirection::ToPreprocess;

pub fn handle_did_open_text_document(
    state: &mut GlobalState,
    params: DidOpenTextDocumentParams,
) -> Vec<DidOpenTextDocumentParams> {
    let doc = &params.text_document;
    if doc.uri.scheme() != "file" {
        info!("DidOpenTextDocument: Encountered unsupported scheme {}.", doc.uri);
        return vec![params];
    }

    let files = state.source_mapping.map_files(ToPreprocess, doc.uri.path());
    if files.is_empty() {
        warn!("DidOpenTextDocument: Encountered unknown file {}.", doc.uri.path());
        return vec![params];
    }

    let mut result = Vec::new();
    for file in files {
        if let Some(count) = state.open_files.get_mut(file) {
            *count += 1;
            // File already opened, multiple source files might map to the same preprocessed file),
            // we must sent another open notification.
            continue;
        }

        // Remember that file is opened and send notification to server.
        state.open_files.insert(file.clone(), 1);
        result.push(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: Url::from_file_path(file).unwrap(),
                language_id: doc.language_id.clone(),
                // TODO: We need custom version numbering...
                version: doc.version,
                text: std::fs::read_to_string(file).unwrap(),
            },
        })
    }
    result
}

pub fn handle_did_change_text_document(
    state: &mut GlobalState,
    params: DidChangeTextDocumentParams,
) -> Vec<DidChangeTextDocumentParams> {
    let doc = &params.text_document;
    if doc.uri.scheme() != "file" {
        info!("DidChangeTextDocument: Encountered unsupported scheme {}.", doc.uri);
        return vec![params];
    }

    let files = state.source_mapping.map_files(ToPreprocess, doc.uri.path());
    if files.is_empty() {
        warn!("DidChangeTextDocument: Encountered unknown file {}.", doc.uri.path());
        return vec![params];
    }

    let mut result = HashMap::new();
    for mut change in params.content_changes {
        match &mut change.range {
            Some(range) => {
                let mut path = doc.uri.path().to_owned();
                if state.source_mapping.map_range(ToPreprocess, &mut path, range).is_ok() {
                    result.entry(path).or_insert(Vec::new()).push(change);
                }
            }
            None => warn!("TODO: Changing of entire files not yet implemented."),
        }
    }

    result
        .into_iter()
        .map(|(file, changes)| DidChangeTextDocumentParams {
            // TODO: We need custom version numbering...
            text_document: VersionedTextDocumentIdentifier::new(
                Url::from_file_path(file).unwrap(),
                params.text_document.version,
            ),
            content_changes: changes,
        })
        .collect()
}

pub fn handle_did_close_text_document(
    state: &mut GlobalState,
    params: DidCloseTextDocumentParams,
) -> Vec<DidCloseTextDocumentParams> {
    let doc = &params.text_document;
    if doc.uri.scheme() != "file" {
        info!("DidCloseTextDocument: Encountered unsupported scheme {}.", doc.uri);
        return vec![params];
    }

    let files = state.source_mapping.map_files(ToPreprocess, doc.uri.path());
    if files.is_empty() {
        warn!("DidCloseTextDocument: Encountered unknown file {}.", doc.uri.path());
        return vec![params];
    }

    let mut result = Vec::new();
    for file in files {
        match state.open_files.get_mut(file) {
            Some(count) => {
                if *count > 1 {
                    // Opened from other source file, do not send a close
                    // notification, just decrement the open count.
                    *count -= 1;
                    continue;
                }
            }
            None => {
                error!("DidCloseTextDocument: Tried to close non-open file {}.", file.display());
                continue;
            }
        }

        // Remove from opened files.
        state.open_files.remove(file);
        result.push(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: Url::from_file_path(file).unwrap() },
        });
    }
    result
}
