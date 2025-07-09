use lsp_types::{GotoDefinitionResponse, Location};

use crate::global_state::{GlobalState, ReqContext};
use crate::source_mapping::MapDirection::FromPreprocess;

pub fn handle_res_goto(
    state: &mut GlobalState,
    req_context: &mut ReqContext,
    res: Option<GotoDefinitionResponse>,
) -> Option<GotoDefinitionResponse> {
    let (source_path, mapped_file) = match req_context.take_value::<(String, String)>() {
        None => return res,
        Some(t) => t,
    };
    let mut result = res?;
    match &mut result {
        GotoDefinitionResponse::Scalar(location) => {
            state.source_mapping.map_location(FromPreprocess, location);
        }
        GotoDefinitionResponse::Array(vec) => vec.retain_mut(|location| {
            state.source_mapping.map_location(FromPreprocess, location).is_ok()
        }),
        GotoDefinitionResponse::Link(vec) => vec.retain_mut(|location| {
            let mut path = mapped_file.clone();
            if let Some(origin_selection_range) = location.origin_selection_range.as_mut() {
                state.source_mapping.map_range(FromPreprocess, &mut path, origin_selection_range);
                if source_path != path {
                    warn!(
                        "GotoRequest: Origin selection mapped to different file ({}) than source file specified in request ({}).",
                        &path, &source_path
                    );
                }
            }
            let mut mapped_uri = location.target_uri.clone();
            if state
                .source_mapping
                .map_range_uri(FromPreprocess, &mut mapped_uri, &mut location.target_range)
                .is_err()
            {
                warn!(
                    "GotoRequest: Encountered unmappable target_range {:?}.",
                    &location.target_range
                );
                return false;
            }

            if state
                .source_mapping
                .map_range_uri(
                    FromPreprocess,
                    &mut location.target_uri,
                    &mut location.target_selection_range,
                )
                .is_err()
            {
                warn!(
                    "GotoRequest: Encountered unmappable target_selection_range {:?}.",
                    &location.target_range
                );
                return false;
            }

            if mapped_uri != location.target_uri {
                warn!(
                    "GotoRequest: target_range {:?} mapped to different file than target_selection_range {:?}.",
                    &mapped_uri, &location.target_range
                );
                return false;
            }

            true
        }),
    }
    Some(result)
}

pub fn handle_res_references(
    state: &mut GlobalState,
    _req_context: &mut ReqContext,
    res: Option<Vec<Location>>,
) -> Option<Vec<Location>> {
    let mut result = res?;
    result
        .retain_mut(|location| state.source_mapping.map_location(FromPreprocess, location).is_ok());
    Some(result)
}
