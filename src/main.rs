#![allow(dead_code)]

#[macro_use]
extern crate log;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use clap::{ArgGroup, Parser};
use color_eyre::eyre::Result;
use crossbeam_channel::select;
use lsp_server::{Connection, Message};
use lsp_types::request::{Initialize, Request};
use lsp_types::{ClientCapabilities, InitializeParams};

mod build_env;
mod dispatch;
mod global_state;
mod handler;
mod language_server_transport;
mod source_mapping;
mod thread_worker;
mod websocket_logger;
#[macro_use]
mod util;

use crate::build_env::BuildEnv;
use crate::dispatch::{NotificationDispatcher, RequestDispatcher, ResponseDispatcher};
use crate::global_state::{
    Direction::{FromServer, ToServer},
    GlobalState, ReqContext,
};
use crate::handler::*;
use crate::source_mapping::MapDirection::ToPreprocess;
use crate::websocket_logger::Logger;

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
#[clap(group(ArgGroup::new("input").required(true).args(&["build_dir", "fiasco_dir"])))]
struct Cli {
    #[clap(long)]
    build_dir: Option<PathBuf>,
    #[clap(long, requires = "fiasco_config")]
    fiasco_dir: Option<PathBuf>,
    // TODO: Maybe allow also with build_dir?
    #[clap(long, requires = "fiasco_dir")]
    fiasco_config: Option<PathBuf>,
    #[clap(long, requires = "fiasco_config")]
    makeconf: Option<PathBuf>,
    /// Connect to LSP editor on port.
    #[clap(long)]
    connect: Option<u16>,
    /// Listen for LSP editor on port.
    #[clap(long)]
    listen: Option<u16>,
    // TODO: Make websocket logger configurable.
    // TODO: Log client requests/answers to logger?!
    // websocket_logger: Option<u16>,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    env_logger::init();

    let cli: Cli = Cli::parse();

    // Note that  we must have our logging only write out to stderr.
    info!("Fiasco LSP Proxy");

    let logger = Logger::spawn();

    info!("Initialize build directory");
    let build_env = match cli.build_dir {
        Some(dir) => BuildEnv::from_dir(&dir),
        None => BuildEnv::from_config(
            &cli.fiasco_dir.unwrap(),
            &cli.fiasco_config.unwrap(),
            cli.makeconf.as_deref(),
        ),
    };
    debug!("Build directory: {}", build_env.build_dir.to_string_lossy());

    info!("Generate compilation database");
    build_env.gen_compile_commands();

    // Create the transport. Includes the stdio (stdin and stdout) versions but this could
    // also be implemented to use sockets or HTTP.
    let (connection, io_threads) = if let Some(port) = cli.connect {
        info!("Client connection via socket on localhost:{port}");
        let server_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        Connection::connect(server_addr)?
    } else if let Some(port) = cli.listen {
        info!("Client connection via socket on localhost:{port}");
        let listen_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        Connection::listen(listen_addr)?
    } else {
        info!("Client connection via stdin/stdout");
        Connection::stdio()
    };

    // Run the server and wait for the two threads to end (typically by trigger LSP Exit event).
    let (req_id, client_params) = connection.initialize_start()?;
    let client_capabilities: ClientCapabilities = serde_json::from_value(client_params.clone())?;
    debug!("Client capabilities: {:#?}", client_capabilities);
    // TODO: Forward options to lsp...
    let server = language_server_transport::start(
        "clangd",
        &["--compile-commands-dir", build_env.build_dir.to_str().unwrap()],
    )?;
    let initialize_request = lsp_server::Request {
        id: req_id,
        method: Initialize::METHOD.to_string(),
        params: client_params,
    };

