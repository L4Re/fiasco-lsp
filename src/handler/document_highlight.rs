use lsp_types::DocumentHighlight;

use crate::global_state::{GlobalState, ReqContext};
use crate::source_mapping::MapDirection::FromPreprocess;

pub fn handle_res_document_highlight(
    state: &mut GlobalState,
    req_context: &mut ReqContext,
    res: Option<Vec<DocumentHighlight>>,
) -> Option<Vec<DocumentHighlight>> {
    let (source_path, mapped_file) = match req_context.take_value::<(String, String)>() {
        None => return res,
        Some(t) => t,
    };
    let mut result = res?;
    result.retain_mut(|highlight| {
        let mut highlight_path = mapped_file.clone();
        if state
            .source_mapping
            .map_range(FromPreprocess, &mut highlight_path, &mut highlight.range)
            .is_err()
        {
            warn!("DocumentHighlightRequest: Encountered unmappable range {:?}.", &highlight.range);
            return false;
        }

        let in_same_doc = highlight_path == source_path;
        if !in_same_doc {
            warn!(
                "CodeAction: Highlight mapped to different file ({}) than source file specified in request ({}).",
                highlight_path, source_path
            );
        }
        in_same_doc
    });
    Some(result)
}
