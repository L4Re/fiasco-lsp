use std::cell::RefCell;
use std::rc::Rc;

use lsp_types::{InlayHint, InlayHintParams, Range, Url};

use crate::global_state::{GlobalState, ReqContext, ReqContextAlloc};
use crate::source_mapping::MapDirection::{FromPreprocess, ToPreprocess};

struct InlayState {
    source_path: String,
    mapped_path: String,
    range: Range,
    result: Rc<RefCell<Vec<InlayHint>>>,
}

// TODO: Maybe add generic abstraction for File+Range -> Many files -> LSP -> One file / Filter File+Range

pub fn handle_req_inlay_hint(
    state: &mut GlobalState,
    req_context_alloc: &ReqContextAlloc,
    params: InlayHintParams,
) -> Vec<(InlayHintParams, ReqContext)> {
    let doc = &params.text_document;
    if doc.uri.scheme() != "file" {
        info!("InlayHintRequest: Encountered unsupported scheme {}.", doc.uri);
        return vec![(params, req_context_alloc.alloc())];
    }

    let source_path = doc.uri.path().to_owned();
    if state.source_mapping.map_files(ToPreprocess, &source_path).is_empty() {
        warn!("InlayHintRequest: Encountered unknown file {}.", source_path);
        return vec![(params, req_context_alloc.alloc())];
    }

    // TODO: Support partial request?
    let files = state.source_mapping.map_file_range_uri(ToPreprocess, &doc.uri, &params.range);
    if files.is_empty() {
        warn!("InlayHintRequest: Encountered unmappable range {:?}.", &params.range);
        return vec![(params, req_context_alloc.alloc())];
    }

    let result_vec = Rc::new(RefCell::new(Vec::new()));
    let mut result = Vec::new();

    // Split up into one request per file...
    for mapped_path in files {
        // Save translated file path for response.
        let mut req_context = req_context_alloc.alloc();
        req_context.set_value(InlayState {
            source_path: source_path.clone(),
            // TODO: Store Path here?
            mapped_path: mapped_path.to_str().unwrap().to_owned(),
            range: params.range,
            result: result_vec.clone(),
        });

        let mut req_params = params.clone();
        // Update file.
        req_params.text_document.uri = Url::from_file_path(mapped_path).unwrap();

        // TODO: Figure out the range...
        req_params.range.start.line = 0;
        req_params.range.start.character = 0;
        req_params.range.end.line =
            state.source_mapping.file_length(FromPreprocess, mapped_path).unwrap();
        req_params.range.end.character = 0;

        result.push((req_params, req_context));
    }

    result
}

pub fn handle_res_inlay_hint(
    state: &mut GlobalState,
    req_context: &mut ReqContext,
    res: Option<Vec<InlayHint>>,
) -> Option<Option<Vec<InlayHint>>> {
    let req_state = match req_context.take_value::<InlayState>() {
        None => return Some(res),
        Some(t) => t,
    };

    req_state.result.borrow_mut().extend(res?.into_iter().filter_map(|mut inlay_hint| {
        let mut inlay_hint_path = req_state.mapped_path.clone();
        state.source_mapping.map_position(
            FromPreprocess,
            &mut inlay_hint_path,
            &mut inlay_hint.position,
        );

        if inlay_hint_path == req_state.source_path {
            Some(inlay_hint)
        } else {
            warn!(
                "InlayHint: Inlay hint mapped to different file ({}) than source file specified in request ({}).",
                inlay_hint_path, req_state.source_path
            );
            None
        }
    }));

    Rc::try_unwrap(req_state.result).ok().map(RefCell::into_inner).map(Some)
}