    server.to_lang_server.sender().send(Message::Request(initialize_request))?;
    if let Message::Response(response) = server.from_lang_server.receiver().recv()? {
        let initialization_params = response.result.unwrap();
        debug!("Server capabilities: {:#?}", client_capabilities);
        connection.initialize_finish(response.id, initialization_params.clone())?;
        let state = GlobalState::new(
            connection,
            server,
            logger,
            source_mapping::load_source_mapping(&build_env.build_dir),
        );
        main_loop(state, initialization_params)?;
        io_threads.join()?;

        // Shut down gracefully.
        info!("shutting down server");
        Ok(())
    } else {
        panic!("Received invalid initialize response from server!")
    }
}

fn main_loop(mut state: GlobalState, params: serde_json::Value) -> Result<()> {
    let _params: InitializeParams = serde_json::from_value(params).unwrap();
    info!("starting example main loop");

    loop {
        select! {
            recv(state.client.receiver) -> r => {
                let msg = r.expect("Lost connection to client!");
                match msg.clone() {
                    Message::Request(req) => {
                        if state.client.handle_shutdown(&req)? {
                            return Ok(());
                        }

                        state.handle_client_request(req)
                    }
                    Message::Response(res) => {
                        state.handle_client_response(res)
                    }
                    Message::Notification(not) => {
                        state.handle_client_notification(not)
                    }
                }
            },
            recv(state.server.from_lang_server.receiver()) -> r => {
                let msg = r.expect("Lost connection to server!");
                state.log_from_server(&msg)?;
                match msg.clone() {
                    Message::Request(req) => {
                        state.handle_server_request(req)
                    }
                    Message::Response(res) => {
                        state.handle_server_response(res)
                    }
                    Message::Notification(not) => {
                        state.handle_server_notification(not)
                    }
                }
            },
        }
    }
}

impl GlobalState {
    fn handle_client_notification(&mut self, not: lsp_server::Notification) {
        use lsp_types::notification::*;
        NotificationDispatcher { direction: ToServer, not: Some(not), state: self }
            // TODO: Do we need to update something in our state? Maybe mark the request as cancelled? Server nevertheless must send a reply!
            .forward::<Cancel>()
            // TODO: Adjust our verbosity?
            .forward::<SetTrace>()
            // TODO: Return some log?
            .forward::<LogTrace>()
            .forward::<Initialized>()
            // TODO: Implement?
            .forward::<Exit>()
            .forward::<WorkDoneProgressCancel>()
            .on_many::<DidOpenTextDocument>(document_sync::handle_did_open_text_document)
            .on_many::<DidChangeTextDocument>(document_sync::handle_did_change_text_document)
            // TODO: Map TextDocumentIdentifier
            .forward::<WillSaveTextDocument>()
            // TODO: Map TextDocumentIdentifier and text context
            .forward::<DidSaveTextDocument>()
            .on_many::<DidCloseTextDocument>(document_sync::handle_did_close_text_document)
            .forward::<DidChangeConfiguration>()
            // TODO: Translate FileEvent
            .forward::<DidChangeWatchedFiles>()
            // TODO: Find out what needs to be done.
            .forward::<DidChangeWorkspaceFolders>()
            // TODO: Files must be mapped
            .forward::<DidCreateFiles>()
            // TODO: Files must be mapped
            .forward::<DidRenameFiles>()
            // TODO: Files must be mapped
            .forward::<DidDeleteFiles>()
            .finish()
    }

    fn handle_server_notification(&mut self, not: lsp_server::Notification) {
        use lsp_types::notification::*;
        NotificationDispatcher { direction: FromServer, not: Some(not), state: self }
            .forward::<ShowMessage>()
            .forward::<LogMessage>()
            .forward::<TelemetryEvent>()
            .on_many::<PublishDiagnostics>(diagnostics::handle_publish_diagnostics)
            .forward::<Progress>()
            .finish()
    }

