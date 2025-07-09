use std::cell::RefCell;
use std::rc::Rc;

use lsp_types::{
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, SymbolInformation, Url,
};

use crate::global_state::{GlobalState, ReqContext, ReqContextAlloc};
use crate::source_mapping::MapDirection::{FromPreprocess, ToPreprocess};

struct DocSymbolState {
    source_path: String,
    mapped_path: String,
    result: Rc<RefCell<Option<DocumentSymbolResponse>>>,
}

// TODO: Maybe add generic abstraction for File+Range -> Many files -> LSP -> One file / Filter File+Range

pub fn handle_req_doc_symbol(
    state: &mut GlobalState,
    req_context_alloc: &ReqContextAlloc,
    params: DocumentSymbolParams,
) -> Vec<(DocumentSymbolParams, ReqContext)> {
    let doc = &params.text_document;
    if doc.uri.scheme() != "file" {
        info!("DocumentSymbolRequest: Encountered unsupported scheme {}.", doc.uri);
        return vec![(params, req_context_alloc.alloc())];
    }

    let source_path = doc.uri.path().to_owned();
    let files = state.source_mapping.map_files(ToPreprocess, &source_path);
    if files.is_empty() {
        warn!("DocumentSymbolRequest: Encountered unknown file {}.", source_path);
        return vec![(params, req_context_alloc.alloc())];
    }

    let result_vec = Rc::new(RefCell::new(Option::None));
    let mut result = Vec::new();

    // Split up into one request per file...
    for mapped_path in files {
        // Save translated file path for response.
        let mut req_context = req_context_alloc.alloc();
        req_context.set_value(DocSymbolState {
            source_path: source_path.clone(),
            // TODO: Store Path here?
            mapped_path: mapped_path.to_str().unwrap().to_owned(),
            result: result_vec.clone(),
        });

        let mut req_params = params.clone();
        // Update file.
        req_params.text_document.uri = Url::from_file_path(mapped_path).unwrap();

        result.push((req_params, req_context));
    }

    result
}

fn filter_symbol_informations(
    state: &mut GlobalState,
    req_state: &DocSymbolState,
    symbols: Vec<SymbolInformation>,
) -> Vec<SymbolInformation> {
    symbols
        .into_iter()
        .filter_map(|mut doc_symbol| {
            let doc_symbol_path = req_state.mapped_path.clone();
            if state.source_mapping.map_location(FromPreprocess, &mut doc_symbol.location).is_err()
            {
                warn!("Drop {} due to map location error.", &doc_symbol.name);
                return None;
            }

            if doc_symbol_path == req_state.source_path {
                Some(doc_symbol)
            } else {
                warn!(
                    "Drop {} due to different file: {} vs. {}.",
                    &doc_symbol.name, &doc_symbol_path, &req_state.source_path
                );
                None
            }
        })
        .collect()
}

fn filter_document_symbols(
    state: &mut GlobalState,
    req_state: &DocSymbolState,
    symbols: Vec<DocumentSymbol>,
) -> Vec<DocumentSymbol> {
    symbols
        .into_iter()
        .filter_map(|mut doc_symbol| {
            let mut doc_symbol_path = req_state.mapped_path.clone();
            if state
                .source_mapping
                .map_range(FromPreprocess, &mut doc_symbol_path, &mut doc_symbol.range)
                .is_err()
            {
                warn!("Drop {} due to map range error.", &doc_symbol.name);
                return None;
            }

            if doc_symbol_path != req_state.source_path {
                warn!(
                    "Drop {} due to different file: {} vs. {}.",
                    &doc_symbol.name, &doc_symbol_path, &req_state.source_path
                );
                return None;
            }

            let mut doc_symbol_selection_path = req_state.mapped_path.clone();
            if state
                .source_mapping
                .map_range(
                    FromPreprocess,
                    &mut doc_symbol_selection_path,
                    &mut doc_symbol.selection_range,
                )
                .is_err()
                || doc_symbol_selection_path != doc_symbol_path
            {
                warn!("DocumentSymbolResponse: Selection range range mapped to different file.");
                return None;
            }

            doc_symbol.children = doc_symbol
                .children
                .map(|children| filter_document_symbols(state, req_state, children));

            Some(doc_symbol)
        })
        .collect()
}

pub fn handle_res_doc_symbol(
    state: &mut GlobalState,
    req_context: &mut ReqContext,
    res: Option<DocumentSymbolResponse>,
) -> Option<Option<DocumentSymbolResponse>> {
    let req_state = match req_context.take_value::<DocSymbolState>() {
        None => return Some(res),
        Some(t) => t,
    };

    match res? {
        DocumentSymbolResponse::Flat(symbols) => {
            let filtered = filter_symbol_informations(state, &req_state, symbols);
            let mut result = req_state.result.borrow_mut();
            if let Some(result_symbols) = result.as_mut() {
                if let DocumentSymbolResponse::Flat(r) = result_symbols {
                    r.extend(filtered);
                } else {
                    warn!(
                        "DocumentSymbolResponse: Responses with mixed flat and nested symbol format."
                    );
                }
            } else {
                let _ = result.insert(DocumentSymbolResponse::Flat(filtered));
            }
        }
        DocumentSymbolResponse::Nested(symbols) => {
            let filtered = filter_document_symbols(state, &req_state, symbols);
            let mut result = req_state.result.borrow_mut();
            if let Some(result_symbols) = result.as_mut() {
                if let DocumentSymbolResponse::Nested(r) = result_symbols {
                    r.extend(filtered);
                } else {
                    warn!(
                        "DocumentSymbolResponse: Responses with mixed flat and nested symbol format."
                    );
                }
            } else {
                let _ = result.insert(DocumentSymbolResponse::Nested(filtered));
            }
        }
    };

    Rc::try_unwrap(req_state.result).ok().map(RefCell::into_inner)
}
