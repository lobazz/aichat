# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```sh
# Development build
cargo build

# Release build
cargo build --release

# Run tests
cargo test
```

## Architecture Overview

AIChat is an all-in-one LLM CLI tool with a modular architecture.

### Core Modules

- **main.rs**: Entry point. Determines `WorkingMode` (Serve/Repl/Cmd) and orchestrates execution
- **cli.rs**: CLI argument parsing using `clap`
- **config/**: Configuration management
  - `mod.rs`: Central `Config` struct with `GlobalConfig = Arc<RwLock<Config>>`
  - `input.rs`: `Input` struct wraps user prompts with metadata (role, files, etc)
  - `role.rs`, `session.rs`, `agent.rs`, `rag.rs`: Feature-specific state
- **client/**: LLM provider integrations
  - `mod.rs`: Client traits and factory functions
  - `model.rs`: `Model` struct with provider-specific handling
  - Provider clients: `openai.rs`, `claude.rs`, `gemini.rs`, etc.
  - `common.rs`: `RequestData`, `ChatCompletionsData`, request patching via `patch` config
- **repl/**: Interactive REPL implementation using `reedline`
- **serve.rs**: HTTP server for API (OpenAI-compatible) and web UI (Playground/Arena)
- **rag/**: Retrieval-Augmented Generation with embeddings
- **function.rs**: Function calling/tool system

### Key Patterns

- **Config access**: Use `config.read()` for reads, `config.write()` for writes on `GlobalConfig`
- **Request patching**: Models support `patch.url`, `patch.body`, `patch.headers` in config
- **Client trait**: `Client` trait defines `prepare_chat_completions`, `chat_completions`, etc.
- **StateFlags**: Bitflags for REPL command availability (ROLE, SESSION, RAG, AGENT, VS)

### Working Modes

1. **Cmd**: Single prompt execution (`aichat "hello"`)
2. **Repl**: Interactive chat with history, completions, commands (`.help`, `.session`, etc.)
3. **Serve**: HTTP server mode (`--serve`)

### REPL Commands

Commands like `.model`, `.role`, `.session`, `.rag`, `.agent` toggle state flags. See `REPL_COMMANDS` array in `repl/mod.rs` for all commands and their `AssertState` requirements.

## Configuration

Config file: `~/.config/aichat/config.yaml`

```yaml
model: openai:gpt-4o
clients:
  - type: openai
    api_key: ${OPENAI_API_KEY}
    # Supports patch for URL, body, headers
```

Environment variables: `*_api_key`, `AICHAT_LOG_LEVEL`, `AICHAT_CONFIG_FILE`
