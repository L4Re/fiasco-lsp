#[macro_export]
macro_rules! handle_source_location {
    ($path:ident) => {
        |state: &mut GlobalState, req_context: &mut ReqContext, mut params| {
            let param = &mut params.$path;
            if param.text_document.uri.scheme() != "file" {
                return params;
            }
            let source_file = param.text_document.uri.path().to_owned();
            state.source_mapping.map_position_uri(
                ToPreprocess,
                &mut param.text_document.uri,
                &mut param.position,
            );
            // TODO: Only for case in that result only contains range...
            req_context.set_value((source_file, param.text_document.uri.path().to_owned()));
            params
        }
    };
}

#[macro_export]
macro_rules! handle_reverse_source_location {
    ($path:ident) => {
        |state: &mut GlobalState, _: &mut ReqContext, mut params| {
            let location = &mut params.$path;
            if location.uri.scheme() == "file" {
                // TODO: Handle errors?
                state.source_mapping.map_range_uri(
                    FromPreprocess,
                    &mut location.uri,
                    &mut location.range,
                );
            }

            params
        }
    };
}