    fn handle_client_request(&mut self, req: lsp_server::Request) {
        use lsp_types::request::*;
        RequestDispatcher { direction: ToServer, req: Some(req), state: self }
            .forward::<Initialize>()
            .forward::<Shutdown>()
            .forward::<RegisterCapability>()
            .forward::<UnregisterCapability>()
            .forward::<WorkspaceSymbolRequest>()
            // TODO: Location / WorkspaceLocation must be mapped
            .forward::<WorkspaceSymbolResolve>()
            .forward::<ExecuteCommand>()
            // TODO: Might map to multiple files...
            .forward::<WillSaveWaitUntil>()
            .on::<Completion>(handle_source_location!(text_document_position))
            // TODO: TextEdit must be translated
            .forward::<ResolveCompletionItem>()
            .on::<HoverRequest>(handle_source_location!(text_document_position_params))
            .on::<SignatureHelpRequest>(handle_source_location!(text_document_position_params))
            .on::<GotoDeclaration>(handle_source_location!(text_document_position_params))
            .on::<GotoDefinition>(handle_source_location!(text_document_position_params))
            .on::<References>(handle_source_location!(text_document_position))
            // TODO: Might need many here, in case one location is mapped to multiple files (function decl e.g.)
            .on::<DocumentHighlightRequest>(handle_source_location!(text_document_position_params))
            .on_many::<DocumentSymbolRequest>(document_symbol::handle_req_doc_symbol)
            .on::<CodeActionRequest>(code_action::handle_req_code_action)
            // TODO: TextDocumentIdentifier must be mapped
            .forward::<CodeLensRequest>()
            // TODO: Range must be mapped, maybe use the data value as identifier?!
            .forward::<CodeLensResolve>()
            // TODO: TextDocumentIdentifier must be mapped
            .forward::<DocumentLinkRequest>()
            // TODO: DocumentLink must be mapped
            .forward::<DocumentLinkResolve>()
            // TODO: TextDocumentIdentifier and Range must be mapped
            .forward::<RangeFormatting>()
            .on::<OnTypeFormatting>(handle_source_location!(text_document_position))
            // TODO: TextDocumentIdentifier must be mapped
            .forward::<Formatting>()
            .on::<Rename>(handle_source_location!(text_document_position))
            // TODO: TextDocumentIdentifier must be mapped
            .forward::<DocumentColor>()
            // TODO: TextDocumentIdentifier and Range must be mapped
            .forward::<ColorPresentationRequest>()
            // TODO: TextDocumentIdentifier must be mapped
            .forward::<FoldingRangeRequest>()
            // TODO: TextDocumentIdentifier and Position must be mapped
            .forward::<PrepareRenameRequest>()
            // TODO: Unify all users of GotoDefinition
            .on::<GotoImplementation>(handle_source_location!(text_document_position_params))
            .on::<GotoTypeDefinition>(handle_source_location!(text_document_position_params))
            // TODO: TextDocumentIdentifier and Position must be mapped
            .forward::<SelectionRangeRequest>()
            // TODO: Url and Range and SelectionRange need to be mapped
            .forward::<CallHierarchyIncomingCalls>()
            // TODO: Url and Range and SelectionRange need to be mapped
            .forward::<CallHierarchyOutgoingCalls>()
            .on::<MonikerRequest>(handle_source_location!(text_document_position_params))
            .on::<LinkedEditingRange>(handle_source_location!(text_document_position_params))
            .on::<CallHierarchyPrepare>(handle_source_location!(text_document_position_params))
            // TODO: TextDocumentIdentifier must be mapped
            .forward::<SemanticTokensFullRequest>()
            // TODO: TextDocumentIdentifier and prev_req_id must be mapped
            .forward::<SemanticTokensFullDeltaRequest>()
            // TODO: TextDocumentIdentifier and Range must be mapped
            .forward::<SemanticTokensRangeRequest>()
            // TODO: Files must be mapped
            .forward::<WillCreateFiles>()
            // TODO: Files must be mapped
            .forward::<WillRenameFiles>()
            // TODO: Files must be mapped
            .forward::<WillDeleteFiles>()
            // TODO: Diagnostic and more must be mapped
            .forward::<CodeActionResolveRequest>()
            // TODO: TextDocumentIdentifier and Range must be resolved (can span entire file, so we might have to map this to multiple requests...).
            .on_many::<InlayHintRequest>(inlay_hint::handle_req_inlay_hint)
            // TODO: Position and Locationmust be resolved.
            .forward::<InlayHintResolveRequest>()
            // TODO: TextDocumentIdentifier and Range must be resolved.
            .forward::<InlineValueRequest>()
            // TODO: TextDocumentIdentifier must be mapped.
            .forward::<DocumentDiagnosticRequest>()
            // TODO: PreviousResultId must be mapped.
            .forward::<WorkspaceDiagnosticRequest>()
            .on::<TypeHierarchyPrepare>(handle_source_location!(text_document_position_params))
            // TODO: URL and Range must be resolved.
            .forward::<TypeHierarchySupertypes>()
            // TODO: URL and Range must be resolved.
            .forward::<TypeHierarchySubtypes>()
            .finish()
    }

