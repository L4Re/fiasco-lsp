# Fiasco LSP Proxy
The Fiasco Language Server Protocol proxy transparently translates between an
editor (sees un-preprocessed Fiasco source) and a language server (sees
preprocessed Fiasco), thereby making it possible to use a language server such
as clangd, when reading/(writing) in your favorite LSP-enabled editor.

An inherent limitation, but depending on the situation can also be an advantage, of
this LSP proxy approach is that you are always limited to a single Fiasco
configuration. This means, for example, implementations or references to a
method in other architectures are not displayed.

## Prerequisites
* [Rust](https://www.rust-lang.org/) development environment with cargo
* [clangd](https://clangd.llvm.org/)

## Build
The Fiasco LSP Proxy is a Rust application built with cargo, so nothing special
there.

## Usage
You have to either tell the LSP proxy a path to a Fiasco build directory using
the `--build-dir` option. Or alternatively a path to the Fiasco source dir via
`--fiasco-dir`, a Fiasco `globalconfig.out` via `--fiasco-config` and optionally
a `Makeconf.local` via `--makeconf`.

Then, depending on your editor's preferences, it can communicate with the LSP
proxy via:
- stdin/stdout (default)
- `--connect <port>`: Connect to LSP-enabled editor on port
- `--listen <port>`: Listen for LSP-enabled editor on port

## What Works
- Navigation (e.g. goto, find references, ...)
- Code diagnostics
- Inlay hints
- Code actions suggested by the language server can be executed (experimental)
- Source code changes that do not change the number of lines in the file, thus
  do not require an update of the line mapping, are possible (experimental)

## Next Steps
- Synchronization of source file changes made in text editor, with the language
  server and the internal mapping of the LSP proxy. The biggest hurdle here is
  certainly the correct synchronization of the internal mapping used by the LSP
  proxy, as it requires a re-execution of preprocess.
- Implement support for more LSP requests/responses.
- Build system integration
- Support multiple configurations at same time?
