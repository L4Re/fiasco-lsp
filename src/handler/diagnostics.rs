use std::collections::HashMap;

use lsp_types::{PublishDiagnosticsParams, Url};

use crate::global_state::GlobalState;
use crate::source_mapping::MapDirection::FromPreprocess;

pub fn handle_publish_diagnostics(
    state: &mut GlobalState,
    params: PublishDiagnosticsParams,
) -> Vec<PublishDiagnosticsParams> {
    if params.uri.scheme() != "file" {
        info!("PublishDiagnostics: Encountered unsupported scheme {}.", params.uri);
        return vec![params];
    }

    let files = state.source_mapping.map_files(FromPreprocess, params.uri.path());
    if files.is_empty() {
        warn!("PublishDiagnostics: Encountered unknown file {}.", params.uri.path());
        return vec![params];
    }

    let mut result = HashMap::new();
    for mut diagnostic in params.diagnostics {
        let mut path = params.uri.path().to_owned();
        if diagnostic.range.start == diagnostic.range.end {
            // Diagnostic for the entire file
            for file in files {
                result
                    .entry(file.to_str().unwrap().to_owned())
                    .or_insert(Vec::new())
                    .push(diagnostic.clone());
            }
        } else if state
            .source_mapping
            .map_range(FromPreprocess, &mut path, &mut diagnostic.range)
            .is_ok()
        {
            result.entry(path).or_insert(Vec::new()).push(diagnostic);
        }
    }

    result
        .into_iter()
        .map(|(file, diagnostics)| PublishDiagnosticsParams {
            uri: Url::from_file_path(file).unwrap(),
            diagnostics,
            // TODO: We need custom version numbering...
            version: params.version,
        })
        .collect()
}