    fn handle_server_request(&mut self, req: lsp_server::Request) {
        use lsp_types::request::*;
        RequestDispatcher { direction: FromServer, req: Some(req), state: self }
            .forward::<ShowMessageRequest>()
            // TODO: WorkspaceEdit must be mapped
            .forward::<ApplyWorkspaceEdit>()
            .forward::<WorkspaceFoldersRequest>()
            // TODO: Find out what needs to be done.
            .forward::<WorkspaceConfiguration>()
            .forward::<WorkDoneProgressCreate>()
            .forward::<SemanticTokensRefresh>()
            .forward::<CodeLensRefresh>()
            // TODO: TextDocument must be mapped?
            .forward::<ShowDocument>()
            .forward::<InlayHintRefreshRequest>()
            .forward::<InlineValueRefreshRequest>()
            .forward::<WorkspaceDiagnosticRefresh>()
            .finish()
    }

    fn handle_client_response(&mut self, res: lsp_server::Response) {
        use lsp_types::request::*;
        ResponseDispatcher::new(ToServer, res, self)
            .forward::<ShowMessageRequest>()
            .forward::<ApplyWorkspaceEdit>()
            // WorkspaceFolder must be added/removed/mapped
            .forward::<WorkspaceFoldersRequest>()
            // TODO: Find out what needs to be done.
            .forward::<WorkspaceConfiguration>()
            .forward::<WorkDoneProgressCreate>()
            .forward::<SemanticTokensRefresh>()
            .forward::<CodeLensRefresh>()
            .forward::<ShowDocument>()
            .forward::<InlayHintRefreshRequest>()
            .forward::<InlineValueRefreshRequest>()
            .forward::<WorkspaceDiagnosticRefresh>()
            .finish()
    }

