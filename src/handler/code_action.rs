use std::collections::HashMap;

use lsp_types::{CodeActionOrCommand, CodeActionParams, CodeActionResponse};

use crate::global_state::{GlobalState, ReqContext};
use crate::source_mapping::MapDirection::{FromPreprocess, ToPreprocess};

pub fn handle_req_code_action(
    state: &mut GlobalState,
    req_context: &mut ReqContext,
    mut params: CodeActionParams,
) -> CodeActionParams {
    let doc = &mut params.text_document;
    if doc.uri.scheme() != "file" {
        info!("CodeActionRequest: Encountered unsupported scheme {}.", doc.uri);
        return params;
    }

    let source_path = doc.uri.path().to_owned();
    if state.source_mapping.map_files(ToPreprocess, &source_path).is_empty() {
        warn!("CodeActionRequest: Encountered unknown file {}.", source_path);
        return params;
    }

    if state.source_mapping.map_range_uri(ToPreprocess, &mut doc.uri, &mut params.range).is_err() {
        warn!("CodeActionRequest: Encountered unmappable range {:?}.", &params.range);
        return params;
    }

    // Save translated file path for response.
    req_context.set_value((source_path.clone(), doc.uri.path().to_owned()));

    // Map diagnostics in CodeActionParams
    params.context.diagnostics.retain_mut(|diagnostic| {
        let mut diagnostic_path = source_path.clone();
        if state
            .source_mapping
            .map_range(ToPreprocess, &mut diagnostic_path, &mut diagnostic.range)
            .is_err()
        {
            warn!("CodeActionRequest: Encountered unmappable range {:?}.", &diagnostic.range);
            return false;
        }

        let in_same_doc = diagnostic_path == doc.uri.path();
        if !in_same_doc {
            warn!(
                "CodeActionRequest: Diagnostic mapped to different file ({}) than code action range ({}).",
                diagnostic_path,
                doc.uri.path()
            );
        }
        in_same_doc
    });

    params
}

pub fn handle_res_code_action(
    state: &mut GlobalState,
    req_context: &mut ReqContext,
    res: Option<CodeActionResponse>,
) -> Option<CodeActionResponse> {
    let (source_path, path) = match req_context.take_value::<(String, String)>() {
        None => return res,
        Some(t) => t,
    };
    let mut result = res?;
    for cc in &mut result {
        if let CodeActionOrCommand::CodeAction(action) = cc {
            if let Some(edit) = &mut action.edit {
                if let Some(changes) = edit.changes.take() {
                    let mut mapped_changes = HashMap::new();
                    for (url, text_edits) in changes.into_iter() {
                        for mut text_edit in text_edits {
                            let mut edit_url = url.clone();
                            if state
                                .source_mapping
                                .map_range_uri(FromPreprocess, &mut edit_url, &mut text_edit.range)
                                .is_ok()
                            {
                                mapped_changes
                                    .entry(edit_url)
                                    .or_insert(Vec::new())
                                    .push(text_edit);
                            }
                        }
                    }
                    edit.changes.replace(mapped_changes);
                }
                // TODO: document_changes and change_annotations
            }

            // Map diagnostics in CodeAction
            if let Some(diagnostics) = action.diagnostics.as_mut() {
                diagnostics.retain_mut(|diagnostic| {
                    let mut diagnostic_path = path.clone();
                    if state
                        .source_mapping
                        .map_range(FromPreprocess, &mut diagnostic_path, &mut diagnostic.range)
                        .is_err()
                    {
                        warn!(
                            "CodeActionRequest: Encountered unmappable range {:?}.",
                            &diagnostic.range
                        );
                        return false;
                    }

                    let in_same_doc = diagnostic_path == source_path;
                    if !in_same_doc {
                        warn!(
                            "CodeAction: Diagnostic mapped to different file ({}) than source file specified in request ({}).",
                            diagnostic_path, source_path
                        );
                    }
                    in_same_doc
                })
            }
        };
    }
    Some(result)
}