    fn handle_server_response(&mut self, res: lsp_server::Response) {
        use lsp_types::request::*;
        ResponseDispatcher::new(FromServer, res, self)
            .forward::<Initialize>()
            .forward::<Shutdown>()
            .forward::<RegisterCapability>()
            .forward::<UnregisterCapability>()
            // TODO: Support workspace location (only Uri)
            .forward::<WorkspaceSymbolRequest>()
            // TODO: Support workspace location (only Uri)
            .forward::<WorkspaceSymbolResolve>()
            .forward::<ExecuteCommand>()
            // TODO: TextEdit need to be mapped
            .forward::<WillSaveWaitUntil>()
            // TODO: All the TextEduts must be mapped.
            .forward::<Completion>()
            // TODO: TextEdit need to be mapped
            .forward::<ResolveCompletionItem>()
            // TODO: Range must be translated
            .forward::<HoverRequest>()
            .forward::<SignatureHelpRequest>()
            // TODO: LocationLink must be mapped (and Location mapping is wrong, uses self.mapped_file which is wrong)
            .on::<GotoDeclaration>(goto::handle_res_goto)
            // TODO: LocationLink must be mapped (and Location mapping is wrong, uses self.mapped_file which is wrong)
            .on::<GotoDefinition>(goto::handle_res_goto)
            .on::<References>(goto::handle_res_references)
            // TODO: Range must be mapped
            .on::<DocumentHighlightRequest>(document_highlight::handle_res_document_highlight)
            .on_collect::<DocumentSymbolRequest>(document_symbol::handle_res_doc_symbol)
            .on::<CodeActionRequest>(code_action::handle_res_code_action)
            // TODO: Range must be mapped
            .forward::<CodeLensRequest>()
            // TODO: Range must be mapped
            .forward::<CodeLensResolve>()
            // TODO: DocumentLink must be mapped, we might need to filter results to include stuff for current document.
            .forward::<DocumentLinkRequest>()
            // TODO: DocumentLink must be mapped
            .forward::<DocumentLinkResolve>()
            // TODO: TextEdit must be mapped
            .forward::<RangeFormatting>()
            // TODO: TextEdit must be mapped
            .forward::<OnTypeFormatting>()
            // TODO: TextEdit must be mapped
            .forward::<Formatting>()
            // TODO: TextEdit must be mapped
            .forward::<Rename>()
            // TODO: Range must be mapped, we might need to filter results to include stuff for current document.
            .forward::<DocumentColor>()
            // TODO: TextEdit must be mapped
            .forward::<ColorPresentationRequest>()
            // TODO: FoldingRange must be mapped
            .forward::<FoldingRangeRequest>()
            // TODO: Range must be mapped
            .forward::<PrepareRenameRequest>()
            // TODO: LocationLink must be mapped (and Location mapping is wrong, uses self.mapped_file which is wrong)
            .on::<GotoImplementation>(goto::handle_res_goto)
            // TODO: LocationLink must be mapped (and Location mapping is wrong, uses self.mapped_file which is wrong)
            .on::<GotoTypeDefinition>(goto::handle_res_goto)
            // TODO: Range must be mapped
            .forward::<SelectionRangeRequest>()
            // TODO: Url and Range and SelectionRange need to be mapped
            .forward::<CallHierarchyIncomingCalls>()
            // TODO: Url and Range and SelectionRange need to be mapped
            .forward::<CallHierarchyOutgoingCalls>()
            .forward::<MonikerRequest>()
            // TODO: Range must be mapped, we might need to filter results to include stuff for current document.
            .forward::<LinkedEditingRange>()
            // TODO: Url and Range and SelectionRange need to be mapped
            .forward::<CallHierarchyPrepare>()
            // TODO: SemanticToken need to be mapped, we might need to filter results to include stuff for current document.
            .forward::<SemanticTokensFullRequest>()
            // TODO: SemanticToken need to be mapped, we might need to filter results to include stuff for current document.
            .forward::<SemanticTokensFullDeltaRequest>()
            // TODO: SemanticToken need to be mapped, we might need to filter results to include stuff for current document.
            .forward::<SemanticTokensRangeRequest>()
            // TODO: WorkspaceEdit must be mapped
            .forward::<WillCreateFiles>()
            // TODO: WorkspaceEdit must be mapped
            .forward::<WillRenameFiles>()
            // TODO: WorkspaceEdit must be mapped
            .forward::<WillDeleteFiles>()
            // TODO: Diagnostic and more must be mapped
            .forward::<CodeActionResolveRequest>()
            // TODO: Position and Location must be resolved (might need to filter to include stuff for current document).
            .on_collect::<InlayHintRequest>(inlay_hint::handle_res_inlay_hint)
            // TODO: Position and Location must be resolved.
            .forward::<InlayHintResolveRequest>()
            // TODO: Range must be resolved.
            .forward::<InlineValueRequest>()
            // TODO: All the reports must be resolved.
            .forward::<DocumentDiagnosticRequest>()
            // TODO: All the reports must be resolved.
            .forward::<WorkspaceDiagnosticRequest>()
            // TODO: URL and Range must be resolved.
            .forward::<TypeHierarchyPrepare>()
            // TODO: URL and Range must be resolved.
            .forward::<TypeHierarchySupertypes>()
            // TODO: URL and Range must be resolved.
            .forward::<TypeHierarchySubtypes>()
            .finish()
    }
}
